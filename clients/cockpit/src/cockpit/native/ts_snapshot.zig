const std = @import("std");
const model_module = @import("../model.zig");
const projection = @import("workspace_projection.zig");
const protocol = @import("ts_protocol.zig");
const theme_module = @import("../../config/theme.zig");

const Model = model_module.Model;

/// 18 bytes of framing, then: active window, placement, tab count, selected
/// tab, flags, a reserved byte, and the RUN the band has room for (first
/// visible tab, visible count, per-tab extent in points as a u16). The run is
/// the engine's because only it knows the surface size and the shipping
/// projection's rule for it (`visibleTabRun`); the core slices its tab list
/// to the run and shows a cue for the rest.
pub const header_len: usize = protocol.snapshot_header_len + 10;
pub const max_title_bytes: usize = 128;
pub const max_cwd_bytes: usize = 128;
pub const max_bytes: usize = 4096;

pub const Error = error{BufferTooSmall};

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

    const workspace = model.wsConst();
    out[18] = @intCast(model.active_window);
    out[19] = @intFromEnum(model.tab_placement);
    out[20] = @intCast(workspace.tab_count);
    out[21] = @intCast(workspace.selected_tab);
    out[22] = snapshotFlags(model);
    out[23] = 0;
    out[24] = run.first;
    out[25] = run.count;
    std.mem.writeInt(u16, out[26..28], run.extent, .little);

    var written: usize = header_len;
    written = try encodeTabs(model, workspace, out, written);
    written = try encodeSettings(model, probe, out, written);
    written = try encodeSecondaryWindows(model, runs, out, written);
    return out[0..written];
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
        var title_buffer: [max_title_bytes]u8 = undefined;
        const title = projection.terminalTitleInto(model, terminal, &title_buffer);
        const bounded = title[0..@min(title.len, max_title_bytes)];
        const cwd_all = if (model.provider.terminalConst(terminal)) |pane| pane.pwd() else "";
        const cwd = cwd_all[0..@min(cwd_all.len, max_cwd_bytes)];
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
