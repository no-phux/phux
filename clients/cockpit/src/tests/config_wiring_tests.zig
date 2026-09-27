//! The config is only real when it CHANGES something. `config_tests.zig`
//! proves the parser; this file proves each knob reaches the pixels, the
//! emulator, or the geometry it names.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

// Every test below uses `font-size = 26`, never the default 13, and that is
// load-bearing rather than arbitrary. The bug they guard was a pair of
// hardcoded metric defaults, `cell_width = 8` and `cell_height = 18`. The SDK
// derives an unmeasured cell as `round(font_size * 0.6)` by
// `round(font_size * 1.4)`, and at font-size 13 that is round(7.8) = 8 by
// round(18.2) = 18 — the wrong values and the right ones are the same
// numbers. A test written at the default font cannot fail no matter how
// broken the metrics path is, which is precisely why this survived to ship.
// At 26 the mono cell is 15.6 x 36 and the two states are far apart.

// The `font-family` knob was parsed, validated and warned about, yet the grid
// tokens never read it: every choice measured and painted the bundled Nerd
// Font, so choosing Geist Mono changed nothing on screen.
test "the font-family knob selects the face the terminal grids measure and paint" {
    var model: app.Model = .{ .provider = undefined, .config = app.parseConfig("font-family = Geist Mono") };
    const geist = app.terminalTokens(&model);
    try testing.expectEqual(canvas.default_mono_font_id, geist.typography.mono_font_id);
    // Geist has no registered companions. Zero asks the renderers for their
    // shared synthesis instead of mixing JetBrains bold with Geist regular.
    try testing.expectEqual(@as(canvas.FontId, 0), geist.typography.mono_bold_font_id);
    // Unset and unsupported names both keep the bundled family and its
    // registered companions; the unsupported one is only a config warning.
    for ([_][]const u8{ "", "font-family = Comic Mono" }) |text| {
        var fallback: app.Model = .{ .provider = undefined, .config = app.parseConfig(text) };
        const tokens = app.terminalTokens(&fallback);
        try testing.expectEqual(canvas.min_registered_font_id, tokens.typography.mono_font_id);
        try testing.expectEqual(@as(canvas.FontId, canvas.min_registered_font_id + 1), tokens.typography.mono_bold_font_id);
    }
}

test "font sizing clamps at both ends without stranding the key" {
    var model: app.Model = .{ .provider = undefined, .config = app.parseConfig("font-size = 71") };
    try testing.expect(model.stepFontSize(1));
    try testing.expectEqual(@as(f32, 72), model.fontSize());
    // At the ceiling the step is refused rather than banked, so ONE step back
    // moves the size instead of unwinding invisible headroom first.
    try testing.expect(!model.stepFontSize(1));
    try testing.expect(!model.stepFontSize(1));
    try testing.expect(model.stepFontSize(-1));
    try testing.expectEqual(@as(f32, 71), model.fontSize());
    // Back at the configured size the offset is genuinely zero, so cmd+0 has
    // nothing to undo and says so.
    try testing.expect(!model.resetFontSize());
    try testing.expect(model.stepFontSize(-10));
    try testing.expectEqual(@as(f32, 61), model.fontSize());
    try testing.expect(model.resetFontSize());
    try testing.expectEqual(@as(f32, 71), model.fontSize());
}

test "background foreground and selection colors reach the terminal tokens" {
    var model: app.Model = .{
        .provider = undefined,
        .config = app.parseConfig(
            \\background = #102030
            \\foreground = #a0b0c0
            \\selection-background = #ff0000
        ),
    };
    const tokens = app.terminalTokens(&model);
    try testing.expectEqual(canvas.Color.rgb8(0x10, 0x20, 0x30), tokens.colors.background);
    try testing.expectEqual(canvas.Color.rgb8(0xa0, 0xb0, 0xc0), tokens.colors.text);
    // The selection wash reads `accent` (see terminal/palette.zig).
    try testing.expectEqual(canvas.Color.rgb8(0xff, 0, 0), tokens.colors.accent);

    // An unset color leaves the app's own register untouched rather than
    // resolving to black.
    var bare: app.Model = .{ .provider = undefined };
    const default_tokens = app.terminalTokens(&bare);
    try testing.expectEqual(app.cockpitTokens(&bare).colors.background, default_tokens.colors.background);
}

test "scrollback-limit reaches the emulator instead of being stored and ignored" {
    // The regression this pins: `scrollback_bytes` parsed and stored, while
    // the session was built from a comptime const that merely HAPPENED to
    // equal the config default. Anyone setting the knob got the default and
    // no indication of it.
    const gpa = testing.allocator;
    const small = try grid.Session.createWithScrollback(gpa, testing.io, 80, 24, 1024 * 1024);
    defer small.destroy();
    const large = try grid.Session.createWithScrollback(gpa, testing.io, 80, 24, 64 * 1024 * 1024);
    defer large.destroy();

    try testing.expect(small.term.screens.active.pages.maxSize() != large.term.screens.active.pages.maxSize());
    try testing.expect(small.term.screens.active.pages.maxSize() < large.term.screens.active.pages.maxSize());
}

test "window-padding moves the content rect and the header band" {
    var tight: app.Model = .{ .provider = undefined, .config = app.parseConfig("window-padding = 0") };
    var loose: app.Model = .{ .provider = undefined, .config = app.parseConfig("window-padding = 24") };
    const size = geometry.SizeF.init(980, 640);
    const tight_chrome = app.workspaceChrome(&tight, size);
    const loose_chrome = app.workspaceChrome(&loose, size);

    try testing.expectEqual(@as(f32, 0), tight_chrome.header.x);
    try testing.expectEqual(@as(f32, 24), loose_chrome.header.x);
    try testing.expect(loose_chrome.content.width < tight_chrome.content.width);
    try testing.expectEqual(@as(f32, 980), tight_chrome.content.width);
}

test "one config diagnostic names the problem as well as the line" {
    // With a single problem there is room for what it was, so the band says it
    // rather than making the user go and look. A `missing_separator` has no key
    // to quote and must not print an empty pair of quotes.
    var model: app.Model = .{ .provider = undefined, .config = app.parseConfig("font-familly = Comic Mono") };
    var storage: [app.config_notice_bytes]u8 = undefined;
    try testing.expectEqualStrings(
        "Config line 1: unknown setting 'font-familly'",
        app.configNoticeLine(&model, &storage),
    );

    model.config = app.parseConfig("cursor-style\n");
    try testing.expectEqualStrings(
        "Config line 1: no '=' on this line",
        app.configNoticeLine(&model, &storage),
    );

    // A clean config says nothing at all, and no band exists to say it in.
    model.config = app.parseConfig("font-size = 14");
    try testing.expectEqualStrings("", app.configNoticeLine(&model, &storage));
    try testing.expect(!app.configNoticeRevealed(&model));
    try testing.expectEqual(
        @as(f32, 0),
        app.workspaceChrome(&model, geometry.SizeF.init(980, 640)).notice.height,
    );
}

test "tab-placement from the config file selects the side rail" {
    try testing.expectEqual(app.ConfigTabPlacement.side, app.parseConfig("tab-placement = sidebar").tab_placement);
    try testing.expectEqual(app.ConfigTabPlacement.top, app.parseConfig("").tab_placement);
}

test "a missing config file is a silent no-op with defaults" {
    // No bytes at all is the NORMAL case, not an error.
    const defaults = app.loadConfigOrDefault(null);
    try testing.expectEqual(@as(usize, 0), defaults.diagnostic_count);
    const pristine: app.Config = .{};
    try testing.expectEqual(pristine.font_size, defaults.font_size);
    try testing.expectEqual(pristine.window_padding, defaults.window_padding);

    // And an unresolvable directory (no HOME) yields no path rather than an
    // error the caller has to decide what to do about.
    var dir_storage: [512]u8 = undefined;
    var path_storage: [512]u8 = undefined;
    try testing.expectEqual(
        @as(?[]const u8, null),
        app.resolveConfigPath(.{}, null, &dir_storage, &path_storage),
    );
}

test "the config path is the app-dirs config directory, and the env override wins" {
    var dir_storage: [512]u8 = undefined;
    var path_storage: [512]u8 = undefined;
    const explicit = app.resolveConfigPath(
        .{ .home = "/Users/alice" },
        "/tmp/somewhere/else",
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedPath;
    try testing.expectEqualStrings("/tmp/somewhere/else", explicit);

    const resolved = app.resolveConfigPath(
        .{ .home = "/Users/alice" },
        null,
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedPath;
    try testing.expect(std.mem.startsWith(u8, resolved, "/Users/alice/"));
    try testing.expect(std.mem.endsWith(u8, resolved, "/config"));
}

test "Phux startup precedence treats empty environment values as unset" {
    const parsed = app.parseConfig(
        "phux-socket = /tmp/from-config.sock\n" ++
            "phux-session = config-session\n",
    );
    const overridden = app.resolvePhuxConfig(parsed, .{
        .socket = "/tmp/from-environment.sock",
        .session = "environment-session",
        .runtime_dir = "/run/user/501",
        .uid = "501",
    });
    try testing.expectEqualStrings("/tmp/from-environment.sock", overridden.phux_socket.slice());
    try testing.expectEqualStrings("environment-session", overridden.phux_session.slice());
    try testing.expectEqual(.environment, overridden.phux_socket_source);
    try testing.expectEqual(.environment, overridden.phux_session_source);

    const empty_environment = app.resolvePhuxConfig(parsed, .{
        .socket = "",
        .session = "",
        .runtime_dir = "/run/user/501",
        .uid = "501",
    });
    try testing.expectEqualStrings("/tmp/from-config.sock", empty_environment.phux_socket.slice());
    try testing.expectEqualStrings("config-session", empty_environment.phux_session.slice());
    try testing.expectEqual(.config, empty_environment.phux_socket_source);
    try testing.expectEqual(.config, empty_environment.phux_session_source);

    // A malformed debug override follows the other environment conventions:
    // it cannot replace a safe config value.
    const invalid_environment = app.resolvePhuxConfig(parsed, .{
        .socket = "relative.sock",
        .session = "bad\xffname",
    });
    try testing.expectEqualStrings("/tmp/from-config.sock", invalid_environment.phux_socket.slice());
    try testing.expectEqualStrings("config-session", invalid_environment.phux_session.slice());
}

test "Phux startup resolves a bounded local default without naming a session" {
    const xdg = app.resolvePhuxConfig(.{}, .{ .runtime_dir = "/run/user/501", .uid = "501" });
    try testing.expectEqualStrings("/run/user/501/phux/phux.sock", xdg.phux_socket.slice());
    try testing.expectEqualStrings("", xdg.phux_session.slice());
    try testing.expectEqual(.default, xdg.phux_socket_source);
    try testing.expectEqual(.default, xdg.phux_session_source);

    // Empty XDG_RUNTIME_DIR is unset, not the filesystem root.
    const fallback = app.resolvePhuxConfig(.{}, .{ .runtime_dir = "", .uid = "501" });
    try testing.expectEqualStrings("/tmp/phux-501/phux.sock", fallback.phux_socket.slice());
    try testing.expect(!std.mem.eql(u8, "/", fallback.phux_socket.slice()));

    const invalid_config = app.resolvePhuxConfig(
        app.parseConfig("phux-socket = relative.sock\nphux-session = bad\xffname"),
        .{ .uid = "501" },
    );
    try testing.expectEqualStrings("/tmp/phux-501/phux.sock", invalid_config.phux_socket.slice());
    try testing.expectEqualStrings("", invalid_config.phux_session.slice());
    try testing.expectEqual(@as(usize, 2), invalid_config.diagnostic_count);
}

test "Phux provider construction owns resolved socket and session bytes" {
    if (comptime !app.phux_enabled) return error.SkipZigTest;

    var resolved = app.resolvePhuxConfig(
        app.parseConfig(
            "phux-socket = /tmp/owned-at-construction.sock\n" ++
                "phux-session = owned-session\n",
        ),
        .{},
    );
    const provider = (try app.createPhuxProviderFromConfig(testing.allocator, testing.io, &resolved)) orelse
        return error.TestExpectedPhuxProvider;
    defer provider.destroy();

    try resolved.phux_socket.set("/tmp/mutated-after-construction.sock");
    try resolved.phux_session.set("mutated-session");
    try testing.expectEqualStrings("/tmp/owned-at-construction.sock", app.configuredPhuxSocket(provider));
    try testing.expectEqualStrings("owned-session", app.configuredPhuxSession(provider).?);
}
