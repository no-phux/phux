//! Opt-in diagnostic output for MEASURED tests. Any stderr from a test makes
//! Zig 0.16's build runner print a step-failure report (`failed command:`)
//! even for a passing run, so measurement output only prints under
//! `zig build test -Dmeasure=true` (a comptime build option; the exit code
//! stays authoritative).
const std = @import("std");
const options = @import("test_options");

/// True when the caller asked for measurement output, via -Dmeasure=true.
pub const enabled: bool = options.measure;

/// `std.debug.print`, but compiled out unless -Dmeasure=true.
pub fn print(comptime fmt: []const u8, args: anytype) void {
    if (!enabled) return;
    std.debug.print(fmt, args);
}
