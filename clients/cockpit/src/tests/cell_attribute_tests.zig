//! Every SGR attribute the packed cell can hold, fed as real escape
//! sequences and read back off the painted lattice (never a hand-built
//! snapshot). The cursor blink state has no painted consequence, so it is
//! read from the projection's snapshot.

const std = @import("std");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;

/// Paint one session and hand back its lattice. The view borrows the
/// builder's command and cell stores, so the builder has to outlive it —
/// hence the caller-owned pointer rather than a builder made in here.
fn paintGrid(session: *grid.Session, builder: *canvas.Builder) !support.CellGridView {
    try grid.paint(session, builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
    });
    return support.expectCellGrid(builder.displayList());
}

/// The flags of the cell where `marker` starts on row 0.
fn flagsAt(view: support.CellGridView, marker: []const u8) !canvas.CellFlags {
    const x = view.findInRow(0, marker) orelse return error.TestExpectedCell;
    return view.style(x, 0) orelse error.TestExpectedCell;
}

test "bold reaches the cell as a flag" {
    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed("plain \x1b[1mBOLD\x1b[22m tail");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    try testing.expect((try flagsAt(view, "BOLD")).bold);
    // The neighbours on the same row bracket it: a flag that leaked
    // across the whole row would pass a lone positive assertion.
    try testing.expect(!(try flagsAt(view, "plain")).bold);
    try testing.expect(!(try flagsAt(view, "tail")).bold);
}

test "a weighted row occupies exactly the same cells as a plain one" {
    // Cells stay a fixed lattice regardless of weight, so a bold face can
    // never shear alignment, mouse reports or the PTY column count.
    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed("REG abcdefgh\r\n");
    session.feed("\x1b[1mBLD\x1b[22m \x1b[3mabcdefgh\x1b[23m\r\n");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    // Same columns: the two rows carry the same text after a prefix of the
    // same LENGTH, so a face-dependent advance shows up as a different
    // column.
    const plain_x = view.findInRow(0, "abcdefgh") orelse return error.TestExpectedCell;
    const weighted_x = view.findInRow(1, "abcdefgh") orelse return error.TestExpectedCell;
    try testing.expectEqual(plain_x, weighted_x);
    const bold_x = view.findInRow(1, "BLD") orelse return error.TestExpectedCell;
    try testing.expect(view.style(bold_x, 1).?.bold);
    try testing.expect(view.style(weighted_x, 1).?.italic);

    // Same cell BOX, column by column, across the whole run.
    var column: usize = 0;
    while (column < "abcdefgh".len) : (column += 1) {
        const plain_rect = view.cellRect(plain_x + column, 0);
        const weighted_rect = view.cellRect(weighted_x + column, 1);
        try testing.expectEqual(plain_rect.x, weighted_rect.x);
        try testing.expectEqual(plain_rect.width, weighted_rect.width);
        try testing.expectEqual(plain_rect.height, weighted_rect.height);
    }

    // Same BASELINE spacing: row 1 sits exactly one cell height below row
    // 0, so a taller face cannot push the rows apart.
    const first = view.cellRect(plain_x, 0);
    const second = view.cellRect(weighted_x, 1);
    try testing.expectEqual(first.height, second.y - first.y);

    // ...and the session's own metrics, which the PTY sizing pump and the
    // pointer hit test both derive from, are one number for the pane.
    try testing.expectEqual(session.measuredCell().?.width, first.width);
    try testing.expectEqual(session.measuredCell().?.height, first.height);
}

test "italic reaches the cell as a flag" {
    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed("plain \x1b[3mITALIC\x1b[23m tail");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    try testing.expect((try flagsAt(view, "ITALIC")).italic);
    try testing.expect(!(try flagsAt(view, "plain")).italic);
    try testing.expect(!(try flagsAt(view, "tail")).italic);
    // Italic is its own bit: a projection that folded the two together
    // (both are "the glyph would change if a companion face existed")
    // would still satisfy the assertions above.
    try testing.expect(!(try flagsAt(view, "ITALIC")).bold);
}

test "strikethrough reaches the cell as a flag" {
    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed("plain \x1b[9mSTRIKE\x1b[29m tail");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    try testing.expect((try flagsAt(view, "STRIKE")).strikethrough);
    try testing.expect(!(try flagsAt(view, "plain")).strikethrough);
    try testing.expect(!(try flagsAt(view, "tail")).strikethrough);
}

test "overline reaches the cell as a flag" {
    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed("plain \x1b[53mOVER\x1b[55m tail");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    try testing.expect((try flagsAt(view, "OVER")).overline);
    try testing.expect(!(try flagsAt(view, "plain")).overline);
    try testing.expect(!(try flagsAt(view, "tail")).overline);
    // Overline and strikethrough are adjacent bits in the packed flags
    // and adjacent rects in the decoration geometry; pin that the one
    // fed is the one carried.
    try testing.expect(!(try flagsAt(view, "OVER")).strikethrough);
}

test "every underline style survives the projection distinctly" {
    const session = try createSession(60, 4);
    defer session.destroy();
    // One row, six spans: the five styles a cell can carry plus an
    // unstyled one. `\x1b[21m` is the legacy double-underline spelling;
    // the colon subparameters are the modern ones.
    session.feed(
        "bare \x1b[4mSINGLE\x1b[24m " ++
            "\x1b[21mDOUBLE\x1b[24m " ++
            "\x1b[4:3mCURLY\x1b[24m " ++
            "\x1b[4:4mDOTTED\x1b[24m " ++
            "\x1b[4:5mDASHED\x1b[24m",
    );

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    // `CellUnderline.none` and the flavor are ONE field in the packed
    // cell, so "underlined" and "which underline" cannot disagree.
    try testing.expectEqual(canvas.CellUnderline.none, (try flagsAt(view, "bare")).underline);
    try testing.expectEqual(canvas.CellUnderline.single, (try flagsAt(view, "SINGLE")).underline);
    try testing.expectEqual(canvas.CellUnderline.double, (try flagsAt(view, "DOUBLE")).underline);
    try testing.expectEqual(canvas.CellUnderline.curly, (try flagsAt(view, "CURLY")).underline);
    try testing.expectEqual(canvas.CellUnderline.dotted, (try flagsAt(view, "DOTTED")).underline);
    try testing.expectEqual(canvas.CellUnderline.dashed, (try flagsAt(view, "DASHED")).underline);
}

test "the underline colour is carried, and its absence is not black" {
    const session = try createSession(40, 4);
    defer session.destroy();
    session.feed("\x1b[4mPLAINLINE\x1b[0m \x1b[4;58;2;10;200;30mTINTED\x1b[0m");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    const tinted_x = view.findInRow(0, "TINTED") orelse return error.TestExpectedCell;
    const tint = view.underlineColor(tinted_x, 0) orelse return error.TestExpectedUnderlineColor;
    try testing.expectEqual(@as(u8, 10), tint.r);
    try testing.expectEqual(@as(u8, 200), tint.g);
    try testing.expectEqual(@as(u8, 30), tint.b);

    // An underline with no SGR 58 must carry NO colour, not a zeroed
    // one: the renderer falls back to the cell foreground on the
    // `has_underline_color` bit, and a cell that always set it would
    // paint every plain underline opaque black.
    const plain_x = view.findInRow(0, "PLAINLINE") orelse return error.TestExpectedCell;
    try testing.expectEqual(canvas.CellUnderline.single, (view.style(plain_x, 0) orelse
        return error.TestExpectedCell).underline);
    try testing.expectEqual(@as(?canvas.CellColor, null), view.underlineColor(plain_x, 0));
}

test "an indexed underline colour resolves through the live palette" {
    const session = try createSession(30, 4);
    defer session.destroy();
    // SGR 58;5;N is the 256-colour spelling; it must go through the same
    // lookup the text colours do, not a second hardcoded table.
    session.feed("\x1b[4;58;5;4mINDEXED\x1b[0m");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    const x = view.findInRow(0, "INDEXED") orelse return error.TestExpectedCell;
    const tint = view.underlineColor(x, 0) orelse return error.TestExpectedUnderlineColor;
    const expected = vt.color.default[4];
    try testing.expectEqual(expected.r, tint.r);
    try testing.expectEqual(expected.g, tint.g);
    try testing.expectEqual(expected.b, tint.b);
}

test "SGR 0 clears every projected attribute at once" {
    const session = try createSession(60, 4);
    defer session.destroy();
    // Everything on, then one reset, then a marker written under it.
    session.feed("\x1b[1;3;9;53;4:3;58;2;10;200;30mLOUD\x1b[0mQUIET");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    const loud = try flagsAt(view, "LOUD");
    try testing.expect(loud.bold);
    try testing.expect(loud.italic);
    try testing.expect(loud.strikethrough);
    try testing.expect(loud.overline);
    try testing.expectEqual(canvas.CellUnderline.curly, loud.underline);
    try testing.expect(loud.has_underline_color);

    const quiet = try flagsAt(view, "QUIET");
    try testing.expect(!quiet.bold);
    try testing.expect(!quiet.italic);
    try testing.expect(!quiet.strikethrough);
    try testing.expect(!quiet.overline);
    try testing.expectEqual(canvas.CellUnderline.none, quiet.underline);
    try testing.expect(!quiet.has_underline_color);
}

test "bold-as-bright and the bold flag both apply, on different channels" {
    const session = try createSession(60, 4);
    defer session.destroy();
    // Three spans that pull the two mechanisms apart: bold over ANSI 1
    // (brightens AND flags), plain ANSI 1 (neither), and bold truecolour
    // (flags, and must NOT touch the exact colour the app named).
    session.feed("\x1b[1;31mBOLDRED\x1b[0m \x1b[31mDIMRED\x1b[0m \x1b[1;38;2;10;200;30mEXACT\x1b[0m");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    const bold_x = view.findInRow(0, "BOLDRED") orelse return error.TestExpectedCell;
    const dim_x = view.findInRow(0, "DIMRED") orelse return error.TestExpectedCell;
    const exact_x = view.findInRow(0, "EXACT") orelse return error.TestExpectedCell;

    // A bold ANSI-1 cell gets both the bright palette entry (colour) and the
    // bold flag (weight); neither reads the other.
    const bright = vt.color.default[9];
    const bold_fg = view.foreground(bold_x, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(bright.r, bold_fg.r);
    try testing.expectEqual(bright.g, bold_fg.g);
    try testing.expectEqual(bright.b, bold_fg.b);
    try testing.expect((view.style(bold_x, 0) orelse return error.TestExpectedCell).bold);

    // Plain ANSI 1 keeps the dim entry and carries no flag — so the two
    // spans differ in BOTH channels, which is what "not double-applied"
    // has to mean to be checkable.
    const dim = vt.color.default[1];
    const dim_fg = view.foreground(dim_x, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(dim.r, dim_fg.r);
    try testing.expectEqual(dim.g, dim_fg.g);
    try testing.expectEqual(dim.b, dim_fg.b);
    try testing.expect(!(view.style(dim_x, 0) orelse return error.TestExpectedCell).bold);

    // Truecolour is untouched by bold: the app named an exact colour and
    // brightening it would be a lie. The flag still rides along.
    const exact_fg = view.foreground(exact_x, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(@as(u8, 10), exact_fg.r);
    try testing.expectEqual(@as(u8, 200), exact_fg.g);
    try testing.expectEqual(@as(u8, 30), exact_fg.b);
    try testing.expect((view.style(exact_x, 0) orelse return error.TestExpectedCell).bold);
}

test "bold does not brighten the already-bright range" {
    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed("\x1b[1;91mBRIGHT\x1b[0m");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    const x = view.findInRow(0, "BRIGHT") orelse return error.TestExpectedCell;
    const expected = vt.color.default[9];
    const fg = view.foreground(x, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(expected.r, fg.r);
    try testing.expectEqual(expected.g, fg.g);
    try testing.expectEqual(expected.b, fg.b);
    try testing.expect((view.style(x, 0) orelse return error.TestExpectedCell).bold);
}

test "the cursor projects the emulator's blink state" {
    const session = try createSession(20, 4);
    defer session.destroy();
    const tokens: canvas.DesignTokens = .{};

    // DECSCUSR 2 is the steady block, 1 the blinking one. The emulator
    // resolves both into mode 12, which is what the projection reads.
    session.feed("\x1b[2 q");
    const steady = try session.snapshot(tokens, true, false);
    const steady_cursor = steady.cursor orelse return error.TestExpectedCursor;
    try testing.expect(!steady_cursor.blinking);

    session.feed("\x1b[1 q");
    const blinking = try session.snapshot(tokens, true, false);
    const blinking_cursor = blinking.cursor orelse return error.TestExpectedCursor;
    try testing.expect(blinking_cursor.blinking);
    // The shape is unchanged by the blink bit: both spellings are blocks.
    try testing.expectEqual(canvas.TerminalCursorShape.block, blinking_cursor.shape);
}

test "an emulator-requested hollow block stays distinct from the focus cue" {
    const session = try createSession(20, 4);
    defer session.destroy();
    const tokens: canvas.DesignTokens = .{};

    // Ghostty's hollow block is not a DECSCUSR value; it arrives as the
    // configured default shape.
    session.term.setDefaultCursorStyle(.block_hollow);
    const snap = try session.snapshot(tokens, true, false);
    const cursor = snap.cursor orelse return error.TestExpectedCursor;
    try testing.expectEqual(canvas.TerminalCursorShape.block_hollow, cursor.shape);

    // And it outlines even while this terminal owns the keyboard — the
    // whole point of keeping the two reasons independent. A focused live
    // terminal with a solid shape fills; this one does not.
    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = tokens,
        .running = true,
        .focused = true,
        .selecting = false,
    });
    try support.expectCursorPaintKind(builder.displayList(), .hollow);

    session.term.setDefaultCursorStyle(.block);
    var solid_commands: [512]canvas.CanvasCommand = undefined;
    var solid_builder = canvas.Builder.init(&solid_commands);
    try grid.paint(session, &solid_builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = tokens,
        .running = true,
        .focused = true,
        .selecting = false,
    });
    try support.expectCursorPaintKind(solid_builder.displayList(), .filled);
}

test "a cursor on a double-width glyph covers both columns" {
    const session = try createSession(20, 4);
    defer session.destroy();
    const tokens: canvas.DesignTokens = .{};

    // Narrow first: the default state has to be the false one, or the
    // wide assertions below would pass on a hardcoded true.
    session.feed("ab\x1b[H");
    const narrow = try session.snapshot(tokens, true, false);
    const narrow_cursor = narrow.cursor orelse return error.TestExpectedCursor;
    try testing.expectEqual(@as(u16, 0), narrow_cursor.x);
    try testing.expect(!narrow_cursor.wide);

    // A wide glyph at the origin, with the cursor parked ON it.
    session.feed("\x1b[2J\x1b[H\u{4e16}\u{754c}\x1b[H");
    const primary = try session.snapshot(tokens, true, false);
    const primary_cursor = primary.cursor orelse return error.TestExpectedCursor;
    try testing.expectEqual(@as(u16, 0), primary_cursor.x);
    try testing.expect(primary_cursor.wide);

    // Column 1 is that glyph's SPACER TAIL. The projection walks the
    // cursor back onto the primary, and it still spans two columns —
    // the half that would otherwise paint a one-column cursor over the
    // blank half of a CJK character.
    session.feed("\x1b[1;2H");
    const tail = try session.snapshot(tokens, true, false);
    const tail_cursor = tail.cursor orelse return error.TestExpectedCursor;
    try testing.expectEqual(@as(u16, 0), tail_cursor.x);
    try testing.expect(tail_cursor.wide);
}

test "attributes ride the wide cell, not just its spacer" {
    const session = try createSession(20, 4);
    defer session.destroy();
    session.feed("\x1b[4:3;58;2;10;200;30;1m\u{4e16}\x1b[0m");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const view = try paintGrid(session, &builder);

    // The primary column carries the ink and every attribute; the
    // renderer widens its decorations from the `wide` bit rather than
    // reading the spacer.
    const primary = view.style(0, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(canvas.CellWidth.wide, primary.width);
    try testing.expectEqual(canvas.CellUnderline.curly, primary.underline);
    try testing.expect(primary.bold);
    try testing.expect(primary.has_underline_color);

    const spacer = view.style(1, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(canvas.CellWidth.spacer, spacer.width);
}
