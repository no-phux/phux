//! A menu action retains the local terminal incarnation that painted it.
//! UI focus and layout positions never authorize clipboard work.
const std = @import("std");
const contract = @import("provider_contract");
const Model = @import("../model.zig").Model;

pub const command_name = "cockpit.local-clipboard";
pub const packet_len = 26;
pub const Action = enum(u8) { copy = 1, paste = 2 };

pub const Target = struct {
    action: Action,
    provider_context: u64,
    terminal_id: contract.LocalResourceId,
    generation: u64,

    pub fn capture(model: *const Model, ref: contract.TerminalRef, action: Action) ?Target {
        const id = contract.localId(ref) orelse return null;
        const owner = model.terminalOwner(ref) orelse return null;
        return .{ .action = action, .provider_context = model.provider.context_id, .terminal_id = id, .generation = owner.generation.bootstrap_id };
    }

    pub fn resolve(self: Target, model: *const Model) ?contract.TerminalRef {
        if (self.provider_context != model.provider.context_id) return null;
        const ref = contract.localTerminalRef(self.terminal_id);
        if (!model.ownerIsCurrent(contract.localReplicaOwner(ref, self.generation))) return null;
        const location = model.locateTerminal(ref) orelse return null;
        if (!model.windowOpen(location.window)) return null;
        const workspace = model.wsAtConst(location.window) orelse return null;
        if (workspace.selected_tab != location.tab) return null;
        return ref;
    }

    pub fn encode(self: Target) [packet_len]u8 {
        var bytes: [packet_len]u8 = undefined;
        bytes[0] = 1;
        bytes[1] = @intFromEnum(self.action);
        std.mem.writeInt(u64, bytes[2..10], self.provider_context, .little);
        std.mem.writeInt(u64, bytes[10..18], @intFromEnum(self.terminal_id), .little);
        std.mem.writeInt(u64, bytes[18..26], self.generation, .little);
        return bytes;
    }
};

pub fn decode(bytes: []const u8) ?Target {
    if (bytes.len != packet_len or bytes[0] != 1) return null;
    const action = std.enums.fromInt(Action, bytes[1]) orelse return null;
    return .{
        .action = action,
        .provider_context = std.mem.readInt(u64, bytes[2..10], .little),
        .terminal_id = @enumFromInt(std.mem.readInt(u64, bytes[10..18], .little)),
        .generation = std.mem.readInt(u64, bytes[18..26], .little),
    };
}
