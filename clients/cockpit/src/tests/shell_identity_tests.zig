//! What the child process tells us about itself, and what we do with it.
//!
//! Three OSC effects used to be wired to null in `installStreamEffects`, so
//! every tab read "Terminal N", nothing could inherit a directory, and a bell
//! in a hidden pane vanished. These pin the wiring, the OSC 7 URL decode
//! (libghostty-vt stores that payload VERBATIM and leaves decoding to the
//! embedder), and the shell quoting that carries a directory into a spawn
//! without carrying an injection with it.

const std = @import("std");
const builtin = @import("builtin");
const app = @import("../native_test_root.zig");
const session_module = @import("../terminal/session.zig");
const local = @import("../providers/local/provider.zig");
const support = @import("support.zig");

const testing = std.testing;
const createSession = support.createSession;

// ------------------------------------------------------------ OSC 0/2 title

test "OSC 2 title reaches the pane, and absence reads as unreported" {
    const session = try createSession(20, 4);
    defer session.destroy();
    // Nothing reported yet. Empty is "not reported", which is what lets the
    // chrome pick its own fallback instead of painting a blank tab.
    try testing.expectEqualStrings("", session.title());

    session.feed("\x1b]2;phux — src\x07");
    try testing.expectEqualStrings("phux — src", session.title());

    // The latest report wins outright; titles do not accumulate.
    session.feed("\x1b]0;build\x07");
    try testing.expectEqualStrings("build", session.title());
}

test "an oversized title truncates at a UTF-8 scalar boundary" {
    const session = try createSession(20, 4);
    defer session.destroy();
    // 100 three-byte scalars: the 256-byte ceiling lands INSIDE one, so a
    // naive @memcpy would leave a cut sequence at the end.
    const report = "\x1b]2;" ++ ("\u{20AC}" ** 100) ++ "\x07";
    session.feed(report);

    const kept = session.title();
    try testing.expect(kept.len <= session_module.max_title_bytes);
    // 85 whole scalars is 255 bytes — one short of the ceiling, which is the
    // proof the boundary walk-back actually fired.
    try testing.expectEqual(@as(usize, 255), kept.len);
    try testing.expect(std.unicode.utf8ValidateSlice(kept));
    try testing.expectEqual(@as(usize, 85), try std.unicode.utf8CountCodepoints(kept));
}

// -------------------------------------------------------------- OSC 7 pwd

test "OSC 7 file URLs decode to a plain absolute path" {
    const session = try createSession(20, 4);
    defer session.destroy();
    try testing.expectEqualStrings("", session.pwd());

    // The common shape: a host component, then a percent-encoded path.
    session.feed("\x1b]7;file://somehost/Users/phall/my%20dir\x1b\\");
    try testing.expectEqualStrings("/Users/phall/my dir", session.pwd());

    // The empty-authority shape (`file:///tmp`) is the same code path.
    session.feed("\x1b]7;file:///tmp\x1b\\");
    try testing.expectEqualStrings("/tmp", session.pwd());

    // A bare path with no scheme at all is taken verbatim.
    session.feed("\x1b]7;/var/log\x1b\\");
    try testing.expectEqualStrings("/var/log", session.pwd());
}

test "OSC 7 percent-decoding survives the bytes a shell escapes" {
    const session = try createSession(20, 4);
    defer session.destroy();
    // `string escape --style=url` and friends escape exactly these.
    session.feed("\x1b]7;file://host/tmp/a%27b%20c%24d%3Be\x1b\\");
    try testing.expectEqualStrings("/tmp/a'b c$d;e", session.pwd());
}

test "an undecodable OSC 7 clears the pwd rather than keeping a stale one" {
    const session = try createSession(20, 4);
    defer session.destroy();

    const refused = [_][]const u8{
        // Not a filesystem scheme.
        "\x1b]7;http://host/tmp\x1b\\",
        // An authority with no path at all.
        "\x1b]7;file://host\x1b\\",
        // Relative: relative to a directory we do not know.
        "\x1b]7;tmp/relative\x1b\\",
        // A truncated escape, and one that decodes to NUL — either would cut
        // the path at the C boundary into a DIFFERENT directory.
        "\x1b]7;file://host/tmp/%2\x1b\\",
        "\x1b]7;file://host/tmp/%zz\x1b\\",
        "\x1b]7;file://host/tmp/a%00b\x1b\\",
    };
    for (refused) |report| {
        session.feed("\x1b]7;file:///tmp\x1b\\");
        try testing.expectEqualStrings("/tmp", session.pwd());
        session.feed(report);
        try testing.expectEqualStrings("", session.pwd());
    }
}

// ------------------------------------------------------------------- bell

test "BEL latches until the app acknowledges it" {
    const session = try createSession(20, 4);
    defer session.destroy();
    try testing.expect(!session.bell_rung);

    session.feed("done\x07");
    try testing.expect(session.bell_rung);
    // Repeated bells collapse into the one latch — an attention marker is a
    // boolean, not a counter.
    session.feed("\x07\x07");
    try testing.expect(session.bell_rung);

    try testing.expect(session.takeBell());
    try testing.expect(!session.bell_rung);
    try testing.expect(!session.takeBell());
}

// ------------------------------------------------------------- OSC 133

test "OSC 133 prompt marks answer atPrompt" {
    const session = try createSession(20, 4);
    defer session.destroy();
    // No shell integration yet: the honest unknown is false.
    try testing.expect(!session.atPrompt());

    session.feed("\x1b]133;P\x07$ ");
    try testing.expect(session.atPrompt());

    // Typing at the prompt is still the prompt.
    session.feed("\x1b]133;B\x07ls");
    try testing.expect(session.atPrompt());

    // Output on a line the prompt never touched is not.
    session.feed("\x1b]133;C\x07\r\noutput\r\nmore");
    try testing.expect(!session.atPrompt());
}

// -------------------------------------------------- reset drops identity

test "reset drops the dead session's title, pwd, and bell" {
    const session = try createSession(20, 4);
    defer session.destroy();
    session.feed("\x1b]2;old shell\x07\x1b]7;file:///tmp\x1b\\\x07");
    try testing.expectEqualStrings("old shell", session.title());
    try testing.expectEqualStrings("/tmp", session.pwd());
    try testing.expect(session.bell_rung);

    session.reset();
    try testing.expectEqualStrings("", session.title());
    try testing.expectEqualStrings("", session.pwd());
    try testing.expect(!session.bell_rung);

    // The rebuilt stream is still wired: a restarted shell reports again.
    session.feed("\x1b]2;new shell\x07");
    try testing.expectEqualStrings("new shell", session.title());
}

// ----------------------------------------------------- the Pane contract

test "Pane surfaces title, pwd, and bell from its session" {
    const session = try createSession(20, 4);
    const provider = try local.LocalProvider.create(testing.allocator, session);
    defer provider.destroy();
    const pane = provider.slot(0);

    try testing.expectEqualStrings("", pane.title());
    try testing.expectEqualStrings("", pane.pwd());
    try testing.expect(!pane.bellRung());
    try testing.expect(!pane.atPrompt());

    pane.session.feed("\x1b]2;zsh\x07\x1b]7;file://host/Users/phall\x1b\\\x07\x1b]133;P\x07");
    const readonly: *const app.Pane = pane;
    try testing.expectEqualStrings("zsh", readonly.title());
    try testing.expectEqualStrings("/Users/phall", readonly.pwd());
    try testing.expect(readonly.bellRung());
    try testing.expect(readonly.atPrompt());

    readonly.clearBell();
    try testing.expect(!readonly.bellRung());
}

// ------------------------------------------- working-directory inheritance

/// Recover the directory out of a generated `cd '<word>' ...` command by
/// undoing POSIX single-quoting. Round-tripping is the real proof the quoting
/// is safe: if any byte of the path could escape the quoted word, the word
/// would end early and this would not reproduce the path.
fn unquoteCd(command: []const u8, out: []u8) ![]const u8 {
    const prefix = "cd '";
    try testing.expect(std.mem.startsWith(u8, command, prefix));
    var index: usize = prefix.len;
    var written: usize = 0;
    while (true) {
        if (index >= command.len) return error.TestUnterminatedQuotedWord;
        if (command[index] == '\'') {
            // Either the `'\''` escape (close, literal quote, reopen) or the
            // quote that ends the word.
            if (std.mem.startsWith(u8, command[index..], "'\\''")) {
                out[written] = '\'';
                written += 1;
                index += 4;
                continue;
            }
            break;
        }
        out[written] = command[index];
        written += 1;
        index += 1;
    }
    // Everything after the word is the fixed tail — nothing from the path
    // leaked past the closing quote into command position.
    try testing.expectEqualStrings(
        "' 2>/dev/null || cd \"$HOME\"; exec /bin/zsh -i",
        command[index..],
    );
    return out[0..written];
}

test "paneArgvIn carries hostile directory names without breaking the command" {
    // The generated argv's SHAPE is per-platform (login shell on macOS, bare
    // `sh -c` elsewhere); the quoting under test is not, but the assertions
    // below name the macOS shape, so they run where that shape is real.
    if (builtin.os.tag != .macos) return error.SkipZigTest;
    const hostile = [_][]const u8{
        "/Users/phall/plain",
        "/Users/phall/with space",
        "/Users/phall/it's here",
        "/Users/phall/$(touch pwned)",
        "/Users/phall/`id`",
        "/Users/phall/\"double\"",
        "/Users/phall/semi; rm -rf /",
        "/Users/phall/back\\slash",
        "/Users/phall/new\nline",
        "/Users/phall/pipe|amp&",
        "/Users/phall/'; id; '",
    };
    var storage: local.CwdArgv = .{};
    var recovered: [local.max_cwd_command_bytes]u8 = undefined;
    for (hostile) |cwd| {
        const argv = local.paneArgvIn(cwd, &storage);
        try testing.expectEqual(@as(usize, 4), argv.len);
        try testing.expectEqualStrings("/bin/zsh", argv[0]);
        try testing.expectEqualStrings("-l", argv[1]);
        try testing.expectEqualStrings("-c", argv[2]);
        try testing.expectEqualStrings(cwd, try unquoteCd(argv[3], &recovered));
    }
}

test "paneArgvIn stays inside the SDK's argv byte budget" {
    var storage: local.CwdArgv = .{};
    // The worst case is a path of nothing but quotes: four output bytes each.
    var quotes: [200]u8 = undefined;
    quotes[0] = '/';
    @memset(quotes[1..], '\'');
    const argv = local.paneArgvIn(&quotes, &storage);
    var total: usize = 0;
    for (argv) |arg| total += arg.len;
    // 2048 is `max_effect_argv_bytes`; past it `ptySpawn` refuses the whole
    // spawn and the pane never opens.
    try testing.expect(total < 2048);
    try testing.expect(argv.len <= 16); // max_effect_argv
}

test "paneArgvIn falls back to the plain argv when a cwd is unusable" {
    var storage: local.CwdArgv = .{};
    const unusable = [_][]const u8{
        // Never reported.
        "",
        // Relative — meaningless without knowing where from.
        "relative/path",
        // An embedded NUL: the SDK rejects the spawn outright, and the C
        // boundary would truncate it into a different directory.
        "/Users/phall/na\x00me",
    };
    for (unusable) |cwd| {
        try testing.expectEqualSlices(
            []const u8,
            local.paneArgv(0),
            local.paneArgvIn(cwd, &storage),
        );
    }

    // Too long to quote inside the command budget: same graceful fallback,
    // which is exactly the first terminal's behavior.
    var huge: [local.max_cwd_command_bytes]u8 = undefined;
    huge[0] = '/';
    @memset(huge[1..], 'a');
    try testing.expectEqualSlices(
        []const u8,
        local.paneArgv(0),
        local.paneArgvIn(&huge, &storage),
    );
}

test "a pane's reported pwd is what the next terminal starts in" {
    if (builtin.os.tag != .macos) return error.SkipZigTest;
    const session = try createSession(20, 4);
    const provider = try local.LocalProvider.create(testing.allocator, session);
    defer provider.destroy();
    const focused = provider.slot(0);
    focused.session.feed("\x1b]7;file://host/Users/phall/work%20space\x1b\\");

    // The whole point of the two pieces together: OSC 7 in, spawnable argv
    // out, with no step in between that the app has to invent.
    var storage: local.CwdArgv = .{};
    const argv = local.paneArgvIn(focused.pwd(), &storage);
    var recovered: [local.max_cwd_command_bytes]u8 = undefined;
    try testing.expectEqualStrings(
        "/Users/phall/work space",
        try unquoteCd(argv[3], &recovered),
    );
}
