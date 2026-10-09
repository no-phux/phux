//! Pointer-tab reorder. The host payload names a stable tab id and a view
//! point; the engine owns the gesture so a snapshot cannot drop the origin.
const std = @import("std");
const model_module = @import("../model.zig");

const Model = model_module.Model;

pub const command_name = "cockpit.tab-drag";
pub const payload_len = 10;

pub const Phase = enum(u8) { change = 0, end = 1, cancel = 2 };

pub const Event = struct {
    source_id: u32,
    phase: Phase,
    x: f32,
    y: f32,
};

pub const Located = struct {
    window: usize,
    index: usize,
    tab_id: u32,
};

pub fn decode(bytes: []const u8) ?Event {
    if (bytes.len != payload_len) return null;
    const phase: Phase = switch (bytes[4]) {
        0 => .change,
        1 => .end,
        2 => .cancel,
        else => return null,
    };
    const source_id = std.mem.readInt(u32, bytes[0..4], .little);
    if (source_id == 0) return null;
    return .{
        .source_id = source_id,
        .phase = phase,
        .x = @floatFromInt(std.mem.readInt(i16, bytes[6..8], .little)),
        .y = @floatFromInt(std.mem.readInt(i16, bytes[8..10], .little)),
    };
}

pub fn findTab(model: *const Model, tab_id: u32) ?Located {
    if (tab_id == 0) return null;
    const active = model.active_window;
    if (indexIn(model, active, tab_id)) |index| return .{ .window = active, .index = index, .tab_id = tab_id };
    for (0..model_module.max_windows) |window| {
        if (window == active) continue;
        if (indexIn(model, window, tab_id)) |index| return .{ .window = window, .index = index, .tab_id = tab_id };
    }
    return null;
}

fn indexIn(model: *const Model, window: usize, tab_id: u32) ?usize {
    const workspace = model.wsAtConst(window) orelse return null;
    return indexOf(workspace, tab_id);
}

pub fn indexOf(workspace: *const model_module.Workspace, tab_id: u32) ?usize {
    for (workspace.tab_ids[0..workspace.tab_count], 0..) |id, index| {
        if (id != 0 and id == tab_id) return index;
    }
    return null;
}

/// Swap `tab_id` one step at a time until it occupies `destination`.
/// Selection follows the existing neighbor swap, so a background tab stays
/// background. Returns whether the order changed.
pub fn moveTabToIndex(model: *Model, window: usize, tab_id: u32, destination: usize) bool {
    const workspace = model.wsAt(window) orelse return false;
    if (destination >= workspace.tab_count) return false;
    const terminal = tabTerminal(workspace, tab_id) orelse return false;
    var changed = false;
    var guard = workspace.tab_count;
    while (guard > 0) : (guard -= 1) {
        const current = indexOf(workspace, tab_id) orelse return changed;
        if (current == destination) return changed;
        const step: i8 = if (destination > current) 1 else -1;
        if (!workspace.moveTerminal(terminal, step)) return changed;
        changed = true;
    }
    return changed;
}

fn tabTerminal(workspace: *model_module.Workspace, tab_id: u32) ?model_module.TerminalRef {
    const index = indexOf(workspace, tab_id) orelse return null;
    return workspace.tabTerminal(index);
}
