//! The minimum-contrast floor, driven by real escape sequences and read back
//! off the painted `cell_grid` through `grid.paint`, so the wire from
//! `PaintOptions` to the session is covered. On the #090b0f ground ANSI black
//! is 1.19:1 and faint blue 2.60:1. Every test that asserts the floor fired
//! first asserts the unfloored colour at `minimum_contrast = 1`.

const std = @import("std");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const config_module = @import("../config/config.zig");
const theme_module = @import("../config/theme.zig");
const support = @import("support.zig");
const measured = @import("measured.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;

/// The app's terminal ground and text (the `phux-dark` pair).
const ground = canvas.Color.rgb8(0x09, 0x0b, 0x0f);
const ink = canvas.Color.rgb8(0xf4, 0xf7, 0xfb);

fn terminalTokens() canvas.DesignTokens {
    var tokens: canvas.DesignTokens = .{};
    tokens.colors.background = ground;
    tokens.colors.text = ink;
    return tokens;
}

/// Paint one session at a given floor and hand back its lattice.
fn paintAt(session: *grid.Session, builder: *canvas.Builder, minimum_contrast: f32) !support.CellGridView {
    try grid.paint(session, builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = terminalTokens(),
        .running = true,
        .selecting = false,
        .minimum_contrast = minimum_contrast,
    });
    return support.expectCellGrid(builder.displayList());
}

/// The foreground of the cell where `marker` starts on row 0, after a paint at
/// `minimum_contrast`.
fn fgOf(session: *grid.Session, marker: []const u8, minimum_contrast: f32) !canvas.CellColor {
    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintAt(session, &builder, minimum_contrast);
    const x = view.findInRow(0, marker) orelse return error.TestExpectedCell;
    return view.foreground(x, 0) orelse error.TestExpectedCell;
}

fn expectRgb(expected: vt.color.RGB, actual: canvas.CellColor) !void {
    try testing.expectEqual(expected.r, actual.r);
    try testing.expectEqual(expected.g, actual.g);
    try testing.expectEqual(expected.b, actual.b);
}

fn ratioAgainstGround(color: canvas.CellColor) f32 {
    return theme_module.contrastRatio(
        .{ .r = color.r, .g = color.g, .b = color.b },
        .{ .r = 0x09, .g = 0x0b, .b = 0x0f },
    );
}

test "SGR 30 on the app's own ground is illegible without a floor, and lifted with one" {
    const session = try createSession(40, 4);
    defer session.destroy();
    session.feed("\x1b[30mBLACK\x1b[0m");

    // THE NEGATIVE HALF. Floor off, and the projection hands the painter
    // libghostty's own ANSI black — 1.19:1 against the ground, which is the
    // defect. If this ever comes back white, the assertion below proves
    // nothing.
    const unfloored = try fgOf(session, "BLACK", 1);
    try expectRgb(vt.color.default[0], unfloored);
    try testing.expect(ratioAgainstGround(unfloored) < 1.3);

    // Floor on at the shipped default, and the same cell is pure white.
    const floored = try fgOf(session, "BLACK", config_module.default_minimum_contrast);
    try expectRgb(.{ .r = 255, .g = 255, .b = 255 }, floored);
    try testing.expect(ratioAgainstGround(floored) >= config_module.default_minimum_contrast);
}

test "SGR 90 grey clears the shipped floor and keeps its hue" {
    const session = try createSession(40, 4);
    defer session.destroy();
    session.feed("\x1b[90mGREY\x1b[0m");

    // 3.43:1 is dim but left alone; a default above 3.43 (e.g. WCAG AA 4.5)
    // would erase a prompt's de-emphasis, and this assertion says so.
    const floored = try fgOf(session, "GREY", config_module.default_minimum_contrast);
    try expectRgb(vt.color.default[8], floored);
}

test "faint over a dim colour is lifted; faint over the default foreground is not" {
    const session = try createSession(60, 4);
    defer session.destroy();
    // Two faint runs on one row: blue, which the 50% blend drops to 2.60:1,
    // and the default foreground, which the same blend leaves at 5.03:1.
    // Faint is not the defect; faint ON SOMETHING ALREADY DIM is.
    session.feed("\x1b[2;34mDIM\x1b[0m \x1b[2mSOFT\x1b[0m");

    const dim_unfloored = try fgOf(session, "DIM", 1);
    try testing.expect(ratioAgainstGround(dim_unfloored) < config_module.default_minimum_contrast);

    const dim_floored = try fgOf(session, "DIM", config_module.default_minimum_contrast);
    try expectRgb(.{ .r = 255, .g = 255, .b = 255 }, dim_floored);

    // And the untouched half, from the SAME paint, so the floor is shown to
    // discriminate rather than to repaint the row.
    const soft_floored = try fgOf(session, "SOFT", config_module.default_minimum_contrast);
    try testing.expect(soft_floored.r != 255 or soft_floored.g != 255 or soft_floored.b != 255);
    try testing.expect(ratioAgainstGround(soft_floored) >= config_module.default_minimum_contrast);
}

test "a graphics code point is exempt from the floor" {
    const session = try createSession(40, 4);
    defer session.destroy();
    // A Powerline separator in ANSI black. Ghostty's `noMinContrast` exempts
    // this range because the glyph IS the divider between two segments and is
    // drawn in a segment's colour on purpose; raising it to white would paint
    // a white wedge through every prompt.
    session.feed("\x1b[30m\u{e0b0}\x1b[0m");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintAt(session, &builder, config_module.default_minimum_contrast);
    const fg = view.foreground(0, 0) orelse return error.TestExpectedCell;
    try expectRgb(vt.color.default[0], fg);

    // The negative half is the ORDINARY code point at the same colour in the
    // same build: without it, a projection that had stopped applying the floor
    // entirely would pass the assertion above.
    const letter = try createSession(40, 4);
    defer letter.destroy();
    letter.feed("\x1b[30mX\x1b[0m");
    const lifted = try fgOf(letter, "X", config_module.default_minimum_contrast);
    try expectRgb(.{ .r = 255, .g = 255, .b = 255 }, lifted);
}

test "the floor is measured against the cell's own background, not the terminal ground" {
    const session = try createSession(40, 4);
    defer session.destroy();
    // Bright white text on a bright-white background: 1:1, and invisible even
    // though the foreground is the most legible colour the ground could ask
    // for. A floor that only ever compared against `tokens.colors.background`
    // would score this 19.69:1 and leave it alone.
    session.feed("\x1b[97;107mHIDDEN\x1b[0m");

    const unfloored = try fgOf(session, "HIDDEN", 1);
    try expectRgb(vt.color.default[15], unfloored);

    const floored = try fgOf(session, "HIDDEN", config_module.default_minimum_contrast);
    try expectRgb(.{ .r = 0, .g = 0, .b = 0 }, floored);
}

test "inverse resolves its ink before the floor sees it" {
    const session = try createSession(40, 4);
    defer session.destroy();
    // SGR 7 with an explicit bright-white background: the ink becomes that
    // background colour and the ground becomes the default foreground. Both
    // near-white, so the pair is illegible and the floor must see the
    // POST-SWAP colours to know it.
    session.feed("\x1b[7;107mSWAP\x1b[0m");

    const unfloored = try fgOf(session, "SWAP", 1);
    try expectRgb(vt.color.default[15], unfloored);

    const floored = try fgOf(session, "SWAP", config_module.default_minimum_contrast);
    try expectRgb(.{ .r = 0, .g = 0, .b = 0 }, floored);
}

test "minimum-contrast parses, clamps, and disables" {
    // The key exists and takes a ratio.
    const on = config_module.parse("minimum-contrast = 4.5");
    try testing.expectEqual(@as(f32, 4.5), on.minimum_contrast);
    try testing.expectEqual(@as(usize, 0), on.diagnosticSlice().len);

    // 1 is Ghostty's disable value and has to survive the clamp intact, or
    // there is no way to turn the floor off.
    const off = config_module.parse("minimum-contrast = 1");
    try testing.expectEqual(config_module.min_minimum_contrast, off.minimum_contrast);

    // Out of range clamps rather than erroring, the way `font-size` does.
    const over = config_module.parse("minimum-contrast = 100");
    try testing.expectEqual(config_module.max_minimum_contrast, over.minimum_contrast);
    const under = config_module.parse("minimum-contrast = 0");
    try testing.expectEqual(config_module.min_minimum_contrast, under.minimum_contrast);

    // A NaN floor would compare false against every ratio. Refused at the
    // parse, so the renderer never has to reason about it.
    const nan = config_module.parse("minimum-contrast = nan");
    try testing.expectEqual(config_module.default_minimum_contrast, nan.minimum_contrast);
    try testing.expectEqual(@as(usize, 1), nan.diagnosticSlice().len);
    try testing.expectEqual(config_module.Diagnostic.Kind.bad_value, nan.diagnosticSlice()[0].kind);

    const bad = config_module.parse("minimum-contrast = plenty");
    try testing.expectEqual(config_module.default_minimum_contrast, bad.minimum_contrast);
    try testing.expectEqual(config_module.Diagnostic.Kind.bad_value, bad.diagnosticSlice()[0].kind);

    // Nobody wrote the key: the shipped default, and NOT Ghostty's 1. This is
    // the assertion that fails if someone "restores fidelity" by flipping the
    // default off without reading why it is 3.
    const silent = config_module.parse("");
    try testing.expectEqual(config_module.default_minimum_contrast, silent.minimum_contrast);
    try testing.expect(config_module.default_minimum_contrast > config_module.min_minimum_contrast);
}

test "the shipped default sits between the illegible ANSI colours and the legible ones" {
    // The derivation behind `default_minimum_contrast`, executed rather than
    // asserted in a comment. If libghostty's palette defaults move, or the
    // app's ground does, this fails and the number gets re-derived instead of
    // silently meaning something else.
    const bg: theme_module.Rgb = .{ .r = 0x09, .g = 0x0b, .b = 0x0f };
    const rgb = struct {
        fn of(c: vt.color.RGB) theme_module.Rgb {
            return .{ .r = c.r, .g = c.g, .b = c.b };
        }
    };

    const black = theme_module.contrastRatio(rgb.of(vt.color.default[0]), bg);
    const bright_black = theme_module.contrastRatio(rgb.of(vt.color.default[8]), bg);
    try testing.expect(black < config_module.default_minimum_contrast);
    try testing.expect(bright_black > config_module.default_minimum_contrast);

    // Nothing else in the ANSI-16 range is caught: the floor is aimed at the
    // two colours that are the terminal's own idea of "the background", not at
    // the palette.
    for (1..8) |index| {
        if (index == 0) continue;
        const ratio = theme_module.contrastRatio(rgb.of(vt.color.default[index]), bg);
        try testing.expect(ratio > config_module.default_minimum_contrast);
    }
    for (9..16) |index| {
        const ratio = theme_module.contrastRatio(rgb.of(vt.color.default[index]), bg);
        try testing.expect(ratio > config_module.default_minimum_contrast);
    }
}

// MEASURED: the projected foreground per low-contrast SGR case and floor,
// printed for `scripts/contrast-floor-check.sh`
// (`zig build test -Dmeasure=true 2>&1 | grep CONTRAST-FLOOR`).
test "MEASURED: the projected foreground for each low-contrast SGR case" {
    if (!measured.enabled) return error.SkipZigTest;

    const cases = [_]struct { label: []const u8, sgr: []const u8, marker: []const u8 }{
        .{ .label = "plain", .sgr = "", .marker = "PLAINX" },
        .{ .label = "sgr30-black", .sgr = "\x1b[30m", .marker = "BLACKK" },
        .{ .label = "sgr90-bright-black", .sgr = "\x1b[90m", .marker = "GREYYY" },
        .{ .label = "sgr2-faint", .sgr = "\x1b[2m", .marker = "FAINTT" },
        .{ .label = "sgr2-34-faint-blue", .sgr = "\x1b[2;34m", .marker = "DIMBLU" },
    };
    const floors = [_]f32{ 1, config_module.default_minimum_contrast };

    for (floors) |floor| {
        for (cases) |case| {
            const session = try createSession(40, 4);
            defer session.destroy();
            var buffer: [64]u8 = undefined;
            session.feed(try std.fmt.bufPrint(&buffer, "{s}{s}\x1b[0m", .{ case.sgr, case.marker }));

            const fg = try fgOf(session, case.marker, floor);
            measured.print(
                "CONTRAST-FLOOR floor={d} label={s} fg=#{x:0>2}{x:0>2}{x:0>2} bg=#090b0f ratio={d:.2}\n",
                .{ floor, case.label, fg.r, fg.g, fg.b, ratioAgainstGround(fg) },
            );
        }
    }
}
