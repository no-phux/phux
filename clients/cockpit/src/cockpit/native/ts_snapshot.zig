const std = @import("std");
const model_module = @import("../model.zig");
const projection = @import("workspace_projection.zig");
const protocol = @import("ts_protocol.zig");
const navigation = @import("ts_navigation.zig");
const theme_module = @import("../../config/theme.zig");
const signals = @import("remote_signals.zig");
const tab_commands = @import("tab_commands.zig");

const Model = model_module.Model;

/// 18 bytes of framing, then: active window, placement, tab count, selected
/// tab, flags, a reserved byte, and the RUN the band has room for (first
/// visible tab, visible count, per-tab extent in points as a u16). The run is
/// the engine's because only it knows the surface size and the shipping
/// projection's rule for it (`visibleTabRun`); the core slices its tab list
/// to the run and shows a cue for the rest.
pub const header_len: usize = protocol.snapshot_header_len + 10;
// Compact strip labels; the paginated navigation catalog carries full labels.
pub const max_title_bytes: usize = 20;
pub const max_cwd_bytes: usize = 8;
pub const max_bytes: usize = 4096;

pub const Error = error{BufferTooSmall};

/// Extension records close the snapshot, after every fixed section: a kind
/// byte, a `u16` length, then that many payload bytes. A decoder that does
/// not know a kind steps over it by its length instead of reading what
/// follows as something it is not, so a later kind costs the seam nothing.
pub const ExtensionKind = enum(u8) { agent_rows = 1, tab_contexts = 2, navigation_context = 3 };
pub const max_session_bytes: usize = 64;
pub const max_endpoint_bytes: usize = 160;
pub const max_connection_detail_bytes: usize = 80;
const navigation_context_bytes = 6 + max_session_bytes + max_endpoint_bytes + max_connection_detail_bytes;

/// Provider slug and per-snapshot ceiling for the agent rows. The ceiling is
/// what keeps the record inside `max_bytes` beside a full workspace; the
/// comptime assert below is the proof, not this comment.
pub const max_provider_bytes: usize = 12;
pub const max_agent_rows: usize = 24;
const agent_row_bytes: usize = 5 + max_provider_bytes;
const agent_record_bytes: usize = 4 + max_agent_rows * agent_row_bytes;

pub const TabRun = struct {
    first: u8 = 0,
    count: u8 = 0,
    extent: u16 = 168,
};

/// What the settings surface needs to know about the configuration file,
/// probed by the engine on request (never by a snapshot: a snapshot is pure).
pub const ConfigProbe = struct {
    exists: bool = false,
    writable: bool = true,
    probed: bool = false,
};

pub const max_config_path_bytes: usize = 200;

comptime {
    var theme_bytes: usize = 0;
    for (theme_module.builtins) |theme| theme_bytes += 1 + @min(theme.name.len, 32);
    const fixed = header_len + 4 + theme_bytes + max_config_path_bytes + 1 + model_module.max_secondary_windows * 7 + model_module.max_windows + agent_record_bytes + 3 + tab_commands.context_len + navigation_context_bytes;
    const tabs = model_module.max_windows * model_module.max_tabs * (7 + max_title_bytes + max_cwd_bytes);
    std.debug.assert(fixed + tabs <= max_bytes);
}

/// Serialize the active window's chrome projection. Raw cells, process state,
/// provider slots, and platform window ids deliberately never cross this seam.
/// The run of every window, main first; a closed secondary slot's run is
/// unused. The engine derives them from each window's own surface size.
pub const WindowRuns = [1 + model_module.max_secondary_windows]TabRun;

pub fn encode(model: *const Model, sequence: u64, revision: u64, runs: WindowRuns, probe: ConfigProbe, out: []u8) Error![]const u8 {
    const run = runs[0];
    if (out.len < header_len) return error.BufferTooSmall;
    const framed = protocol.encodeSnapshotHeader(sequence, revision);
    @memcpy(out[0..framed.len], &framed);

    // The fixed header is always the main window. `active_window` is carried
    // separately for focused targeting; using wsConst() here silently paired
    // the secondary tab count with runs[0], corrupting the snapshot whenever
    // a secondary window owned focus and the two windows had different runs.
    const workspace = &model.primary;
    out[18] = @intCast(model.active_window);
    out[19] = @intFromEnum(model.tab_placement);
    out[20] = @intCast(workspace.tab_count);
    out[21] = @intCast(workspace.selected_tab);
    out[22] = snapshotFlags(model);
    out[23] = @intFromEnum(navigation.connection(model));
    out[24] = run.first;
    out[25] = run.count;
    std.mem.writeInt(u16, out[26..28], run.extent, .little);

    var written: usize = header_len;
    written = try encodeTabs(model, workspace, out, written);
    written = try encodeSettings(model, probe, out, written);
    written = try encodeSecondaryWindows(model, runs, out, written);
    if (written + model_module.max_windows > out.len) return error.BufferTooSmall;
    for (0..model_module.max_windows) |window| out[written + window] = @intFromEnum(signals.windowState(model, window));
    written += model_module.max_windows;
    written = try encodeTargetContexts(model, out, written);
    written = try encodeAgentRows(model, out, written);
    written = try encodeNavigationContext(model, out, written);
    return out[0..written];
}

fn currentSession(model: *const Model, out: []u8) []const u8 {
    const remote = model.phuxConst() orelse return if (navigation.connection(model) == .local) "Local terminals" else "";
    const id = remote.selectedSessionId() orelse return "";
    for (remote.sessionCatalog()) |session| {
        if (session.id != id) continue;
        if (session.name.len > 0) return session.name;
        break;
    }
    return std.fmt.bufPrint(out, "Session #{d}", .{id}) catch "Session";
}

fn coordinatorEndpoint(model: *const Model, out: []u8) []const u8 {
    const remote = model.phuxConst() orelse return "";
    return switch (remote.endpointDescriptor()) {
        .unix => |path| path,
        .tcp => |address| std.fmt.bufPrint(out, "{s}:{d}", .{ address.host, address.port }) catch address.host,
        // A registered remote host is named by its registry label; the
        // endpoint URI and credentials stay inside phux-client-ffi.
        .remote => |host| std.fmt.bufPrint(out, "Registered host {s}", .{host.target}) catch host.target,
    };
}

fn connectionDetail(model: *const Model) []const u8 {
    return switch (navigation.connection(model)) {
        .local => "Ephemeral local PTYs",
        .connecting => "Connecting to coordinator",
        .connected => "Connected to coordinator",
        .offline => "Coordinator unavailable",
        .workspace_unavailable => "Connected; shared workspace unavailable",
    };
}

fn encodeContextText(text: []const u8, display: []u8, out: []u8, start: usize) Error!usize {
    const bounded = navigation.displayText(text, display);
    if (start + 1 + bounded.len > out.len) return error.BufferTooSmall;
    out[start] = @intCast(bounded.len);
    @memcpy(out[start + 1 ..][0..bounded.len], bounded);
    return start + 1 + bounded.len;
}

/// Display context only: endpoint elision never changes an execution identity.
fn encodeNavigationContext(model: *const Model, out: []u8, start: usize) Error!usize {
    if (start + 3 > out.len) return error.BufferTooSmall;
    var full: [512]u8 = undefined;
    var session: [max_session_bytes]u8 = undefined;
    var endpoint: [max_endpoint_bytes]u8 = undefined;
    var detail: [max_connection_detail_bytes]u8 = undefined;
    var at = try encodeContextText(currentSession(model, &full), &session, out, start + 3);
    at = try encodeContextText(coordinatorEndpoint(model, &full), &endpoint, out, at);
    at = try encodeContextText(connectionDetail(model), &detail, out, at);
    out[start] = @intFromEnum(ExtensionKind.navigation_context);
    std.mem.writeInt(u16, out[start + 1 ..][0..2], @intCast(at - start - 3), .little);
    return at;
}

/// The agent sessions running under each window's tabs, as one extension
/// record: `[window][tab][state][flags][provider len][provider]` per row.
/// The row is addressed by the tab it hangs under, never by the session's own
/// identity -- an agent session addresses no surface (ADR-0103), and the core
/// draws it as a child of a terminal it already has.
///
/// A workspace with no agent sessions writes NO record at all, so a snapshot
/// that carries none is byte-identical to one from before this kind existed.
fn encodeAgentRows(model: *const Model, out: []u8, start: usize) Error!usize {
    var written = start;
    var emitted: usize = 0;
    for (0..model_module.max_windows) |window| {
        const workspace = model.wsAtConst(window) orelse continue;
        for (0..workspace.tab_count) |index| {
            const room = max_agent_rows - emitted;
            if (room == 0) break;
            var rows: [max_agent_rows]projection.AgentRow = undefined;
            const count = projection.tabAgentRows(model, workspace, index, rows[0..room]);
            for (rows[0..count]) |row| {
                var provider_display: [max_provider_bytes]u8 = undefined;
                const provider = navigation.displayText(row.provider_name, &provider_display);
                // The record's own header is claimed by the first row, so a
                // quiet workspace pays nothing -- not even four bytes.
                if (emitted == 0) {
                    if (start + 4 > out.len) return error.BufferTooSmall;
                    out[start] = @intFromEnum(ExtensionKind.agent_rows);
                    written = start + 4;
                }
                if (written + 5 + provider.len > out.len) return error.BufferTooSmall;
                out[written] = @intCast(window);
                out[written + 1] = @intCast(index);
                out[written + 2] = @intFromEnum(row.state);
                out[written + 3] = if (row.needsAttention()) 1 else 0;
                out[written + 4] = @intCast(provider.len);
                @memcpy(out[written + 5 ..][0..provider.len], provider);
                written += 5 + provider.len;
                emitted += 1;
            }
        }
    }
    if (emitted == 0) return start;
    out[start + 3] = @intCast(emitted);
    std.mem.writeInt(u16, out[start + 1 ..][0..2], @intCast(written - (start + 3)), .little);
    return written;
}

fn encodeTargetContexts(model: *const Model, out: []u8, start: usize) Error!usize {
    if (start + 3 + tab_commands.context_len > out.len) return error.BufferTooSmall;
    out[start] = @intFromEnum(ExtensionKind.tab_contexts);
    std.mem.writeInt(u16, out[start + 1 ..][0..2], tab_commands.context_len, .little);
    for (0..model_module.max_windows) |window| {
        const at = start + 3 + window * 16;
        const workspace_at = model.wsAtConst(window);
        std.mem.writeInt(u64, out[at..][0..8], model.window_epochs[window], .little);
        std.mem.writeInt(u64, out[at + 8 ..][0..8], if (workspace_at) |ws| ws.tab_generation else std.math.maxInt(u64), .little);
    }
    return start + 3 + tab_commands.context_len;
}

/// The open secondary windows, each as its own section: index, tab count,
/// selection, run, then the same tab records the main section carries. A
/// closed slot is absent; presence is liveness, as it is for the platform
/// windows the core declares from this.
fn encodeSecondaryWindows(model: *const Model, runs: WindowRuns, out: []u8, start: usize) Error!usize {
    var written = start;
    if (written + 1 > out.len) return error.BufferTooSmall;
    const count_at = written;
    out[count_at] = 0;
    written += 1;
    for (1..1 + model_module.max_secondary_windows) |index| {
        if (!model.windowOpen(index)) continue;
        const workspace = model.wsAtConst(index) orelse continue;
        if (written + 7 > out.len) return error.BufferTooSmall;
        out[written] = @intCast(index);
        out[written + 1] = @intCast(workspace.tab_count);
        out[written + 2] = @intCast(workspace.selected_tab);
        out[written + 3] = runs[index].first;
        out[written + 4] = runs[index].count;
        std.mem.writeInt(u16, out[written + 5 ..][0..2], runs[index].extent, .little);
        written += 7;
        written = try encodeTabs(model, workspace, out, written);
        out[count_at] += 1;
    }
    return written;
}

fn encodeTabs(model: *const Model, workspace: *const model_module.Workspace, out: []u8, start: usize) Error!usize {
    var written = start;
    for (0..workspace.tab_count) |index| {
        const terminal = workspace.tabTerminal(index) orelse continue;
        var title_buffer: [512]u8 = undefined;
        const title = projection.tabTitleInto(model, workspace, index, &title_buffer);
        var title_display: [max_title_bytes]u8 = undefined;
        const bounded = navigation.displayText(title, &title_display);
        const cwd_all = if (model.provider.terminalConst(terminal)) |pane| pane.pwd() else "";
        var cwd_display: [max_cwd_bytes]u8 = undefined;
        const cwd = navigation.displayText(cwd_all, &cwd_display);
        const needed = 7 + bounded.len + cwd.len;
        if (written + needed > out.len) return error.BufferTooSmall;

        std.mem.writeInt(u32, out[written..][0..4], workspace.tabId(index) orelse 0, .little);
        out[written + 4] = if (projection.terminalNeedsAttention(model, terminal)) 1 else 0;
        out[written + 5] = @intCast(bounded.len);
        out[written + 6] = @intCast(cwd.len);
        @memcpy(out[written + 7 ..][0..bounded.len], bounded);
        @memcpy(out[written + 7 + bounded.len ..][0..cwd.len], cwd);
        written += needed;
    }
    return written;
}

/// The trailer after the tab records: the builtin theme catalog by name (the
/// core has no catalog of its own and must not grow one), the theme in
/// effect, the config file's state as last probed, and its path.
fn encodeSettings(model: *const Model, probe: ConfigProbe, out: []u8, start: usize) Error!usize {
    var written = start;
    if (written + 1 > out.len) return error.BufferTooSmall;
    out[written] = @intCast(theme_module.builtins.len);
    written += 1;
    for (theme_module.builtins) |theme| {
        const name = theme.name[0..@min(theme.name.len, 32)];
        if (written + 1 + name.len > out.len) return error.BufferTooSmall;
        out[written] = @intCast(name.len);
        @memcpy(out[written + 1 ..][0..name.len], name);
        written += 1 + name.len;
    }
    if (written + 2 > out.len) return error.BufferTooSmall;
    out[written] = if (theme_module.indexOf(model.config.theme.slice())) |index| @intCast(index) else 255;
    var config_flags: u8 = 0;
    if (model.config_file.enabled()) config_flags |= 1 << 0;
    if (probe.exists) config_flags |= 1 << 1;
    if (probe.writable) config_flags |= 1 << 2;
    if (probe.probed) config_flags |= 1 << 3;
    out[written + 1] = config_flags;
    written += 2;
    const path_all = if (model.config_file.enabled()) model.config_file.path() else "";
    const path = path_all[0..@min(path_all.len, max_config_path_bytes)];
    if (written + 1 + path.len > out.len) return error.BufferTooSmall;
    out[written] = @intCast(path.len);
    @memcpy(out[written + 1 ..][0..path.len], path);
    return written + 1 + path.len;
}

fn snapshotFlags(model: *const Model) u8 {
    const workspace = model.wsConst();
    var flags: u8 = 0;
    if (model.window_limit_refused) flags |= 1 << 0;
    if (workspace.tab_limit_refused) flags |= 1 << 1;
    if (model.terminal_limit_refused) flags |= 1 << 2;
    if (model.config_write_refused) flags |= 1 << 3;
    if (model.state.write_failed) flags |= 1 << 4;
    if (workspace.palette.open) flags |= 1 << 5;
    if (workspace.settings.open) flags |= 1 << 6;
    return flags;
}

test "navigation snapshot remains bounded across windows with maximum terminal labels" {
    const engine_module = @import("ts_engine.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    for (1..model_module.max_tabs) |_| {
        const intent = protocol.encodeIntent(.{ .kind = .new_terminal, .expected_revision = engine.revision, .argument = 0, .window = 0 });
        try std.testing.expect(engine.applyIntent(&intent, &engine_module.NoShells{}));
    }
    for (1..2) |window| {
        const open_window = protocol.encodeIntent(.{ .kind = .new_window, .expected_revision = engine.revision, .argument = 0 });
        try std.testing.expect(engine.applyIntent(&open_window, &engine_module.NoShells{}));
        for (1..model_module.max_tabs) |_| {
            const intent = protocol.encodeIntent(.{ .kind = .new_terminal, .expected_revision = engine.revision, .argument = 0, .window = @intCast(window) });
            try std.testing.expect(engine.applyIntent(&intent, &engine_module.NoShells{}));
        }
    }
    for (0..2) |window| {
        const workspace = engine.model.wsAtConst(window).?;
        for (0..model_module.max_tabs) |index| {
            const ref = workspace.tabTerminal(index).?;
            const pane = engine.model.provider.terminal(ref).?;
            pane.session.feed("\x1b]2;" ++ "T" ** 128 ++ "\x07");
            pane.session.feed("\x1b]7;file://host/" ++ "d" ** 127 ++ "\x1b\\");
            try std.testing.expectEqual(@as(usize, 128), pane.pwd().len);
        }
    }
    var buffer: [max_bytes]u8 = undefined;
    const response = try engine.snapshot(&buffer);
    try std.testing.expect(response.len <= max_bytes);
    try std.testing.expectEqual(@as(u8, model_module.max_tabs), response[20]);
    try std.testing.expect(std.mem.indexOf(u8, response, "…") != null);
}

test "navigation snapshot context reflects selected session and real endpoint" {
    if (comptime !@import("../phux_support.zig").phux_enabled) return error.SkipZigTest;
    const engine_module = @import("ts_engine.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/real/coordinator.sock" }, "startup-session", "navigation");
    engine.model.phux_provider = remote;
    remote.host.attached_session_id = 42;
    try remote.host.sessions.append(std.testing.allocator, .{ .id = 42, .name = try std.testing.allocator.dupe(u8, "selected-session"), .created_at_unix_secs = 0, .window_count = 1, .attached_client_count = 1, .focused = true });
    var scratch: [512]u8 = undefined;
    try std.testing.expectEqualStrings("selected-session", currentSession(engine.model, &scratch));
    try std.testing.expectEqualStrings("/real/coordinator.sock", coordinatorEndpoint(engine.model, &scratch));
    remote.host.attached_session_id = 43;
    try std.testing.expectEqualStrings("Session #43", currentSession(engine.model, &scratch));
    remote.host.attached_session_id = null;
    try std.testing.expectEqualStrings("", currentSession(engine.model, &scratch));
    var encoded: [navigation_context_bytes]u8 = undefined;
    const length = try encodeNavigationContext(engine.model, &encoded, 0);
    try std.testing.expectEqual(@as(u8, 3), encoded[0]);
    try std.testing.expectEqual(length - 3, std.mem.readInt(u16, encoded[1..3], .little));
    try std.testing.expectError(error.BufferTooSmall, encodeNavigationContext(engine.model, encoded[0 .. length - 1], 0));
}
