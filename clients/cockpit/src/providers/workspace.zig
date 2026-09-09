//! Owned plain workspace values; no transport or engine dependencies.
const contract = @import("contract.zig");

pub const max_text_bytes = 4096;
pub const max_windows = 32;
pub const max_nodes = 512;
pub const max_terminals = 256;
pub const WindowId = [16]u8;
pub const Text = struct {
    storage: [max_text_bytes]u8 = undefined,
    len: u16 = 0,

    pub fn init(value: []const u8) !Text {
        if (value.len > max_text_bytes) return error.TextTooLong;
        var result: Text = .{};
        @memcpy(result.storage[0..value.len], value);
        result.len = @intCast(value.len);
        return result;
    }

    pub fn slice(self: *const Text) []const u8 {
        return self.storage[0..self.len];
    }
};

pub const Window = struct { id: WindowId, name: Text = .{}, root: u32 };
pub const Node = struct {
    kind: enum { leaf, horizontal, vertical },
    terminal_ref: ?contract.TerminalRef = null,
    first: u32 = 0,
    second: u32 = 0,
    ratio: f32 = 0.5,
};
pub const State = enum(u32) { unavailable, fallback, authoritative, last_good_error };
pub const Status = enum(u32) { idle, pending, confirmed, refused, unknown_outcome };
/// Slices borrow host-owned storage until the next mutable provider call.
pub const Snapshot = struct {
    revision: u64 = 0,
    session_id: u32 = 0,
    state: State = .unavailable,
    request_id: u32 = 0,
    status: Status = .idle,
    windows: []const Window = &.{},
    nodes: []const Node = &.{},
    message: []const u8 = &.{},
};
pub const CatalogTerminal = struct {
    terminal_ref: contract.TerminalRef,
    session_id: u32,
    title: Text = .{},
    cwd: Text = .{},
};
pub const Mutation = struct {
    expected_revision: u64,
    session_id: u32,
    kind: enum(u32) { add = 1, split, remove, reorder, resize, rename, remove_window },
    window_id: WindowId = @splat(0),
    terminal_ref: ?contract.TerminalRef = null,
    new_terminal_ref: ?contract.TerminalRef = null,
    name: []const u8 = &.{},
    index: u32 = 0,
    direction: enum(u32) { horizontal = 2, vertical = 3 } = .horizontal,
    ratio: f32 = 0.5,
    path_bits: u64 = 0,
    path_len: u32 = 0,
};

test "plain workspace values share contract identity and preserve bounded text" {
    const std = @import("std");
    const ref: contract.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{
        .phux = try contract.RemoteTerminalId.fromPhux(0, 7, ""),
    } };
    const node: Node = .{ .kind = .leaf, .terminal_ref = ref };
    const value: Mutation = .{ .expected_revision = 1, .session_id = 1, .kind = .add, .terminal_ref = ref };
    try std.testing.expect(node.terminal_ref.?.eql(value.terminal_ref.?));
    const input = [_]u8{'x'} ** (max_text_bytes + 1);
    const text = try Text.init(input[0..max_text_bytes]);
    try std.testing.expectEqualSlices(u8, input[0..max_text_bytes], text.slice());
    try std.testing.expectError(error.TextTooLong, Text.init(&input));
}
