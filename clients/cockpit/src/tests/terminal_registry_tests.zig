//! The local terminal registry under the tab/tree model.
//!
//! Detach/attach are gone — a terminal is a LEAF of a tab's tree, not the
//! occupant of a placement slot — so these were rewritten to pin the
//! invariants that actually survive: identity is stable and never reissued,
//! a hidden terminal stays live, and closing FREES its emulator instead of
//! parking a tombstone against capacity.

const std = @import("std");
const app = @import("../native_test_root.zig");
const local = @import("../providers/local/provider.zig");
const support = @import("support.zig");

const testing = std.testing;

const createDefaultSession = support.createDefaultSession;

test "the SDK PTY table covers Cockpit's bounded terminal registry" {
    try testing.expect(local.max_live_shells >= app.max_terminals);
}

test "terminal identity allocation rejects exhaustion reserved keys and duplicates" {
    const session = try createDefaultSession();
    var model = try app.initialModelWithIo(testing.allocator, testing.io, session);
    defer app.deinitModel(&model);

    model.provider.next_terminal_raw = std.math.maxInt(u64) - 1;
    try testing.expectError(error.TerminalIdentityExhausted, model.provider.createTerminal());
    model.provider.next_terminal_raw = @intFromEnum(app.LocalResourceId.terminal_1);
    try testing.expectError(error.TerminalIdentityCollision, model.provider.createTerminal());
    model.provider.next_terminal_raw = @intFromEnum(app.LocalResourceId.terminal_2) + 1;

    model.provider.next_pty_key = std.math.maxInt(u64) - 1;
    try testing.expectError(error.TerminalIdentityExhausted, model.provider.createTerminal());
    model.provider.next_pty_key = app.ptyKey(0);
    try testing.expectError(error.TerminalIdentityCollision, model.provider.createTerminal());
    model.provider.next_pty_key = app.clipboard_key;
    try testing.expectError(error.TerminalIdentityCollision, model.provider.createTerminal());
}
