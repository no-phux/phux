//! Process-local tab targets and immediate selection receipts. These identify
//! presentation, not durable Phux work, and carry no positional revision fence.
const std = @import("std");
const model_module = @import("../model.zig");
const Model = model_module.Model;
pub const catalog = @import("catalog_targets.zig");

pub const request_name = "cockpit.tab-command";
pub const target_len = 22;
pub const request_len = 10 + target_len;
pub const receipt_len = 27;
pub const context_len = model_module.max_windows * 16;

pub const Target = struct {
    window: u8,
    window_epoch: u64,
    tab_generation: u64,
    tab_id: u32,

    pub fn resolve(self: Target, model: *const Model) ?u8 {
        const workspace = self.resolveWindow(model) orelse return null;
        if (self.tab_generation == std.math.maxInt(u64)) return null;
        if (workspace.tab_generation != self.tab_generation) return null;
        return findTab(workspace, self.tab_id);
    }

    fn resolveWindow(self: Target, model: *const Model) ?*const model_module.Workspace {
        if (!model.windowOpen(self.window)) return null;
        if (self.window_epoch == std.math.maxInt(u64)) return null;
        if (model.window_epochs[self.window] != self.window_epoch) return null;
        return model.wsAtConst(self.window);
    }

    pub fn encode(self: Target) [target_len]u8 {
        var out: [target_len]u8 = undefined;
        out[0] = 1;
        out[1] = self.window;
        std.mem.writeInt(u64, out[2..10], self.window_epoch, .little);
        std.mem.writeInt(u64, out[10..18], self.tab_generation, .little);
        std.mem.writeInt(u32, out[18..22], self.tab_id, .little);
        return out;
    }
};

fn findTab(workspace: *const model_module.Workspace, tab_id: u32) ?u8 {
    for (workspace.tab_ids[0..workspace.tab_count], 0..) |id, index| {
        if (id != 0 and id == tab_id) return @intCast(index);
    }
    return null;
}

pub fn capture(model: *const Model, window: usize, index: usize) ?Target {
    const workspace = model.wsAtConst(window) orelse return null;
    return .{
        .window = @intCast(window),
        .window_epoch = model.window_epochs[window],
        .tab_generation = workspace.tab_generation,
        .tab_id = workspace.tabId(index) orelse return null,
    };
}

pub const Request = struct { id: u64, target: union(enum) { tab: Target, catalog: catalog.Target } };

pub fn decode(bytes: []const u8) ?Request {
    if (bytes.len < 10 or bytes[0] != 1) return null;
    const id = std.mem.readInt(u64, bytes[2..10], .little);
    if (id == 0) return null;
    return switch (bytes[1]) {
        1 => .{ .id = id, .target = .{ .tab = decodeTab(bytes) orelse return null } },
        2 => .{ .id = id, .target = .{ .catalog = catalog.decode(bytes[10..]) orelse return null } },
        else => null,
    };
}

fn decodeTab(bytes: []const u8) ?Target {
    if (bytes.len != request_len) return null;
    if (bytes[10] != 1 or bytes[11] >= model_module.max_windows) return null;
    return .{
        .window = bytes[11],
        .window_epoch = std.mem.readInt(u64, bytes[12..20], .little),
        .tab_generation = std.mem.readInt(u64, bytes[20..28], .little),
        .tab_id = std.mem.readInt(u32, bytes[28..32], .little),
    };
}

pub const Reason = enum(u8) { none = 0, invalid_command = 1, stale_target = 2, unavailable = 3 };
pub const Status = enum(u8) { applied = 1, rejected = 2, accepted_pending = 3 };
pub const Receipt = struct {
    id: u64 = 0,
    reason: Reason,
    sequence: u64 = 0,
    revision: u64 = 0,
    status: Status = .applied,

    pub fn encode(self: Receipt) [receipt_len]u8 {
        var out: [receipt_len]u8 = undefined;
        out[0] = 1;
        out[1] = if (self.reason == .none) @intFromEnum(self.status) else @intFromEnum(Status.rejected);
        out[2] = @intFromEnum(self.reason);
        std.mem.writeInt(u64, out[3..11], self.id, .little);
        std.mem.writeInt(u64, out[11..19], self.sequence, .little);
        std.mem.writeInt(u64, out[19..27], self.revision, .little);
        return out;
    }
};
