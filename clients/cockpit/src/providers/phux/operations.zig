//! Fixed-capacity request correlation; no allocation after queue acceptance.
const std = @import("std");
const provider = @import("provider_contract");
pub const types = @import("operation_types.zig");

pub fn Ledger(comptime capacity: usize) type {
    return struct {
        const Self = @This();
        const Entry = struct {
            request_id: u32,
            epoch: u64,
            kind: types.Kind,
            terminal_ref: ?provider.TerminalRef,
            result: ?types.Result = null,
        };
        entries: [capacity]Entry = undefined,
        len: usize = 0,
        last_id: u32 = 0,

        pub fn nextId(self: *const Self) !u32 {
            if (self.len == capacity) return error.OperationCapacity;
            return std.math.add(u32, self.last_id, 1) catch error.RequestIdExhausted;
        }

        pub fn accepted(self: *Self, id: u32, epoch: u64, kind: types.Kind, terminal_ref: ?provider.TerminalRef) void {
            std.debug.assert(self.len < capacity);
            self.entries[self.len] = .{ .request_id = id, .epoch = epoch, .kind = kind, .terminal_ref = terminal_ref };
            self.len += 1;
            self.last_id = id;
        }

        pub fn pendingSpawns(self: *const Self) usize {
            var count: usize = 0;
            for (self.entries[0..self.len]) |entry| {
                if (entry.kind == .spawn and entry.result == null) count += 1;
            }
            return count;
        }

        pub fn complete(self: *Self, result: types.Result) !void {
            for (self.entries[0..self.len]) |*entry| {
                if (entry.request_id != result.request_id or entry.epoch != result.connection_epoch) continue;
                if (entry.kind != result.kind or entry.result != null) return error.Protocol;
                entry.result = result;
                return;
            }
            return error.Protocol;
        }

        pub fn disconnect(self: *Self, epoch: u64) void {
            for (self.entries[0..self.len]) |*entry| {
                if (entry.epoch != epoch or entry.result != null) continue;
                entry.result = .{
                    .request_id = entry.request_id,
                    .connection_epoch = epoch,
                    .kind = entry.kind,
                    .status = .unknown_outcome,
                    .terminal_ref = entry.terminal_ref,
                };
            }
        }

        pub fn take(self: *Self) ?types.Result {
            for (self.entries[0..self.len], 0..) |entry, index| {
                const result = entry.result orelse continue;
                std.mem.copyForwards(Entry, self.entries[index .. self.len - 1], self.entries[index + 1 .. self.len]);
                self.len -= 1;
                return result;
            }
            return null;
        }
    };
}

test "completed and pending operations share capacity and epoch correlation" {
    var ledger: Ledger(2) = .{};
    ledger.accepted(try ledger.nextId(), 8, .spawn, null);
    ledger.accepted(try ledger.nextId(), 8, .attach, null);
    try std.testing.expectError(error.OperationCapacity, ledger.nextId());
    try std.testing.expectError(error.Protocol, ledger.complete(.{ .request_id = 1, .connection_epoch = 9, .kind = .spawn, .status = .success }));
    try ledger.complete(.{ .request_id = 2, .connection_epoch = 8, .kind = .attach, .status = .refused });
    try std.testing.expectError(error.OperationCapacity, ledger.nextId());
    try std.testing.expectEqual(@as(u32, 2), ledger.take().?.request_id);
    ledger.disconnect(8);
    const unknown = ledger.take().?;
    try std.testing.expectEqual(types.Status.unknown_outcome, unknown.status);
    try std.testing.expectEqual(@as(u64, 8), unknown.connection_epoch);
    try std.testing.expectEqual(@as(u32, 3), try ledger.nextId());
    try std.testing.expect(ledger.take() == null);
}
