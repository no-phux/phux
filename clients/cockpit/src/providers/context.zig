//! Collision-free, process-local provider/host lifetimes. Never persisted or
//! derived from addresses, names or hashes. Exhaustion refuses new contexts.
const std = @import("std");

var next = std.atomic.Value(u64).init(1);

pub fn allocate() error{ContextExhausted}!u64 {
    return take(&next);
}

fn take(counter: *std.atomic.Value(u64)) error{ContextExhausted}!u64 {
    var value = counter.load(.monotonic);
    while (value != std.math.maxInt(u64)) {
        value = counter.cmpxchgWeak(value, value + 1, .monotonic, .monotonic) orelse return value;
    }
    return error.ContextExhausted;
}

test "context allocation preserves full u64 and never recycles at exhaustion" {
    var counter = std.atomic.Value(u64).init(std.math.maxInt(u64) - 1);
    try std.testing.expectEqual(std.math.maxInt(u64) - 1, try take(&counter));
    try std.testing.expectError(error.ContextExhausted, take(&counter));
    try std.testing.expectError(error.ContextExhausted, take(&counter));
    try std.testing.expect(try allocate() != try allocate());
}
