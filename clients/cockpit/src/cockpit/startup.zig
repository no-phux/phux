//! Shared Cockpit startup: config, state restoration, and provider selection.
//! The shipping TypeScript native extension enters through this module; tests
//! call the resolved-input variants so startup behavior has one implementation.

const std = @import("std");
const native_sdk = @import("native_sdk");
const grid = @import("../terminal/grid.zig");
const support = @import("phux_support.zig");
const topology = @import("topology.zig");
const model_module = @import("model.zig");
const session_state = @import("session_state.zig");
const config_module = @import("../config/config.zig");
const ghostty = @import("../config/ghostty.zig");
const scene = @import("native/scene.zig");
const remote_memory = @import("remote_memory.zig");

pub const Config = config_module.Config;
pub const parseConfig = config_module.parse;
const PhuxProvider = support.PhuxProvider;
const phux_enabled = support.phux_enabled;
const Model = model_module.Model;
const TopologySnapshot = topology.TopologySnapshot;
const PersistedTopologySnapshot = topology.PersistedTopologySnapshot;
const TabPlacement = topology.TabPlacement;
const migrateTopologySnapshot = topology.migrateTopologySnapshot;
const initialModelWithIo = model_module.initialModelWithIo;
const attachPhuxProvider = model_module.attachPhuxProvider;
const app_name = scene.app_name;

pub fn tabPlacementFromText(value: []const u8) ?TabPlacement {
    if (std.ascii.eqlIgnoreCase(value, "top")) return .top;
    if (std.ascii.eqlIgnoreCase(value, "side") or std.ascii.eqlIgnoreCase(value, "sidebar")) return .side;
    return null;
}

/// Ambient values that can select a local Phux coordinator at startup. Kept
/// as slices here and copied into `Config` by `resolvePhuxConfig`.
pub const PhuxEnvironment = struct {
    socket: ?[]const u8 = null,
    session: ?[]const u8 = null,
    runtime_dir: ?[]const u8 = null,
    uid: ?[]const u8 = null,
    user: ?[]const u8 = null,
    /// `PHUX_REMOTE`: a registered remote host, as `phux-remote` in the config.
    remote: ?[]const u8 = null,
};

fn nonEmpty(value: ?[]const u8) ?[]const u8 {
    const candidate = value orelse return null;
    return if (candidate.len == 0) null else candidate;
}

fn runtimePhuxSocket(runtime_dir: []const u8, output: []u8) ?[]const u8 {
    if (runtime_dir.len == 0) return null;
    const candidate = std.fmt.bufPrint(output, "{s}/phux/phux.sock", .{runtime_dir}) catch return null;
    if (!config_module.validPhuxSocket(candidate)) return null;
    return candidate;
}

fn temporaryPhuxSocket(identity: []const u8, output: []u8) ?[]const u8 {
    const candidate = std.fmt.bufPrint(output, "/tmp/phux-{s}/phux.sock", .{identity}) catch return null;
    if (!config_module.validPhuxSocket(candidate)) return null;
    return candidate;
}

fn defaultPhuxSocket(env: PhuxEnvironment, output: []u8) []const u8 {
    if (runtimePhuxSocket(nonEmpty(env.runtime_dir) orelse "", output)) |path| return path;
    const identity = nonEmpty(env.uid) orelse nonEmpty(env.user) orelse "default";
    return temporaryPhuxSocket(identity, output) orelse "/tmp/phux-default/phux.sock";
}

/// Apply startup precedence without borrowing any environment or stack bytes:
///
///   non-empty, valid PHUX_* > config file > local default.
///
/// Empty environment values are unset by convention. In particular they do
/// not turn a socket into `/` or a session into a fabricated `default` name.
pub fn resolvePhuxConfig(parsed: Config, env: PhuxEnvironment) Config {
    var resolved = parsed;
    if (nonEmpty(env.socket)) |socket| _ = resolved.setPhuxSocket(socket, .environment);
    if (resolved.phux_socket.slice().len == 0) {
        var storage: [config_module.max_phux_socket_bytes]u8 = undefined;
        _ = resolved.setPhuxSocket(defaultPhuxSocket(env, &storage), .default);
    }
    if (nonEmpty(env.session)) |session| _ = resolved.setPhuxSession(session, .environment);
    if (nonEmpty(env.remote)) |remote| _ = resolved.setPhuxRemote(remote, .environment);
    return resolved;
}

/// Restore the remembered Connect to Host choice (the one client-side fact
/// a relaunch needs), unless config or `PHUX_REMOTE` names a host.
pub fn restoreRememberedRemote(io: std.Io, state_path: ?[]const u8, config: *Config) void {
    const path = remote_memory.setPathFor(state_path) orelse return;
    if (config.phux_remote.slice().len != 0) return;
    var buffer: [config_module.max_phux_remote_bytes]u8 = undefined;
    const remembered = remote_memory.load(io, path, &buffer) orelse return;
    _ = config.setPhuxRemote(remembered, .default);
}

/// The provider-construction seam. `PhuxProvider.create` duplicates both
/// slices, so neither the resolved Config copied into the model nor this
/// caller's stack is part of the worker's lifetime.
pub fn createPhuxProviderFromConfig(
    gpa: std.mem.Allocator,
    io: std.Io,
    config: *const Config,
) !?*PhuxProvider {
    if (comptime !phux_enabled) return null;
    const socket = config.phux_socket.slice();
    if (!config_module.validPhuxSocket(socket)) return error.InvalidPhuxSocket;
    const session_name = config.phux_session.slice();
    if (!config_module.validPhuxSession(session_name)) return error.InvalidPhuxSession;
    const session: ?[]const u8 = if (session_name.len == 0) null else session_name;
    // A registered remote host replaces the local socket; raw TCP is still
    // never admitted by this composition. The remote endpoint is a registry
    // label that phux-client-ffi resolves, pins, and authenticates.
    const remote_target = config.phux_remote.slice();
    // A remembered host (`.default` provenance) is reattached beside this
    // Mac, as the peer; only a configured or environment host is active.
    if (remote_target.len != 0 and config.phux_remote_source != .default) {
        if (!config_module.validPhuxRemote(remote_target)) return error.InvalidPhuxRemote;
        return try createRemotePhuxProvider(gpa, io, remote_target, session);
    }
    return try PhuxProvider.create(gpa, io, .{ .unix = socket }, session, "phux-cockpit");
}

/// The coordinator held beside the active one at launch, so the switcher
/// lists both (docs/REMOTE_HOSTS.md, "Side by side"): this Mac's while a
/// configured or environment host is active, the remembered host while this
/// Mac is, and none when no remote host is involved. The remembered host
/// honors its registry entry's pinned session, as the active one would.
pub fn createPhuxPeerFromConfig(
    gpa: std.mem.Allocator,
    io: std.Io,
    config: *const Config,
) !?*PhuxProvider {
    if (comptime !phux_enabled) return null;
    const remote_target = config.phux_remote.slice();
    if (remote_target.len == 0) return null;
    if (!config_module.validPhuxRemote(remote_target)) return error.InvalidPhuxRemote;
    const peer = if (config.phux_remote_source == .default)
        // A remembered host that cannot be set up is skipped, never the
        // launch (as in attachRememberedPeers); it stays remembered.
        createRemotePhuxProvider(gpa, io, remote_target, null) catch return null
    else blk: {
        const socket = config.phux_socket.slice();
        if (!config_module.validPhuxSocket(socket)) return error.InvalidPhuxSocket;
        // The configured session names the active host's session, not this Mac's.
        break :blk try PhuxProvider.create(gpa, io, .{ .unix = socket }, null, "phux-cockpit");
    };
    // Lists sessions only; never attaches, so it sizes nobody's panes.
    peer.standBy();
    return peer;
}

/// Every remembered host not already held joins beside the coordinators at
/// launch, listing, in the next free peer slot (docs/REMOTE_HOSTS.md,
/// "Persistence and relaunch"). None is attached until one of its sessions is
/// shown. A host already held (the active one, or the first remembered host
/// `createPhuxPeerFromConfig` placed) is never held twice.
pub fn attachRememberedPeers(gpa: std.mem.Allocator, io: std.Io, model: *model_module.Model) !void {
    if (comptime !phux_enabled) return;
    const path = remote_memory.path() orelse return;
    var hosts: remote_memory.Hosts = .{};
    defer hosts.deinit();
    remote_memory.loadAll(io, path, &hosts);
    for (0..hosts.count) |index| {
        const target = hosts.get(index);
        const id = PhuxProvider.coordinatorId(.{ .remote = .{ .target = target } });
        // The first remembered host may already stand beside this Mac
        // (createPhuxPeerFromConfig); its record is still its own.
        if (heldPeerSlot(model, id)) |slot| {
            noteRestore(model, slot, hosts.shown[index]);
            continue;
        }
        if (coordinatorHeld(model, id)) continue;
        const slot = try model.freePeerSlot();
        // One host that cannot be built never costs the launch; it stays
        // remembered for the next one.
        const peer = createRemotePhuxProvider(gpa, io, target, null) catch continue;
        // Lists sessions only; never attaches, so it sizes nobody's panes.
        peer.standBy();
        model.peers.items[slot].provider = peer;
        noteRestore(model, slot, hosts.shown[index]);
    }
}

/// What the host in `slot` was showing at the last quit (ADR-0110), keyed by
/// that peer's own coordinator id. Only a front record is to be shown, and
/// only once its list judges it (native/peer_restore.zig); the peer stays
/// listing until then.
fn noteRestore(model: *model_module.Model, slot: usize, shown: ?remote_memory.Shown) void {
    const value = shown orelse return;
    const peer = model.peers.items[slot].provider orelse return;
    model.peers.items[slot].restore = .{ .coordinator = peer.providerId(), .shown = value, .pending = value.front };
}

fn heldPeerSlot(model: *const model_module.Model, id: anytype) ?usize {
    for (model.peers.items, 0..) |entry, slot| {
        const peer = entry.provider orelse continue;
        if (peer.effectiveProviderId() == id) return slot;
    }
    return null;
}

fn coordinatorHeld(model: *const model_module.Model, id: anytype) bool {
    if (model.phux_provider) |active| if (active.effectiveProviderId() == id) return true;
    for (model.peers.items) |entry| {
        const peer = entry.provider orelse continue;
        if (peer.effectiveProviderId() == id) return true;
    }
    return false;
}

/// Resolve a launch-selected host through the registry like Connect to Host,
/// so its pinned session applies from the first attach; an explicit session
/// still wins.
fn createRemotePhuxProvider(
    gpa: std.mem.Allocator,
    io: std.Io,
    target: []const u8,
    configured_session: ?[]const u8,
) !*PhuxProvider {
    const described = PhuxProvider.describeRemote(target);
    const resolved = described.state == .resolved;
    const pinned = described.session.slice();
    const pinned_session: ?[]const u8 = if (resolved and pinned.len != 0) pinned else null;
    const provider = try PhuxProvider.create(gpa, io, .{ .remote = .{ .target = target } }, configured_session orelse pinned_session, "phux-cockpit");
    errdefer provider.destroy();
    if (resolved) try provider.setRemoteLabel(described.name.slice());
    return provider;
}

/// Read-only construction evidence for settings/tests: the local-domain
/// location, or empty when the provider dials a registered remote host.
pub fn configuredPhuxSocket(provider: *const PhuxProvider) []const u8 {
    if (comptime !phux_enabled) return "";
    return switch (provider.endpoint) {
        .unix => |path| path,
        else => "",
    };
}

/// The registered remote host the provider was built for, or null.
pub fn configuredPhuxRemote(provider: *const PhuxProvider) ?[]const u8 {
    if (comptime !phux_enabled) return null;
    return provider.remoteTarget();
}

pub fn configuredPhuxSession(provider: *const PhuxProvider) ?[]const u8 {
    if (comptime !phux_enabled) return null;
    return provider.session;
}

fn createConfiguredPhuxProvider(init: std.process.Init, config: *const Config) !?*PhuxProvider {
    return createPhuxProviderFromConfig(std.heap.page_allocator, init.io, config);
}

/// The platform config path (`app_dirs`); `PHUX_COCKPIT_CONFIG` names a file
/// and wins. Null (no home) falls back to defaults silently.
pub fn resolveConfigPath(env: native_sdk.app_dirs.Env, override_path: ?[]const u8, dir_storage: []u8, path_storage: []u8) ?[]const u8 {
    if (override_path) |explicit| {
        if (explicit.len == 0 or explicit.len > path_storage.len) return null;
        @memcpy(path_storage[0..explicit.len], explicit);
        return path_storage[0..explicit.len];
    }
    const dir = native_sdk.app_dirs.resolveOne(
        .{ .name = app_name },
        native_sdk.app_dirs.currentPlatform(),
        env,
        .config,
        dir_storage,
    ) catch return null;
    return config_module.joinPath(dir, path_storage) catch null;
}

/// The dotfile location (`$XDG_CONFIG_HOME` or `~/.config`, then
/// `phux-cockpit/config`), tried before the platform path because users
/// arriving from Ghostty put it there.
pub fn resolveDotfileConfigPath(env: native_sdk.app_dirs.Env, path_storage: []u8) ?[]const u8 {
    var joined: [std.fs.max_path_bytes]u8 = undefined;
    const base = if (env.xdg_config_home) |xdg| blk: {
        if (xdg.len == 0) break :blk null;
        break :blk xdg;
    } else null;
    const dir = if (base) |explicit|
        config_module.joinDir(explicit, "phux-cockpit", &joined) catch return null
    else dir: {
        const home = env.home orelse return null;
        if (home.len == 0) return null;
        var home_config: [std.fs.max_path_bytes]u8 = undefined;
        const dotconfig = config_module.joinDir(home, ".config", &home_config) catch return null;
        break :dir config_module.joinDir(dotconfig, "phux-cockpit", &joined) catch return null;
    };
    return config_module.joinPath(dir, path_storage) catch null;
}

/// The loaded config plus where it came from, so the settings surface can
/// write back to the same file (`path_len` zero disables writing). Every
/// failure lands on defaults.
pub const LoadedConfig = struct {
    config: Config,
    /// Where the Ghostty layer under `config` was looked for; kept so a
    /// Settings reload re-imports from the same places.
    ghostty: ghostty.Locator = .{},
    path_storage: [std.fs.max_path_bytes]u8 = undefined,
    path_len: usize = 0,

    pub fn path(self: *const LoadedConfig) []const u8 {
        return self.path_storage[0..self.path_len];
    }

    fn setPath(self: *LoadedConfig, value: []const u8) void {
        if (value.len == 0 or value.len > self.path_storage.len) {
            self.path_len = 0;
            return;
        }
        @memcpy(self.path_storage[0..value.len], value);
        self.path_len = value.len;
    }
};

fn loadUserConfig(io: std.Io, init: std.process.Init) LoadedConfig {
    var dir_storage: [std.fs.max_path_bytes]u8 = undefined;
    var path_storage: [std.fs.max_path_bytes]u8 = undefined;
    var dotfile_storage: [std.fs.max_path_bytes]u8 = undefined;
    const env = native_sdk.debug.envFromMap(init.environ_map);
    const override = init.environ_map.get("PHUX_COCKPIT_CONFIG");

    // The user's Ghostty font and colours are the defaults every Cockpit key
    // below then overrides.
    const locator = ghostty.Locator.fromEnv(env.home, env.xdg_config_home, init.environ_map.get("PHUX_COCKPIT_GHOSTTY_CONFIG"));
    const inherited = ghostty.load(io, &locator);
    reportInherited(&inherited);
    var loaded: LoadedConfig = .{ .config = Config.seeded(inherited), .ghostty = locator };

    // An explicit override answers on its own — including for WRITING. A
    // wrapper or a test that named a file is naming the file the app should
    // edit too, whether or not it exists yet.
    if (override) |explicit| {
        loaded.setPath(explicit);
        if (readConfig(io, explicit, inherited)) |parsed| loaded.config = parsed;
        return loaded;
    }
    // Dotfile first, then the platform path; the first file that opens wins.
    if (resolveDotfileConfigPath(env, &dotfile_storage)) |dotfile| {
        if (readConfig(io, dotfile, inherited)) |parsed| {
            loaded.config = parsed;
            loaded.setPath(dotfile);
            return loaded;
        }
    }
    const path = resolveConfigPath(env, null, &dir_storage, &path_storage) orelse {
        // No file opened anywhere and no platform directory either. A write
        // still needs somewhere to go, and the dotfile path is the one this
        // audience expects — see `resolveDotfileConfigPath`.
        if (resolveDotfileConfigPath(env, &dotfile_storage)) |dotfile| loaded.setPath(dotfile);
        return loaded;
    };
    if (readConfig(io, path, inherited)) |parsed| {
        loaded.config = parsed;
        loaded.setPath(path);
        return loaded;
    }
    // No file yet: target the dotfile location for later writes.
    if (resolveDotfileConfigPath(env, &dotfile_storage)) |dotfile| {
        loaded.setPath(dotfile);
        return loaded;
    }
    loaded.setPath(path);
    return loaded;
}

/// Read and parse one candidate. Null means "there was no usable file here",
/// which is the normal case for every location but one and must never be an
/// error.
fn readConfig(io: std.Io, path: []const u8, inherited: config_module.Inherited) ?Config {
    var bytes: [config_module.max_config_bytes]u8 = undefined;
    var file = std.Io.Dir.cwd().openFile(io, path, .{}) catch return null;
    defer file.close(io);
    // An over-long config is truncated, not refused, keeping earlier lines.
    const read = file.readPositionalAll(io, &bytes, 0) catch return null;
    return config_module.loadOver(inherited, bytes[0..read]);
}

fn reportInherited(inherited: *const config_module.Inherited) void {
    var buffer: [1024]u8 = undefined;
    const line = ghostty.summary(inherited, &buffer);
    if (line.len != 0) std.log.info("config: {s}", .{line});
}

/// Where the workspace layout is written: the platform state directory,
/// never the config file. `PHUX_COCKPIT_STATE` names a file and wins. Null
/// silently disables persistence.
pub fn resolveStatePath(
    env: native_sdk.app_dirs.Env,
    override_path: ?[]const u8,
    dir_storage: []u8,
    path_storage: []u8,
) ?[]const u8 {
    if (override_path) |explicit| {
        if (explicit.len == 0 or explicit.len > path_storage.len) return null;
        @memcpy(path_storage[0..explicit.len], explicit);
        return path_storage[0..explicit.len];
    }
    const dir = native_sdk.app_dirs.resolveOne(
        .{ .name = app_name },
        native_sdk.app_dirs.currentPlatform(),
        env,
        .state,
        dir_storage,
    ) catch return null;
    return session_state.joinPath(dir, path_storage) catch null;
}

/// Provenance for one state-file read. A missing file is the ordinary first
/// launch. A rejected file definitely existed and carries the exact path whose
/// bytes must be preserved. Every other I/O failure is returned to startup.
pub const PersistedStateLoad = union(enum) {
    missing,
    restored,
    rejected_existing: []const u8,
};

/// Read and parse the state file without collapsing "missing", "rejected", and
/// a real I/O failure into one false value.
pub fn readPersistedState(
    io: std.Io,
    path: []const u8,
    out: *PersistedTopologySnapshot,
) !PersistedStateLoad {
    var bytes: [session_state.max_state_bytes + 1]u8 = undefined;
    var file = std.Io.Dir.cwd().openFile(io, path, .{}) catch |err| switch (err) {
        error.FileNotFound => return .missing,
        else => return err,
    };
    defer file.close(io);
    // The extra byte distinguishes an exactly bounded valid file from a file
    // whose valid-looking prefix was truncated at the read ceiling.
    const read = try file.readPositionalAll(io, &bytes, 0);
    if (read > session_state.max_state_bytes) return .{ .rejected_existing = path };
    if (!session_state.parse(bytes[0..read], out)) return .{ .rejected_existing = path };
    return .restored;
}

/// Startup provenance after parsing, migration, and model reconstruction.
///
/// `.restored = null` is valid state containing no terminal tabs. It follows
/// the established fresh-terminal behavior without mislabeling the file as
/// missing or rejected.
pub const WorkspaceRestore = union(enum) {
    missing,
    restored: ?Model,
    rejected_existing: []const u8,
};

/// Rebuild the saved workspace before anything else exists, so the window
/// opens into it. `restored` receives the migrated snapshot for applying
/// working directories once the model is in its final storage.
pub fn restoreWorkspace(
    gpa: std.mem.Allocator,
    io: std.Io,
    path: []const u8,
    restored: *TopologySnapshot,
    max_scrollback_bytes: usize,
) !WorkspaceRestore {
    var persisted: PersistedTopologySnapshot = undefined;
    switch (try readPersistedState(io, path, &persisted)) {
        .missing => return .missing,
        .rejected_existing => |rejected_path| return .{ .rejected_existing = rejected_path },
        .restored => {},
    }
    const snapshot = migrateTopologySnapshot(persisted) catch
        return .{ .rejected_existing = path };
    restored.* = snapshot;
    if (snapshot.tab_count == 0) return .{ .restored = null };
    const model = try model_module.restoreModelWithScrollback(
        gpa,
        io,
        .{ .v5 = snapshot },
        max_scrollback_bytes,
    );
    return .{ .restored = model };
}

fn freshWorkspace(
    gpa: std.mem.Allocator,
    io: std.Io,
    max_scrollback_bytes: usize,
) !Model {
    const session = try grid.Session.createWithScrollback(gpa, io, 80, 24, max_scrollback_bytes);
    return initialModelWithIo(gpa, io, session) catch |err| {
        session.destroy();
        return err;
    };
}

pub const WorkspaceStateProvenance = enum {
    missing,
    restored,
    rejected_existing,
};

const InitialWorkspace = struct {
    model: *Model,
    provenance: WorkspaceStateProvenance,
    rejected_state_path: ?[]const u8 = null,
};

fn loadInitialWorkspace(
    gpa: std.mem.Allocator,
    io: std.Io,
    state_path: ?[]const u8,
    restored_snapshot: *TopologySnapshot,
    max_scrollback_bytes: usize,
) !InitialWorkspace {
    const model = try std.heap.page_allocator.create(Model);
    errdefer std.heap.page_allocator.destroy(model);
    const outcome: WorkspaceRestore = if (state_path) |path|
        try restoreWorkspace(gpa, io, path, restored_snapshot, max_scrollback_bytes)
    else
        .missing;
    model.* = switch (outcome) {
        .restored => |saved| saved orelse try freshWorkspace(gpa, io, max_scrollback_bytes),
        else => try freshWorkspace(gpa, io, max_scrollback_bytes),
    };
    return .{
        .model = model,
        .provenance = switch (outcome) {
            .missing => .missing,
            .restored => .restored,
            .rejected_existing => .rejected_existing,
        },
        .rejected_state_path = if (outcome == .rejected_existing) outcome.rejected_existing else null,
    };
}

fn initializeStatePersistence(
    model: *Model,
    state_path: ?[]const u8,
    rejected_state_path: ?[]const u8,
) void {
    model.state.setPath(state_path);
    if (rejected_state_path) |path| model.state.preserveRejectedExisting(path);
    // Seed the shape hash with what is already live, so a launch that changes
    // nothing writes nothing.
    model.state.fingerprint = model.topologyFingerprint();
}

/// Log every config diagnostic in full; the dismissible band
/// (`projection.configNoticeLine`) is the user-facing notice.
fn reportConfigDiagnostics(user_config: *const Config) void {
    for (user_config.diagnosticSlice()) |diagnostic| {
        switch (diagnostic.kind) {
            .unsupported_key => std.log.warn(
                "config line {d}: '{s}' is understood but does nothing in this build",
                .{ diagnostic.line, diagnostic.text() },
            ),
            .unknown_key => std.log.warn(
                "config line {d}: unknown setting '{s}'",
                .{ diagnostic.line, diagnostic.text() },
            ),
            .bad_value => std.log.warn(
                "config line {d}: value '{s}' was not understood, so the default is in effect",
                .{ diagnostic.line, diagnostic.text() },
            ),
            .missing_separator => std.log.warn(
                "config line {d}: no '=' on this line, so it was skipped",
                .{diagnostic.line},
            ),
            .too_long => std.log.warn(
                "config line {d}: value is too long for this setting, so it was ignored",
                .{diagnostic.line},
            ),
        }
    }
}

pub const InitializedModel = struct {
    /// Final storage is allocated before configuration. Passing this owner
    /// through startup must not copy the large runtime model onto each frame.
    model: *Model,
    restored_snapshot: TopologySnapshot = .{},
    provenance: WorkspaceStateProvenance,
};

/// Complete startup after path and environment resolution. Keeping this step
/// explicit makes the shared composition-root contract testable without
/// manufacturing a process Init: config, restore, cwd, shell, scrollback and
/// tab-placement precedence still execute in exactly one implementation.
pub fn initializeResolvedModel(
    gpa: std.mem.Allocator,
    io: std.Io,
    user_config: Config,
    config_path: ?[]const u8,
    state_path: ?[]const u8,
    tab_placement_override: ?[]const u8,
) !InitializedModel {
    const max_scrollback_bytes: usize = @intCast(@min(
        user_config.scrollback_bytes,
        @as(u64, std.math.maxInt(usize)),
    ));
    var restored_snapshot: TopologySnapshot = .{};
    const loaded = try loadInitialWorkspace(
        gpa,
        io,
        state_path,
        &restored_snapshot,
        max_scrollback_bytes,
    );
    const model = loaded.model;
    errdefer std.heap.page_allocator.destroy(model);
    errdefer model_module.deinitModel(model);
    model.provider.max_scrollback_bytes = max_scrollback_bytes;
    if (user_config.shell.slice().len != 0 and !model.provider.setShellCommand(user_config.shell.slice())) {
        std.log.warn(
            "config: shell/command value was rejected (empty, too long, or contains a NUL), so the default shell is in effect",
            .{},
        );
    }
    initializeStatePersistence(model, state_path, loaded.rejected_state_path);
    model.config = user_config;
    model.config_file.setPath(config_path orelse "");
    model.tab_placement = switch (model.config.tab_placement) {
        .top => .top,
        .side => .side,
    };
    if (loaded.provenance == .restored) model.tab_placement = restored_snapshot.tab_placement;
    if (tab_placement_override) |value| {
        if (tabPlacementFromText(value)) |placement| model.tab_placement = placement;
    }
    return .{
        .model = model,
        .restored_snapshot = restored_snapshot,
        .provenance = loaded.provenance,
    };
}

/// Construct the complete shipping model before either composition root opens
/// a window. Config precedence, state restoration, cwd projection, and the
/// optional Phux provider therefore cannot drift between Zig and TypeScript.
pub fn initializeModel(gpa: std.mem.Allocator, init: std.process.Init) !InitializedModel {
    const env = native_sdk.debug.envFromMap(init.environ_map);
    var state_dir_storage: [std.fs.max_path_bytes]u8 = undefined;
    var state_path_storage: [std.fs.max_path_bytes]u8 = undefined;
    const state_path = resolveStatePath(
        env,
        init.environ_map.get("PHUX_COCKPIT_STATE"),
        &state_dir_storage,
        &state_path_storage,
    );

    const loaded_config = loadUserConfig(init.io, init);
    var user_config = resolvePhuxConfig(loaded_config.config, .{
        .socket = init.environ_map.get("PHUX_SOCKET"),
        .session = init.environ_map.get("PHUX_SESSION"),
        .runtime_dir = init.environ_map.get("XDG_RUNTIME_DIR"),
        .uid = init.environ_map.get("UID"),
        .user = init.environ_map.get("USER"),
        .remote = init.environ_map.get("PHUX_REMOTE"),
    });
    if (phux_enabled) restoreRememberedRemote(init.io, state_path, &user_config);
    reportConfigDiagnostics(&user_config);
    const initialized = try initializeResolvedModel(
        gpa,
        init.io,
        user_config,
        loaded_config.path(),
        if (phux_enabled) null else state_path,
        init.environ_map.get("PHUX_COCKPIT_TABS"),
    );
    errdefer std.heap.page_allocator.destroy(initialized.model);
    errdefer model_module.deinitModel(initialized.model);
    initialized.model.ghostty = loaded_config.ghostty;
    const remote_provider = try createConfiguredPhuxProvider(init, &user_config);
    attachPhuxProvider(initialized.model, remote_provider);
    if (remote_provider != null) {
        if (try createPhuxPeerFromConfig(std.heap.page_allocator, init.io, &user_config)) |peer| {
            errdefer peer.destroy();
            const slot = try initialized.model.freePeerSlot();
            initialized.model.peers.items[slot].provider = peer;
        }
        try attachRememberedPeers(std.heap.page_allocator, init.io, initialized.model);
    }
    return initialized;
}
