//! Paste protection: which clipboard pastes ask before they reach a terminal.
//!
//! The rule is Ghostty's `clipboard-paste-protection` with its default
//! `clipboard-paste-bracketed-safe` (Surface.zig `completeClipboardPaste`): a
//! paste the receiving program will see as a paste (DEC 2004 bracketed paste
//! on) is delivered; one that would arrive as typed input asks first when it
//! carries a line break, because each line break is an Enter. A paste that
//! carries the bracketed-paste terminator `ESC [ 201 ~` asks even when
//! bracketed, since it could close the bracket early and type the rest.
//!
//! Two deliberate departures, both toward asking: a bare carriage return
//! counts as a line break (Ghostty's `isSafe` checks only `\n`, but an
//! unbracketed `\r` is the Enter key itself), and the bracketed mode of a Phux
//! terminal that cannot be read counts as off.
//!
//! Whether the terminal sits at a prompt (`projection.terminalAtPrompt`) does
//! not decide delivery: a running program with bracketed paste on (an editor,
//! an agent) handles a paste as safely as a shell does, and an unbracketed
//! receiver runs every line whether or not it is a shell. It decides what the
//! confirmation says: a shell at its prompt runs each line as a command; a
//! running program receives them as typed input.

const std = @import("std");

/// Who would receive an unbracketed multi-line paste.
pub const Receiver = enum(u8) {
    /// The terminal sits at a shell prompt: each line runs as a command.
    shell = 0,
    /// A program is running (or the prompt state is unknown): it receives
    /// the lines as typed input.
    program = 1,
};

pub const Verdict = union(enum) {
    deliver,
    confirm: Receiver,
};

const bracket_end = "\x1b[201~";

pub fn assess(text: []const u8, bracketed: bool, at_prompt: bool) Verdict {
    if (std.mem.indexOf(u8, text, bracket_end) == null) {
        if (bracketed) return .deliver;
        // Ghostty's `input.paste.isSafe`, plus the bare CR.
        if (std.mem.indexOfAny(u8, text, "\n\r") == null) return .deliver;
    }
    return .{ .confirm = if (at_prompt) .shell else .program };
}

/// The lines a paste would enter, counted the way a reader would: a trailing
/// line break ends the last line rather than opening an empty one, and
/// `\r\n` is one break.
pub fn lineCount(text: []const u8) u32 {
    var count: u32 = 0;
    var open = false;
    var index: usize = 0;
    while (index < text.len) : (index += 1) {
        const byte = text[index];
        if (byte == '\n' or byte == '\r') {
            count +|= 1;
            open = false;
            if (byte == '\r' and index + 1 < text.len and text[index + 1] == '\n') index += 1;
            continue;
        }
        open = true;
    }
    return if (open) count +| 1 else count;
}

const testing = std.testing;

test "a single line is delivered whatever the receiver" {
    try testing.expectEqual(Verdict.deliver, assess("echo hello", false, true));
    try testing.expectEqual(Verdict.deliver, assess("echo hello", false, false));
    try testing.expectEqual(Verdict.deliver, assess("", false, false));
}

test "bracketed paste makes a multi-line paste safe, as Ghostty's bracketed-safe default" {
    try testing.expectEqual(Verdict.deliver, assess("make\nmake install\n", true, true));
    try testing.expectEqual(Verdict.deliver, assess("line one\nline two", true, false));
}

test "an unbracketed line break asks, naming who would receive it" {
    try testing.expectEqual(Verdict{ .confirm = .shell }, assess("rm -rf build\nls\n", false, true));
    try testing.expectEqual(Verdict{ .confirm = .program }, assess("rm -rf build\nls\n", false, false));
    // A bare CR is the Enter key itself.
    try testing.expectEqual(Verdict{ .confirm = .shell }, assess("sudo reboot\r", false, true));
}

test "the bracket terminator asks even when the receiver brackets" {
    try testing.expectEqual(Verdict{ .confirm = .shell }, assess("a\x1b[201~rm -rf ~", true, true));
    try testing.expectEqual(Verdict{ .confirm = .program }, assess("a\x1b[201~b", true, false));
}

test "line count reads like the text" {
    try testing.expectEqual(@as(u32, 0), lineCount(""));
    try testing.expectEqual(@as(u32, 1), lineCount("one"));
    try testing.expectEqual(@as(u32, 1), lineCount("one\n"));
    try testing.expectEqual(@as(u32, 2), lineCount("one\ntwo"));
    try testing.expectEqual(@as(u32, 2), lineCount("one\r\ntwo\r\n"));
    try testing.expectEqual(@as(u32, 3), lineCount("one\n\nthree"));
}
