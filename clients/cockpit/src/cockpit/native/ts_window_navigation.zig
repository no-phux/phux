//! Open native windows are presentation identities, including empty windows.
//! Their slot is only an address: the lifetime epoch authorizes activation.
const std = @import("std");

pub const Target = struct {
    window: u8,
    epoch: u64,
    tab: ?struct { generation: u64, id: u32 } = null,

    pub fn validWindow(self: Target, model: anytype) bool {
        if (!model.windowOpen(self.window)) return false;
        if (self.epoch == std.math.maxInt(u64)) return false;
        return model.window_epochs[self.window] == self.epoch;
    }

    /// Reordering preserves a tab's identity; removal or replacement refuses.
    pub fn resolve(self: Target, model: anytype) ?Selection {
        if (!self.validWindow(model)) return null;
        const tab = self.tab orelse return .{ .window = self.window };
        const ws = model.wsAtConst(self.window) orelse return null;
        if (tab.generation == std.math.maxInt(u64)) return null;
        if (ws.tab_generation != tab.generation) return null;
        for (ws.tab_ids[0..ws.tab_count], 0..) |id, index| {
            if (id != 0 and id == tab.id) return .{ .window = self.window, .tab = @intCast(index) };
        }
        return null;
    }
};

pub const Selection = struct { window: u8, tab: ?u8 = null };

/// Explicit visual elision, on a UTF-8 boundary.
pub fn display(text: []const u8, out: []u8) []const u8 {
    if (text.len <= out.len) {
        @memcpy(out[0..text.len], text);
        return out[0..text.len];
    }
    if (out.len < 3) return "";
    var end = out.len - 3;
    while (end > 0 and (text[end] & 0xc0) == 0x80) end -= 1;
    @memcpy(out[0..end], text[0..end]);
    @memcpy(out[end..][0..3], "…");
    return out[0 .. end + 3];
}

const Fixture = struct {
    const Workspace = struct { tab_ids: [2]u32 = .{ 11, 12 }, tab_count: usize = 2, tab_generation: u64 = 7, selected_tab: u8 = 0, web_selected: bool = false };
    window_epochs: [3]u64 = .{ 1, 2, 3 },
    open: [3]bool = @splat(true),
    workspaces: [3]Workspace = .{ .{}, .{ .tab_count = 0 }, .{} },
    active_window: usize = 0,
    pub fn windowOpen(self: *const Fixture, index: usize) bool {
        return index < self.open.len and self.open[index];
    }
    pub fn wsAtConst(self: *const Fixture, index: usize) ?*const Workspace {
        return if (self.windowOpen(index)) &self.workspaces[index] else null;
    }
};

test "tab identity follows reorder but refuses removed and replaced tabs" {
    var model: Fixture = .{};
    const target: Target = .{ .window = 2, .epoch = 3, .tab = .{ .generation = 7, .id = 11 } };
    model.workspaces[2].tab_ids = .{ 12, 11 };
    try std.testing.expectEqual(@as(?u8, 1), target.resolve(&model).?.tab);
    model.workspaces[2].tab_ids[1] = 13;
    try std.testing.expect(target.resolve(&model) == null);
    model.workspaces[2].tab_ids[1] = 11;
    model.workspaces[2].tab_generation += 1;
    try std.testing.expect(target.resolve(&model) == null);
    try std.testing.expect((Target{ .window = 255, .epoch = 0 }).resolve(&model) == null);
}

test "a closed and reopened window slot refuses its old identity" {
    var model: Fixture = .{};
    const target: Target = .{ .window = 1, .epoch = 2 };
    try std.testing.expect(target.resolve(&model) != null);
    model.window_epochs[1] += 1;
    try std.testing.expect(target.resolve(&model) == null);
}
