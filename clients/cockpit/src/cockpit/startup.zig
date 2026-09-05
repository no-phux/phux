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
const scene = @import("native/scene.zig");

pub const Config = config_module.Config;
pub const parseConfig = config_module.parse;
const PhuxProvider = support.PhuxProvider;
const phux_enabled = support.phux_enabled;
const Model = model_module.Model;
const TopologySnapshot = topology.TopologySnapshot;
const PersistedTopologySnapshot = topology.PersistedTopologySnapshot;
const TabPlacement = topology.TabPlacement;
const migrateTopologySnapshot = topology.migrateTopologySnapshot;
const initialProductionModelWithIo = model_module.initialProductionModelWithIo;
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
    return resolved;
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
    return try PhuxProvider.create(gpa, io, .{ .unix = socket }, session, "phux-cockpit");
}

/// Read-only construction evidence for settings/tests. This returns only the
/// local-domain location; no TCP endpoint is admitted by this composition.
pub fn configuredPhuxSocket(provider: *const PhuxProvider) []const u8 {
    if (comptime !phux_enabled) return "";
    return switch (provider.endpoint) {
        .unix => |path| path,
        else => unreachable,
    };
}

pub fn configuredPhuxSession(provider: *const PhuxProvider) ?[]const u8 {
    if (comptime !phux_enabled) return null;
    return provider.session;
}

fn createConfiguredPhuxProvider(init: std.process.Init, config: *const Config) !?*PhuxProvider {
    return createPhuxProviderFromConfig(std.heap.page_allocator, init.io, config);
}

/// Where the config file lives, resolved through the SDK's `app_dirs`
/// primitive so the platform owns the rule (macOS:
/// `~/Library/Preferences/Phux Cockpit/config`). `PHUX_COCKPIT_CONFIG` names
/// a file directly and wins — that is the seam a test, a second profile, or a
/// `--config` wrapper uses, and it costs one env read.
///
/// Returns null when the platform has no home to resolve against, which is a
/// silent fall back to defaults, never an error: a terminal that refuses to
/// start because it could not find a file the user never wrote is broken.
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

/// The dotfile location: `$XDG_CONFIG_HOME/phux-cockpit/config`, else
/// `~/.config/phux-cockpit/config`.
///
/// `app_dirs` follows each platform's own convention, which on macOS means
/// `~/Library/Preferences/Phux Cockpit/`. That is correct for a Mac app and
/// wrong for this audience: the people most likely to write a config here are
/// arriving from Ghostty, and they will put the file in `~/.config` without
/// looking it up. Checking here FIRST costs one stat and removes an entire
/// class of "my config does nothing" confusion. The platform path still works,
/// so nothing is taken away.
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

/// Read and parse the user's config. Every failure — no home, no file, an
/// unreadable file, a file larger than the ceiling — lands on defaults, and a
/// malformed LINE is already a diagnostic rather than a failure inside the
/// parser. There is exactly one way this function does not produce a usable
/// Config, and that is never.
/// The loaded config plus WHERE it came from.
///
/// The path is now part of the answer because the settings surface writes a
/// theme choice back into that same file, and `update` has no environment to
/// re-resolve it from — the same reason `StatePersistence` carries the layout
/// path. `path_len` is zero when no location could be resolved at all, which
/// disables the write rather than failing anything.
pub const LoadedConfig = struct {
    config: Config,
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

    var loaded: LoadedConfig = .{ .config = .{} };

    // An explicit override answers on its own — including for WRITING. A
    // wrapper or a test that named a file is naming the file the app should
    // edit too, whether or not it exists yet.
    if (override) |explicit| {
        loaded.setPath(explicit);
        if (readConfig(io, explicit)) |parsed| loaded.config = parsed;
        return loaded;
    }
    // Otherwise the dotfile location is tried first and the platform location
    // second — first file that opens wins, so someone who has never heard of
    // `~/Library/Preferences` and someone who expects a Mac app to live there
    // are both right.
    if (resolveDotfileConfigPath(env, &dotfile_storage)) |dotfile| {
        if (readConfig(io, dotfile)) |parsed| {
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
    if (readConfig(io, path)) |parsed| {
        loaded.config = parsed;
        loaded.setPath(path);
        return loaded;
    }
    // NO config file exists yet, which is the ordinary first-run state. The
    // write target is the DOTFILE location rather than the platform one for
    // the same reason the read tries it first: it is where this audience will
    // look for it afterwards.
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
fn readConfig(io: std.Io, path: []const u8) ?Config {
    var bytes: [config_module.max_config_bytes]u8 = undefined;
    var file = std.Io.Dir.cwd().openFile(io, path, .{}) catch return null;
    defer file.close(io);
    // A config longer than the ceiling is TRUNCATED, not refused: the bytes
    // that fit are a prefix of whole lines plus at most one partial one, and a
    // partial line is a diagnostic. Refusing the whole file would drop every
    // valid setting above it.
    const read = file.readPositionalAll(io, &bytes, 0) catch return null;
    return config_module.loadOrDefault(bytes[0..read]);
}

/// Where the workspace LAYOUT is written: the platform STATE directory
/// (macOS: `~/Library/Application Support/Phux Cockpit/State`), never the
/// config file. Layout is state — nobody hand-writes it, and it changes every
/// time a tab opens — so mixing it into the file a user edits would mean
/// rewriting their settings on every split.
///
/// `PHUX_COCKPIT_STATE` names a file directly and wins, the same seam
/// `PHUX_COCKPIT_CONFIG` gives the config: a second profile, a test, or a
/// wrapper that wants a throwaway workspace. Null means no state directory
/// could be resolved, which silently disables persistence rather than
/// refusing to start.
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
/// opens INTO the restored layout instead of being seen to assemble it.
/// `restored` receives the migrated snapshot, which still holds the working
/// directories the panes have to be put in once the model reaches its final
/// storage.
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
        .{ .v4 = snapshot },
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
    return initialProductionModelWithIo(gpa, io, session) catch |err| {
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
    model: Model,
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
    const outcome: WorkspaceRestore = if (state_path) |path|
        try restoreWorkspace(gpa, io, path, restored_snapshot, max_scrollback_bytes)
    else
        .missing;
    return switch (outcome) {
        .missing => .{
            .model = try freshWorkspace(gpa, io, max_scrollback_bytes),
            .provenance = .missing,
        },
        .rejected_existing => |path| .{
            .model = try freshWorkspace(gpa, io, max_scrollback_bytes),
            .provenance = .rejected_existing,
            .rejected_state_path = path,
        },
        .restored => |saved| restored: {
            if (saved) |model| {
                break :restored .{ .model = model, .provenance = .restored };
            }
            break :restored .{
                .model = try freshWorkspace(gpa, io, max_scrollback_bytes),
                .provenance = .restored,
            };
        },
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

/// Say out loud what the config file did not do.
///
/// This is the RECORD, not the notification. It carries every diagnostic in
/// full sentences, with the offending text quoted, which is what someone
/// debugging a config in a terminal wants — and from a bundle it lands in the
/// unified log, where nobody is looking. The notification is the dismissible
/// band the app itself draws (`projection.configNoticeLine`), which is what
/// closes the gap between "the setting did nothing" and "the user found out".
/// Both read the same diagnostics; neither is a second source of truth.
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
    model: Model,
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
    var model = loaded.model;
    errdefer model_module.deinitModel(&model);
    model.provider.max_scrollback_bytes = max_scrollback_bytes;
    if (user_config.shell.slice().len != 0 and !model.provider.setShellCommand(user_config.shell.slice())) {
        std.log.warn(
            "config: shell/command value was rejected (empty, too long, or contains a NUL), so the default shell is in effect",
            .{},
        );
    }
    initializeStatePersistence(&model, state_path, loaded.rejected_state_path);
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
    const user_config = resolvePhuxConfig(loaded_config.config, .{
        .socket = init.environ_map.get("PHUX_SOCKET"),
        .session = init.environ_map.get("PHUX_SESSION"),
        .runtime_dir = init.environ_map.get("XDG_RUNTIME_DIR"),
        .uid = init.environ_map.get("UID"),
        .user = init.environ_map.get("USER"),
    });
    reportConfigDiagnostics(&user_config);
    var initialized = try initializeResolvedModel(
        gpa,
        init.io,
        user_config,
        loaded_config.path(),
        state_path,
        init.environ_map.get("PHUX_COCKPIT_TABS"),
    );
    errdefer model_module.deinitModel(&initialized.model);
    const remote_provider = try createConfiguredPhuxProvider(init, &user_config);
    attachPhuxProvider(&initialized.model, remote_provider);
    return initialized;
}
