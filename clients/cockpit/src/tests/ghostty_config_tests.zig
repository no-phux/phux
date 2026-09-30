//! Adopting the user's Ghostty font and colours (`config/ghostty.zig`).
//!
//! The fixture is the desktop client's own (`clients/desktop/tests/model/
//! ghostty.test.ts`), so the two clients are held to one reading of the same
//! file. Keybind, padding and cell-adjustment lines stay in it on purpose:
//! they are what a real config carries, and they must be ignored here.

const std = @import("std");
const native_sdk = @import("native_sdk");
const config = @import("../config/config.zig");
const ghostty = @import("../config/ghostty.zig");
const app = @import("../native_test_root.zig");

const canvas = native_sdk.canvas;
const testing = std.testing;

const desktop_fixture =
    \\
    \\# comments and blank lines are ignored
    \\
    \\font-family = "JetBrainsMono Nerd Font"
    \\font-family = Symbols Nerd Font
    \\font-size = 14
    \\adjust-cell-width = -8%
    \\adjust-cell-height = 2
    \\background = #071012
    \\foreground = c5d0cd
    \\cursor-color = #45d0bd
    \\selection-background = #1d3035
    \\palette = 0=#081114
    \\palette = 1=#e45f57
    \\palette = 17=#ffffff
    \\window-padding-x = 4
    \\window-padding-y = 4,6
    \\unfocused-split-opacity = 0.85
    \\keybind = global:ctrl+grave_accent=toggle_quick_terminal
    \\keybind = super+d=new_split:right
    \\keybind = super+==reset_font_size
    \\keybind = f12=set_font_size:15
    \\nonsense line without equals
    \\
;

fn rgb(r: u8, g: u8, b: u8) config.Rgb {
    return .{ .r = r, .g = g, .b = b };
}

test "the desktop fixture adopts the primary font, its size and every colour" {
    const adopted = ghostty.parse(desktop_fixture, "~/.config/ghostty/config");
    try testing.expect(adopted.found());
    // The first family is primary; the second is Ghostty's fallback.
    try testing.expectEqualStrings("JetBrainsMono Nerd Font", adopted.requested_font.slice());
    // ...and it is the design Cockpit bundles, so it maps onto that face.
    try testing.expectEqualStrings("", adopted.font_family.?.slice());
    try testing.expectEqual(@as(f32, 14), adopted.font_size.?);
    try testing.expectEqual(rgb(0x07, 0x10, 0x12), adopted.background.?);
    // A colour without `#` reads the same.
    try testing.expectEqual(rgb(0xc5, 0xd0, 0xcd), adopted.foreground.?);
    try testing.expectEqual(rgb(0x45, 0xd0, 0xbd), adopted.cursor_color.?);
    try testing.expectEqual(rgb(0x1d, 0x30, 0x35), adopted.selection_background.?);
    try testing.expectEqual(rgb(0x08, 0x11, 0x14), adopted.palette[0].?);
    try testing.expectEqual(rgb(0xe4, 0x5f, 0x57), adopted.palette[1].?);
    // Slot 17 is outside the ANSI-16 range and is ignored, not wrapped.
    for (adopted.palette[2..]) |entry| try testing.expect(entry == null);
}

test "an empty font-family resets the primary, as in Ghostty" {
    const adopted = ghostty.parse("font-family = Menlo\nfont-family =\nfont-family = Geist Mono\nfont-family = Menlo\n", "x");
    try testing.expectEqualStrings("Geist Mono", adopted.requested_font.slice());
    try testing.expectEqualStrings("Geist Mono", adopted.font_family.?.slice());
}

test "a family Cockpit does not ship is reported, never adopted" {
    const adopted = ghostty.parse("font-family = Menlo\nfont-size = 15\n", "~/.config/ghostty/config");
    try testing.expect(adopted.font_family == null);
    try testing.expectEqualStrings("Menlo", adopted.requested_font.slice());
    var buffer: [512]u8 = undefined;
    const line = ghostty.summary(&adopted, &buffer);
    try testing.expect(std.mem.indexOf(u8, line, "~/.config/ghostty/config") != null);
    try testing.expect(std.mem.indexOf(u8, line, "15 pt") != null);
    try testing.expect(std.mem.indexOf(u8, line, "Menlo is not one Cockpit ships") != null);
}

test "every bundled-family spelling maps to the bundled face" {
    for ([_][]const u8{ "JetBrainsMono Nerd Font", "JetBrains Mono", "JetBrains Mono NL Nerd Font Mono", "jetbrains-mono" }) |name| {
        try testing.expectEqualStrings("", ghostty.cockpitFamily(name).?);
    }
    try testing.expectEqualStrings("Geist Mono", ghostty.cockpitFamily("GeistMono Nerd Font").?);
    try testing.expect(ghostty.cockpitFamily("SF Mono") == null);
}

test "out-of-range and malformed values are ignored, never clamped" {
    for ([_][]const u8{ "font-size = 3", "font-size = 73", "font-size = nan", "font-size = inf", "font-size = big", "font-size =" }) |text| {
        try testing.expect(ghostty.parse(text, "x").font_size == null);
    }
    try testing.expectEqual(@as(f32, 12.5), ghostty.parse("font-size = 12.5", "x").font_size.?);
    const colours = ghostty.parse("background = rebeccapurple\nforeground = #12345\npalette = x=#ffffff\npalette = 3\n", "x");
    try testing.expect(colours.background == null);
    try testing.expect(colours.foreground == null);
    for (colours.palette) |entry| try testing.expect(entry == null);
}

test "no Ghostty config adopts nothing and says nothing" {
    const nothing: config.Inherited = .{};
    var buffer: [64]u8 = undefined;
    try testing.expectEqualStrings("", ghostty.summary(&nothing, &buffer));
    try testing.expect(!nothing.found());
}

test "theme and include directives resolve the way Desktop resolves them" {
    try testing.expectEqualStrings("Dark One", ghostty.themeChoice("light:Light One,dark:Dark One").?);
    try testing.expectEqualStrings("nord", ghostty.themeChoice("nord").?);
    try testing.expect(ghostty.themeChoice("light:Only Light") == null);
    try testing.expect(ghostty.themeChoice("") == null);

    try testing.expectEqualStrings("extra.conf", ghostty.includeTarget("config-file = ?\"extra.conf\"").?);
    try testing.expectEqualStrings("/abs/x", ghostty.includeTarget("  config-file = /abs/x  ").?);
    try testing.expect(ghostty.includeTarget("# config-file = x") == null);
    try testing.expect(ghostty.includeTarget("config-files = x") == null);

    var out: [256]u8 = undefined;
    try testing.expectEqualStrings("/h/.config/ghostty/extra", ghostty.resolveInclude("/h/.config/ghostty/config", "extra", "/h", &out).?);
    try testing.expectEqualStrings("/h/themes/x", ghostty.resolveInclude("/h/.config/ghostty/config", "~/themes/x", "/h", &out).?);
    try testing.expectEqualStrings("/etc/x", ghostty.resolveInclude("/h/.config/ghostty/config", "/etc/x", "/h", &out).?);
    try testing.expectEqualStrings("~/.config/ghostty/config", ghostty.abbreviateHome("/h/.config/ghostty/config", "/h/", &out));
    try testing.expectEqualStrings("/hx/config", ghostty.abbreviateHome("/hx/config", "/h", &out));
}

test "the locator searches, reads an explicit file, or is switched off" {
    try testing.expectEqual(ghostty.Locator.Mode.search, ghostty.Locator.fromEnv("/h", null, null).mode);
    const searched = ghostty.Locator.fromEnv("/h", null, null);
    try testing.expectEqualStrings("/h/.config", searched.config_home.slice());
    try testing.expectEqualStrings("/x", ghostty.Locator.fromEnv("/h", "/x", null).config_home.slice());
    try testing.expectEqual(ghostty.Locator.Mode.explicit, ghostty.Locator.fromEnv("/h", null, "/g").mode);
    // Empty means OFF: how hermetic runs and measurements opt out.
    try testing.expectEqual(ghostty.Locator.Mode.disabled, ghostty.Locator.fromEnv("/h", null, "").mode);
    try testing.expectEqual(ghostty.Locator.Mode.disabled, ghostty.Locator.fromEnv(null, null, null).mode);
    const off: ghostty.Locator = .{};
    try testing.expect(!ghostty.load(testing.io, &off).found());
}

test "loading layers the theme, then the file, then its includes" {
    const io = testing.io;
    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.createDirPath(io, "home/.config/ghostty/themes");
    try tmp.dir.writeFile(io, .{ .sub_path = "home/.config/ghostty/config", .data =
    \\font-size = 15
    \\background = #101010
    \\theme = light:Nope,dark:Blackwater
    \\config-file = ?"colors.conf"
    \\config-file = ?missing.conf
    \\
    });
    try tmp.dir.writeFile(io, .{ .sub_path = "home/.config/ghostty/colors.conf", .data = "foreground = #aabbcc\n" });
    try tmp.dir.writeFile(io, .{ .sub_path = "home/.config/ghostty/themes/Blackwater", .data =
    \\background = #000001
    \\foreground = #000002
    \\cursor-color = #000003
    \\palette = 2=#000004
    \\
    });
    const home = try tmp.dir.realPathFileAlloc(io, "home", testing.allocator);
    defer testing.allocator.free(home);

    const locator = ghostty.Locator.fromEnv(home, null, null);
    const adopted = ghostty.load(io, &locator);
    try testing.expectEqualStrings("~/.config/ghostty/config", adopted.source.slice());
    try testing.expectEqual(@as(f32, 15), adopted.font_size.?);
    // The file's own line overrides the theme it names...
    try testing.expectEqual(rgb(0x10, 0x10, 0x10), adopted.background.?);
    // ...an include overrides both...
    try testing.expectEqual(rgb(0xaa, 0xbb, 0xcc), adopted.foreground.?);
    // ...and what nobody overrode comes from the theme.
    try testing.expectEqual(rgb(0, 0, 3), adopted.cursor_color.?);
    try testing.expectEqual(rgb(0, 0, 4), adopted.palette[2].?);

    // An explicit file is the only candidate.
    var explicit_path: [std.fs.max_path_bytes]u8 = undefined;
    const colors = try config.joinDir(home, ".config/ghostty/colors.conf", &explicit_path);
    const only = ghostty.load(io, &ghostty.Locator.fromEnv(home, null, colors));
    try testing.expect(only.font_size == null);
    try testing.expectEqual(rgb(0xaa, 0xbb, 0xcc), only.foreground.?);
    const absent = ghostty.load(io, &ghostty.Locator.fromEnv(home, null, "/nonexistent/ghostty"));
    try testing.expect(!absent.found());
}

test "the Application Support copy is found when there is no XDG file" {
    const io = testing.io;
    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.createDirPath(io, "home/Library/Application Support/com.mitchellh.ghostty");
    try tmp.dir.writeFile(io, .{ .sub_path = "home/Library/Application Support/com.mitchellh.ghostty/config.ghostty", .data = "font-size = 16\n" });
    const home = try tmp.dir.realPathFileAlloc(io, "home", testing.allocator);
    defer testing.allocator.free(home);
    const adopted = ghostty.load(io, &ghostty.Locator.fromEnv(home, null, null));
    try testing.expectEqual(@as(f32, 16), adopted.font_size.?);
    try testing.expectEqualStrings("~/Library/Application Support/com.mitchellh.ghostty/config.ghostty", adopted.source.slice());
}

// ------------------------------------------------------------------ precedence

fn blackwater() config.Inherited {
    return ghostty.parse(desktop_fixture, "~/.config/ghostty/config");
}

test "Ghostty's values are defaults: every Cockpit key outranks them" {
    const adopted = blackwater();
    const bare = config.parseOver(adopted, "");
    try testing.expectEqual(@as(f32, 14), bare.fontSize());
    try testing.expectEqual(rgb(0xc5, 0xd0, 0xcd), bare.resolvedForeground().?);
    try testing.expectEqual(rgb(0x07, 0x10, 0x12), bare.resolvedBackground().?);
    try testing.expectEqual(rgb(0x1d, 0x30, 0x35), bare.resolvedSelectionBackground().?);
    try testing.expectEqual(rgb(0x45, 0xd0, 0xbd), bare.resolvedCursorColor().?);
    try testing.expectEqual(rgb(0x08, 0x11, 0x14), bare.resolvedPalette(0).?);
    try testing.expect(bare.resolvedPalette(5) == null);

    const explicit = config.parseOver(adopted,
        \\font-size = 16
        \\foreground = #010203
        \\cursor-color = #040506
        \\palette = 0 = #070809
    );
    try testing.expectEqual(@as(f32, 16), explicit.fontSize());
    try testing.expectEqual(rgb(1, 2, 3), explicit.resolvedForeground().?);
    try testing.expectEqual(rgb(4, 5, 6), explicit.resolvedCursorColor().?);
    try testing.expectEqual(rgb(7, 8, 9), explicit.resolvedPalette(0).?);
    // Keys the Cockpit file does not name keep Ghostty's.
    try testing.expectEqual(rgb(0x07, 0x10, 0x12), explicit.resolvedBackground().?);
    try testing.expectEqual(rgb(0xe4, 0x5f, 0x57), explicit.resolvedPalette(1).?);
    // The Ghostty layer never produces a Cockpit diagnostic.
    try testing.expectEqual(@as(usize, 0), explicit.diagnostic_count);
}

test "a Cockpit theme outranks Ghostty's colours, and clearing it restores them" {
    var themed = config.parseOver(blackwater(), "theme = nord\n");
    try testing.expectEqual(rgb(0x2e, 0x34, 0x40), themed.resolvedBackground().?);
    try testing.expectEqual(rgb(0xd8, 0xde, 0xe9), themed.resolvedForeground().?);
    try testing.expect(themed.setTheme(""));
    try testing.expectEqual(rgb(0x07, 0x10, 0x12), themed.resolvedBackground().?);
}

test "the Ghostty layer reaches the terminal tokens the painter uses" {
    var model: app.Model = .{ .provider = undefined, .config = config.parseOver(blackwater(), "") };
    const tokens = app.terminalTokens(&model);
    try testing.expectEqual(@as(f32, 14), tokens.typography.label_size);
    try testing.expectEqual(canvas.Color.rgb8(0xc5, 0xd0, 0xcd), tokens.colors.text);
    try testing.expectEqual(canvas.Color.rgb8(0x07, 0x10, 0x12), tokens.colors.background);
}

test "with no Ghostty config the terminal is 14pt in the design token colours" {
    // The 14pt default (config.zig `default_font_size`) is the size the
    // terminal grid is measured and painted at, not only a stored number.
    var model: app.Model = .{ .provider = undefined, .config = config.loadOrDefault(null) };
    try testing.expectEqual(@as(f32, 14), config.default_font_size);
    const tokens = app.terminalTokens(&model);
    try testing.expectEqual(@as(f32, 14), tokens.typography.label_size);
    // phux-cockpit-aht: the default foreground is the design token itself,
    // #f4f7fb (15.2:1 on #090b0f), never a dimmed derivative of it.
    try testing.expectEqual(canvas.Color.rgb8(0xf4, 0xf7, 0xfb), tokens.colors.text);
    try testing.expectEqual(app.cockpitTokens(&model).colors.text, tokens.colors.text);
    try testing.expectEqual(canvas.Color.rgb8(0x09, 0x0b, 0x0f), tokens.colors.background);
}
