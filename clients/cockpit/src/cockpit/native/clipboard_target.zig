//! A menu action retains the exact provider replica and visible placement that
//! painted it. UI focus and ref-wide provider lookup never authorize work.
const std = @import("std");
const contract = @import("provider_contract");
const Model = @import("../model.zig").Model;
const layout = @import("../layout.zig");

pub const command_name = "cockpit.clipboard";
const header_len = 72;
pub const max_packet_len = header_len + 9 + contract.RemoteResourceId.max_host_bytes;
pub const Action = enum(u8) { copy = 1, paste = 2 };

pub const Resolved = struct {
    action: Action,
    owner: contract.ReplicaOwner,
};

pub const Target = struct {
    action: Action,
    window: u8,
    window_epoch: u64,
    tab_generation: u64,
    tab_id: u32,
    provider_context: u64,
    owner: contract.ReplicaOwner,

    pub fn capture(model: *const Model, tree: *const layout.Tree, window: usize, ref: contract.TerminalRef, action: Action) ?Target {
        if (window >= model.window_epochs.len or !model.windowOpen(window)) return null;
        if (model.window_epochs[window] == std.math.maxInt(u64)) return null;
        const workspace = model.wsAtConst(window) orelse return null;
        if (workspace.selectedTreeConst() != tree or tree.find(ref) == null) return null;
        if (workspace.tab_generation == std.math.maxInt(u64)) return null;
        const tab_id = workspace.tabId(workspace.selected_tab) orelse return null;
        const source = captureOwner(model, tree, ref) orelse return null;
        return .{
            .action = action,
            .window = @intCast(window),
            .window_epoch = model.window_epochs[window],
            .tab_generation = workspace.tab_generation,
            .tab_id = tab_id,
            .provider_context = source.context,
            .owner = source.owner,
        };
    }

    pub fn resolve(self: Target, model: *const Model) ?Resolved {
        const tree = self.resolveTree(model) orelse return null;
        if (tree.find(self.owner.terminal_ref) == null) return null;
        if (!sourceIsCurrent(model, tree, self.provider_context, self.owner)) return null;
        return .{ .action = self.action, .owner = self.owner };
    }

    fn resolveTree(self: Target, model: *const Model) ?*const layout.Tree {
        if (!model.windowOpen(self.window)) return null;
        if (model.window_epochs[self.window] != self.window_epoch) return null;
        const workspace = model.wsAtConst(self.window) orelse return null;
        if (workspace.tab_generation != self.tab_generation) return null;
        if (workspace.tabId(workspace.selected_tab) != self.tab_id) return null;
        return workspace.selectedTreeConst();
    }

    pub fn encode(self: Target, bytes: *[max_packet_len]u8) []const u8 {
        bytes[0] = 2;
        bytes[1] = @intFromEnum(self.action);
        bytes[2] = self.window;
        std.mem.writeInt(u64, bytes[4..12], self.window_epoch, .little);
        std.mem.writeInt(u64, bytes[12..20], self.tab_generation, .little);
        std.mem.writeInt(u32, bytes[20..24], self.tab_id, .little);
        std.mem.writeInt(u64, bytes[24..32], @intFromEnum(self.owner.terminal_ref.provider_id), .little);
        std.mem.writeInt(u64, bytes[32..40], self.provider_context, .little);
        std.mem.writeInt(u64, bytes[40..48], self.owner.source_context, .little);
        std.mem.writeInt(u64, bytes[48..56], self.owner.generation.epoch_id, .little);
        std.mem.writeInt(u64, bytes[56..64], self.owner.generation.stream_id, .little);
        std.mem.writeInt(u64, bytes[64..72], self.owner.generation.bootstrap_id, .little);
        return encodeResource(self.owner.terminal_ref, bytes);
    }
};

pub fn decode(bytes: []const u8) ?Target {
    if (bytes.len < header_len + 8 or bytes.len > max_packet_len or bytes[0] != 2 or bytes[3] > 1) return null;
    const action = std.enums.fromInt(Action, bytes[1]) orelse return null;
    const provider_id: contract.ProviderId = @enumFromInt(std.mem.readInt(u64, bytes[24..32], .little));
    const terminal_ref = decodeResource(bytes, provider_id) orelse return null;
    return .{
        .action = action,
        .window = bytes[2],
        .window_epoch = std.mem.readInt(u64, bytes[4..12], .little),
        .tab_generation = std.mem.readInt(u64, bytes[12..20], .little),
        .tab_id = std.mem.readInt(u32, bytes[20..24], .little),
        .provider_context = std.mem.readInt(u64, bytes[32..40], .little),
        .owner = .{
            .terminal_ref = terminal_ref,
            .source_context = std.mem.readInt(u64, bytes[40..48], .little),
            .generation = .{
                .epoch_id = std.mem.readInt(u64, bytes[48..56], .little),
                .stream_id = std.mem.readInt(u64, bytes[56..64], .little),
                .bootstrap_id = std.mem.readInt(u64, bytes[64..72], .little),
            },
        },
    };
}

const CapturedOwner = struct { context: u64, owner: contract.ReplicaOwner };

fn captureOwner(model: *const Model, tree: *const layout.Tree, ref: contract.TerminalRef) ?CapturedOwner {
    if (contract.isLocal(ref)) {
        return .{ .context = model.provider.context_id, .owner = model.provider.owner(ref) orelse return null };
    }
    const remote = model.phuxForTreeConst(tree) orelse return null;
    if (remote.providerId() != ref.provider_id) return null;
    const owner = remote.owner(ref) orelse return null;
    const presentation = model.remotePaintPresentationIn(tree, ref) orelse return null;
    if (!presentation.owner.eql(owner)) return null;
    return .{ .context = remote.context_id, .owner = owner };
}

fn sourceIsCurrent(model: *const Model, tree: *const layout.Tree, context: u64, owner: contract.ReplicaOwner) bool {
    if (contract.isLocal(owner.terminal_ref)) return localSourceIsCurrent(model, context, owner);
    return remoteSourceIsCurrent(model, tree, context, owner);
}

fn localSourceIsCurrent(model: *const Model, context: u64, owner: contract.ReplicaOwner) bool {
    if (context != model.provider.context_id) return false;
    const current = model.provider.owner(owner.terminal_ref) orelse return false;
    return current.eql(owner);
}

fn remoteSourceIsCurrent(model: *const Model, tree: *const layout.Tree, context: u64, owner: contract.ReplicaOwner) bool {
    const remote = model.phuxForTreeConst(tree) orelse return false;
    if (remote.context_id != context or remote.providerId() != owner.terminal_ref.provider_id) return false;
    const current = remote.owner(owner.terminal_ref) orelse return false;
    if (!current.eql(owner)) return false;
    const presentation = model.remotePaintPresentationIn(tree, owner.terminal_ref) orelse return false;
    return presentation.owner.eql(owner);
}

fn encodeResource(ref: contract.TerminalRef, bytes: *[max_packet_len]u8) []const u8 {
    switch (ref.terminal_id) {
        .local => |id| {
            bytes[3] = 0;
            std.mem.writeInt(u64, bytes[header_len .. header_len + 8], @intFromEnum(id), .little);
            return bytes[0 .. header_len + 8];
        },
        .phux => |id| {
            bytes[3] = 1;
            std.mem.writeInt(u32, bytes[header_len .. header_len + 4], id.kind, .little);
            std.mem.writeInt(u32, bytes[header_len + 4 .. header_len + 8], id.id, .little);
            bytes[header_len + 8] = id.host_len;
            @memcpy(bytes[header_len + 9 ..][0..id.host_len], id.host());
            return bytes[0 .. header_len + 9 + @as(usize, id.host_len)];
        },
    }
}

fn decodeResource(bytes: []const u8, provider_id: contract.ProviderId) ?contract.TerminalRef {
    return switch (bytes[3]) {
        0 => if (provider_id == .local and bytes.len == header_len + 8)
            .{ .provider_id = provider_id, .terminal_id = .{ .local = @enumFromInt(std.mem.readInt(u64, bytes[header_len .. header_len + 8], .little)) } }
        else
            null,
        1 => decodeRemote(bytes, provider_id),
        else => null,
    };
}

fn decodeRemote(bytes: []const u8, provider_id: contract.ProviderId) ?contract.TerminalRef {
    if (!contract.isPhuxCoordinator(provider_id) or bytes.len < header_len + 9) return null;
    const host_len = bytes[header_len + 8];
    if (bytes.len != header_len + 9 + @as(usize, host_len)) return null;
    const id = contract.RemoteResourceId.fromPhux(
        std.mem.readInt(u32, bytes[header_len .. header_len + 4], .little),
        std.mem.readInt(u32, bytes[header_len + 4 .. header_len + 8], .little),
        bytes[header_len + 9 ..],
    ) catch return null;
    return .{ .provider_id = provider_id, .terminal_id = .{ .phux = id } };
}
