//! Scrollback search: the engine, the wash it puts on the grid, and the
//! modal field that owns the keyboard while it is open.
//!
//! The session-level tests paint through `grid.paint` directly, with no
//! harness, because what they are pinning is the PROJECTION — which cell got
//! which background — and a bare session plus a builder is the shortest path
//! to a real display list. The app-level tests need the harness because what
//! they are pinning is ROUTING: which keys reach the pty, and what the chrome
//! says.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const expectCellGrid = support.expectCellGrid;

/// A session on the TESTING allocator, so a leaked search engine is a failed
/// test rather than a page the process happens to still own. (`support`'s own
/// helper hands out page-allocator sessions for fixtures that never free.)
fn makeSession(cols: u16, rows: u16) !*grid.Session {
    return grid.Session.create(testing.allocator, testing.io, cols, rows);
}

/// Tokens with the two search hues pinned, so the assertions below are about
/// the PROJECTION and not about whatever the default token pack happens to
/// use for `warning` and `accent`.
fn searchTokens() canvas.DesignTokens {
    var tokens: canvas.DesignTokens = .{};
    tokens.colors.warning = canvas.Color.rgb8(253, 224, 71);
    tokens.colors.accent = canvas.Color.rgb8(190, 242, 100);
    return tokens;
}

/// Paint one session into caller-owned command storage and hand back the
/// aggregated screen.
fn paintScreen(
    session: *grid.Session,
    commands: []canvas.CanvasCommand,
    builder: *canvas.Builder,
) !support.CellGridView {
    builder.* = canvas.Builder.init(commands);
    try grid.paint(session, builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = searchTokens(),
        .running = true,
        .selecting = false,
    });
    return expectCellGrid(builder.displayList());
}

/// Where the `occurrence`-th (0-based, top to bottom) instance of `needle`
/// starts on the painted screen.
fn findOccurrence(view: support.CellGridView, needle: []const u8, occurrence: usize) ?support.CellPos {
    var seen: usize = 0;
    var y: usize = 0;
    const height = view.rows();
    while (y < height) : (y += 1) {
        const x = view.findInRow(y, needle) orelse continue;
        if (seen == occurrence) return .{ .x = x, .y = y };
        seen += 1;
    }
    return null;
}

// ------------------------------------------------------------- the engine

test "search finds every match for the needle and reports how many" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\nbeta two\r\ngamma NEEDLE three\r\n");

    session.searchOpen();
    try testing.expect(session.search.open);
    // An open field with nothing typed has not searched for anything.
    try testing.expectEqual(@as(usize, 0), session.searchMatchCount());
    try testing.expectEqualStrings("", session.searchNeedle());

    try testing.expect(session.searchInput("NEEDLE"));
    try testing.expectEqualStrings("NEEDLE", session.searchNeedle());
    try testing.expectEqual(@as(usize, 2), session.searchMatchCount());
    // The engine indexes from the newest match; the chrome counts in reading
    // order, so landing on the newest of two reads as "2 of 2".
    try testing.expectEqual(@as(usize, 2), session.searchMatchOrdinal());
}

test "a needle with no match reports zero rather than silently doing nothing" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha one\r\nbeta two\r\n");

    session.searchOpen();
    try testing.expect(session.searchInput("NOSUCHTHING"));
    try testing.expectEqual(@as(usize, 0), session.searchMatchCount());
    try testing.expectEqual(@as(usize, 0), session.searchMatchOrdinal());
    // ...and stepping is a reported failure, not a no-op that looks like one.
    try testing.expect(!session.searchStep(true));
    try testing.expect(!session.searchStep(false));
}

test "backspace drops a whole scalar and re-runs the search" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\n");

    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLEX"));
    try testing.expectEqual(@as(usize, 0), session.searchMatchCount());
    try testing.expect(session.searchBackspace());
    try testing.expectEqualStrings("NEEDLE", session.searchNeedle());
    try testing.expectEqual(@as(usize, 1), session.searchMatchCount());

    // A multi-byte scalar leaves whole, never cut into an invalid needle.
    try testing.expect(session.searchInput("é"));
    try testing.expectEqualStrings("NEEDLEé", session.searchNeedle());
    try testing.expect(session.searchBackspace());
    try testing.expectEqualStrings("NEEDLE", session.searchNeedle());
}

test "control bytes never enter the needle" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.searchOpen();
    try testing.expect(!session.searchInput("\x1b"));
    try testing.expect(!session.searchInput("\x00"));
    try testing.expect(!session.searchInput("\x7f"));
    try testing.expectEqualStrings("", session.searchNeedle());
}

test "stepping scrolls a scrollback match into view" {
    const session = try makeSession(40, 5);
    defer session.destroy();
    // One match, far above the viewport, with plenty of history under it.
    session.feed("BURIEDNEEDLE\r\n");
    for (0..80) |index| {
        var line: [32]u8 = undefined;
        session.feed(std.fmt.bufPrint(&line, "filler {d}\r\n", .{index}) catch unreachable);
    }
    session.scrollToBottom();
    const bottom = session.scrollbar().offset;
    try testing.expect(bottom > 0);

    session.searchOpen();
    try testing.expect(session.searchInput("BURIEDNEEDLE"));
    try testing.expectEqual(@as(usize, 1), session.searchMatchCount());
    // Landing on the only match had to bring it on screen; nothing else in
    // this test moved the viewport.
    try testing.expect(session.scrollbar().offset < bottom);
}

test "escape restores the viewport the search started from" {
    const session = try makeSession(40, 5);
    defer session.destroy();
    session.feed("BURIEDNEEDLE\r\n");
    for (0..80) |index| {
        var line: [32]u8 = undefined;
        session.feed(std.fmt.bufPrint(&line, "filler {d}\r\n", .{index}) catch unreachable);
    }
    // Park somewhere that is NOT the live bottom, so the restore has a real
    // row to put back rather than the trivial "scroll to bottom".
    session.scrollToBottom();
    session.scrollLines(-9);
    const parked = session.scrollbar().offset;
    try testing.expect(parked > 0);

    session.searchOpen();
    try testing.expect(session.searchInput("BURIEDNEEDLE"));
    try testing.expect(session.scrollbar().offset != parked);

    session.searchClose();
    try testing.expect(!session.search.open);
    try testing.expectEqualStrings("", session.searchNeedle());
    try testing.expectEqual(parked, session.scrollbar().offset);
}

test "a search opened at the live bottom returns to the bottom, not to a stale row" {
    const session = try makeSession(40, 5);
    defer session.destroy();
    session.feed("BURIEDNEEDLE\r\n");
    for (0..40) |index| {
        var line: [32]u8 = undefined;
        session.feed(std.fmt.bufPrint(&line, "filler {d}\r\n", .{index}) catch unreachable);
    }
    session.scrollToBottom();

    session.searchOpen();
    try testing.expect(session.searchInput("BURIEDNEEDLE"));
    // More output arrives while the search is up: the row the viewport was
    // pinned at is no longer the bottom.
    for (0..20) |index| {
        var line: [32]u8 = undefined;
        session.feed(std.fmt.bufPrint(&line, "later {d}\r\n", .{index}) catch unreachable);
    }
    session.searchClose();

    const bar = session.scrollbar();
    try testing.expectEqual(bar.total, bar.offset + bar.len);
}

test "search state is per session and never leaks across terminals" {
    const alpha = try makeSession(40, 6);
    defer alpha.destroy();
    const bravo = try makeSession(40, 6);
    defer bravo.destroy();
    alpha.feed("ONLY_ALPHA_NEEDLE here\r\n");
    bravo.feed("ONLY_BRAVO_NEEDLE here\r\n");

    alpha.searchOpen();
    try testing.expect(alpha.searchInput("ONLY_ALPHA_NEEDLE"));
    try testing.expectEqual(@as(usize, 1), alpha.searchMatchCount());

    // The neighbour has no field, no needle, and no matches — and searching
    // it for the first terminal's needle finds nothing.
    try testing.expect(!bravo.search.open);
    try testing.expectEqualStrings("", bravo.searchNeedle());
    try testing.expectEqual(@as(usize, 0), bravo.searchMatchCount());
    bravo.searchOpen();
    try testing.expect(bravo.searchInput("ONLY_ALPHA_NEEDLE"));
    try testing.expectEqual(@as(usize, 0), bravo.searchMatchCount());
    // ...while the first terminal's own search is untouched by any of it.
    try testing.expectEqual(@as(usize, 1), alpha.searchMatchCount());
}

test "destroying a session with an open search leaves nothing behind" {
    // The testing allocator is the assertion: a search engine that outlived
    // its session, or one torn down against a screen that was already gone,
    // shows up here as a leak or a fault.
    const session = try makeSession(40, 6);
    session.feed("alpha NEEDLE one\r\n");
    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));
    try testing.expect(session.searchMatchCount() > 0);
    session.destroy();
}

test "a restart drops the search along with the scrollback it searched" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\n");
    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));
    try testing.expect(session.searchMatchCount() > 0);

    session.reset();
    try testing.expect(!session.search.open);
    try testing.expectEqualStrings("", session.searchNeedle());
    try testing.expectEqual(@as(usize, 0), session.searchMatchCount());
}

test "a resize rebuilds the search against the reflowed screen" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\n");
    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));
    try testing.expectEqual(@as(usize, 1), session.searchMatchCount());

    // Reflow can replace every page node the results were flattened over.
    try testing.expect(session.resize(24, 10));
    try testing.expectEqual(@as(usize, 1), session.searchMatchCount());
    var commands: [1024]canvas.CanvasCommand = undefined;
    var builder: canvas.Builder = undefined;
    _ = try paintScreen(session, &commands, &builder);
}

// ------------------------------------------------------------ the grid wash

test "matches wash the grid and the current match washes differently" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\nbeta NEEDLE two\r\n");

    var commands: [1024]canvas.CanvasCommand = undefined;
    var builder: canvas.Builder = undefined;

    // Before the search: no cell in either row paints a background of its
    // own, so anything found below came from the search.
    {
        const view = try paintScreen(session, &commands, &builder);
        const at = findOccurrence(view, "NEEDLE", 0) orelse return error.TestExpectedMatch;
        try testing.expectEqual(@as(?canvas.CellColor, null), view.background(at.x, at.y));
    }

    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));
    try testing.expectEqual(@as(usize, 2), session.searchMatchCount());

    const view = try paintScreen(session, &commands, &builder);
    const older = findOccurrence(view, "NEEDLE", 0) orelse return error.TestExpectedMatch;
    const newer = findOccurrence(view, "NEEDLE", 1) orelse return error.TestExpectedMatch;

    const older_bg = view.background(older.x, older.y) orelse return error.TestExpectedWash;
    const newer_bg = view.background(newer.x, newer.y) orelse return error.TestExpectedWash;
    // Both matched, and the one the user is standing on (the newest, which
    // is where `select(.next)` lands) is a DIFFERENT color, not merely a
    // brighter one — otherwise "which match am I on" is unanswerable on a
    // screen full of hits.
    try testing.expect(!std.meta.eql(older_bg, newer_bg));

    // The wash covers the needle and stops: the cell before the match keeps
    // the terminal's own (absent) background.
    try testing.expect(older.x > 0);
    try testing.expectEqual(@as(?canvas.CellColor, null), view.background(older.x - 1, older.y));
    // ...and it covers the WHOLE needle, not just its first cell.
    try testing.expect(view.background(older.x + "NEEDLE".len - 1, older.y) != null);
    try testing.expectEqual(@as(?canvas.CellColor, null), view.background(older.x + "NEEDLE".len, older.y));
}

test "closing the search takes every wash off the grid" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\nbeta NEEDLE two\r\n");

    var commands: [1024]canvas.CanvasCommand = undefined;
    var builder: canvas.Builder = undefined;

    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));
    {
        const view = try paintScreen(session, &commands, &builder);
        const at = findOccurrence(view, "NEEDLE", 0) orelse return error.TestExpectedMatch;
        try testing.expect(view.background(at.x, at.y) != null);
    }

    session.searchClose();
    const view = try paintScreen(session, &commands, &builder);
    const at = findOccurrence(view, "NEEDLE", 0) orelse return error.TestExpectedMatch;
    // A stale wash on a row nothing rewrote is exactly the bug that
    // clear-then-apply exists to prevent.
    try testing.expectEqual(@as(?canvas.CellColor, null), view.background(at.x, at.y));
}

test "stepping moves the current wash to another match" {
    const session = try makeSession(40, 6);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\nbeta NEEDLE two\r\n");

    var commands: [1024]canvas.CanvasCommand = undefined;
    var builder: canvas.Builder = undefined;

    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));
    const before = try paintScreen(session, &commands, &builder);
    const older = findOccurrence(before, "NEEDLE", 0) orelse return error.TestExpectedMatch;
    const newer = findOccurrence(before, "NEEDLE", 1) orelse return error.TestExpectedMatch;
    const current_wash = before.background(newer.x, newer.y) orelse return error.TestExpectedWash;

    try testing.expect(session.searchStep(false));
    try testing.expectEqual(@as(usize, 1), session.searchMatchOrdinal());

    const after = try paintScreen(session, &commands, &builder);
    // The current wash is now on the OLDER match and off the newer one.
    try testing.expectEqual(current_wash, after.background(older.x, older.y) orelse return error.TestExpectedWash);
    try testing.expect(!std.meta.eql(current_wash, after.background(newer.x, newer.y) orelse return error.TestExpectedWash));
}

// ------------------------------------------------------------- the chrome

test "a deep scrollback search is incremental, not one long stall" {
    const session = try makeSession(80, 24);
    defer session.destroy();

    // Deep enough that walking it whole on a keystroke is the stall this
    // exists to remove.
    var line: [48]u8 = undefined;
    for (0..4000) |index| {
        session.feed(std.fmt.bufPrint(&line, "row {d} NEEDLE here\r\n", .{index}) catch unreachable);
    }

    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));

    // The keystroke did BOUNDED work: the search is still owed slices rather
    // than having walked 4000 rows inline.
    try testing.expect(session.searchPending());
    const after_keystroke = session.searchMatchCount();

    // ...and it already found the matches on the ACTIVE screen, so what the
    // user is looking at is highlighted on the frame they typed into.
    try testing.expect(after_keystroke > 0);

    // Pump it the way the frame pump does, until it is done.
    // Pump the way the frame pump does, but a slice at a time so the streaming
    // is observable: a full `search_frame_slice_steps` budget finishes a
    // history this size in one call, which is the point of that number.
    var slices: usize = 0;
    while (session.searchPump(2)) : (slices += 1) {
        if (slices > 10_000) return error.TestSearchNeverCompleted;
    }
    try testing.expect(!session.searchPending());

    // It genuinely took several slices, and the completed search found more
    // than the keystroke did — the streaming actually streamed.
    try testing.expect(slices > 1);
    try testing.expect(session.searchMatchCount() > after_keystroke);
    try testing.expectEqual(@as(usize, 4000), session.searchMatchCount());
}

test "a search that completes inside one slice owes the pump nothing" {
    const session = try makeSession(80, 24);
    defer session.destroy();
    session.feed("alpha NEEDLE one\r\nbeta NEEDLE two\r\n");

    session.searchOpen();
    try testing.expect(session.searchInput("NEEDLE"));

    // Two matches on a nearly empty screen: finished inline, so the frame pump
    // is never woken for it at all.
    try testing.expect(!session.searchPending());
    try testing.expectEqual(@as(usize, 2), session.searchMatchCount());
    try testing.expect(!session.searchPump(64));
}
