//! OSC 8 hyperlinks, and the hover underline that advertises a link.
//!
//! Every test here starts from the ESCAPE SEQUENCE a program actually writes,
//! never from a hand-built link: OSC 8 is parsed by libghostty-vt and the
//! whole point of this work is that the parse reaches the pointer and the
//! painted cell. A hand-built fixture would pass with the plumbing deleted.
//!
//! The refusals carry more weight than the resolutions. OSC 8 is the one
//! channel where remote output picks BOTH what the user reads and where the
//! click goes, so most of what follows is about that gap staying closed.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const url = @import("../terminal/url.zig");
const support = @import("support.zig");
const pointer_support = @import("pointer_support.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;
const destroyModelSessions = app.deinitModel;
const startPointerHost = pointer_support.startPointerHost;
const pointerInput = pointer_support.pointerInput;
const terminalCellPoint = pointer_support.terminalCellPoint;
const terminalInteractionFrame = support.terminalInteractionFrame;

/// OSC 8, no id parameter: `open ++ href ++ st` starts a hyperlink and
/// `close_link` ends it. Split into pieces rather than wrapped in a helper so
/// every call site stays a comptime string concatenation.
const open_link = "\x1b]8;;";
const st = "\x1b\\";
const close_link = open_link ++ st;

/// Paint one session and hand back its lattice. Same seam
/// `cell_attribute_tests` uses: the view borrows the builder's stores, so the
/// builder outlives it in the caller's frame.
fn paintGrid(session: *grid.Session, builder: *canvas.Builder) !support.CellGridView {
    try grid.paint(session, builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
    });
    return support.expectCellGrid(builder.displayList());
}

/// The middle of cell (col, row), in widget points.
///
/// MEASURED, never assumed: the cell box comes from the painter that just ran
/// (`Session.setMeasuredCell` is its only writer), so this stays right at any
/// font size rather than encoding the estimate for one.
fn cellPoint(session: *grid.Session, col: usize, row: usize) grid.Session.HoverPoint {
    const cell = session.measuredCell().?;
    return .{
        .x = (@as(f32, @floatFromInt(col)) + 0.5) * cell.width,
        .y = (@as(f32, @floatFromInt(row)) + 0.5) * cell.height,
    };
}

fn linkAtCell(session: *grid.Session, col: usize, row: usize) ?grid.Session.Link {
    const point = cellPoint(session, col, row);
    return session.linkAtPoint(point.x, point.y);
}

/// Whether the painted cell carries any underline at all.
fn underlined(view: support.CellGridView, x: usize, y: usize) !bool {
    const flags = view.style(x, y) orelse return error.TestExpectedCell;
    return flags.underline != .none;
}

/// Assert exactly `[start, end)` of row `y` is underlined, out to `width`.
/// The negative half is the load-bearing half: an underline applied to the
/// whole row would satisfy any positive assertion on its own.
fn expectUnderlinedRange(view: support.CellGridView, y: usize, start: usize, end: usize, width: usize) !void {
    var x: usize = 0;
    while (x < width) : (x += 1) {
        const want = x >= start and x < end;
        const got = try underlined(view, x, y);
        if (want != got) {
            std.debug.print(
                "column {d} of row {d}: expected underline={}, got {}\n",
                .{ x, y, want, got },
            );
            return error.TestUnexpectedUnderline;
        }
    }
}

test "an OSC 8 hyperlink resolves through display text that is not a URL" {
    const session = try createSession(40, 6);
    defer session.destroy();
    session.setMeasuredCell(10, 20);

    // ABSENT FIRST: the same words with no OSC 8 around them are not a link,
    // so the assertion below cannot be passing on the text alone.
    session.feed("click here\r\n");
    session.refreshScreenText();
    try testing.expect(linkAtCell(session, 3, 0) == null);

    session.reset();
    session.setMeasuredCell(10, 20);
    session.feed(open_link ++ "https://example.com/docs" ++ st ++ "click here" ++ close_link ++ "\r\n");
    session.refreshScreenText();

    const link = linkAtCell(session, 3, 0) orelse return error.TestExpectedLink;
    try testing.expectEqualStrings("https://example.com/docs", link.url);
    try testing.expectEqual(grid.Session.LinkSource.osc8, link.source);
    // The cells AFTER the link are not part of it.
    try testing.expect(linkAtCell(session, 12, 0) == null);
}

test "REFUSED: an OSC 8 href whose scheme is not a document" {
    // The whole reason the explicit channel needs its own gate. `ls
    // --hyperlink` emits file:// for every name it prints, and a hostile
    // program can emit anything at all.
    const cases = [_][]const u8{
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html;base64,PHNjcmlwdD4=",
        "vbscript:msgbox(1)",
    };
    for (cases) |href| {
        const session = try createSession(60, 6);
        defer session.destroy();
        session.setMeasuredCell(10, 20);
        session.feed("\x1b]8;;");
        session.feed(href);
        session.feed("\x1b\\report" ++ close_link ++ "\r\n");
        session.refreshScreenText();
        try testing.expect(linkAtCell(session, 2, 0) == null);
    }
}

test "REFUSED: an OSC 8 href carrying control bytes or no target at all" {
    const session = try createSession(60, 6);
    defer session.destroy();
    session.setMeasuredCell(10, 20);
    // A scheme with nothing after it names no destination.
    session.feed(open_link ++ "https://" ++ st ++ "bare" ++ close_link ++ "\r\n");
    session.refreshScreenText();
    try testing.expect(linkAtCell(session, 1, 0) == null);

    // Refused at the module seam too, where the reasoning lives.
    try testing.expect(!url.isAllowedTarget("https://ok.example\x00javascript:alert(1)"));
    // U+202E RIGHT-TO-LEFT OVERRIDE could make the preview itself lie about
    // its visible order, so explicit targets stay in the URL ASCII alphabet.
    try testing.expect(!url.isAllowedTarget("https://bank.example/\xe2\x80\xaeevil.example"));
    try testing.expect(!url.isAllowedTarget("https://ok.example/a b"));
    try testing.expect(!url.isAllowedTarget("https://"));
    try testing.expect(!url.isAllowedTarget(""));
    try testing.expect(url.isAllowedTarget("https://ok.example/a"));
    try testing.expect(url.isAllowedTarget("mailto:a@example.com"));
    try testing.expect(url.targetIdentity("https://user@evil.example/path") != null);
    try testing.expect(url.targetIdentity("https://user@evil.example/path").?.effective_authority != null);
    try testing.expectEqualStrings("evil.example", url.targetIdentity("https://user@evil.example/path").?.effective_authority.?);
    try testing.expect(url.targetIdentity("https://bank.example\\@evil.example/path") == null);
    try testing.expect(url.targetIdentity("https://evil%2eexample/path") == null);
    try testing.expect(url.targetIdentity("https://2130706433/path") == null);
    try testing.expect(url.targetIdentity("https://127.0.0.1/path") != null);
}

test "REFUSED: a spoofing OSC 8 target neither previews nor overrides visible URL text" {
    const session = try createSession(80, 6);
    defer session.destroy();
    session.setMeasuredCell(10, 20);
    const shown = "https://bank.example";
    session.feed(open_link ++ "https://bank.example/\xe2\x80\xaeevil.example" ++ st ++ shown ++ close_link ++ "\r\n");
    session.refreshScreenText();

    _ = session.setPointerPoint(cellPoint(session, 5, 0));
    _ = session.setHoverPoint(cellPoint(session, 5, 0));
    try testing.expect(session.hoveredOsc8Target() == null);
    const fallback = linkAtCell(session, 5, 0) orelse return error.TestExpectedLink;
    try testing.expectEqualStrings(shown, fallback.url);
    try testing.expectEqual(grid.Session.LinkSource.text, fallback.source);
}

test "a display text that disagrees with its href previews and opens the real target" {
    const session = try createSession(60, 6);
    defer session.destroy();
    session.setMeasuredCell(10, 20);
    // The phishing shape: reads as the bank, points at the attacker.
    session.feed(open_link ++ "https://evil.example/steal" ++ st ++ "https://bank.example" ++ close_link ++ "\r\n");
    session.refreshScreenText();

    // The target is absent until the link chord is armed over these cells.
    try testing.expect(session.hoveredOsc8Target() == null);
    _ = session.setPointerPoint(cellPoint(session, 5, 0));
    _ = session.setHoverPoint(cellPoint(session, 5, 0));
    try testing.expectEqualStrings("https://evil.example/steal", session.hoveredOsc8Target().?);

    const hidden = linkAtCell(session, 5, 0) orelse return error.TestExpectedLink;
    try testing.expectEqualStrings("https://bank.example", hidden.url);
    try testing.expectEqual(grid.Session.LinkSource.text, hidden.source);

    session.markOsc8PreviewRendered("https://evil.example/steal");
    const revealed = linkAtCell(session, 5, 0) orelse return error.TestExpectedLink;
    try testing.expectEqualStrings("https://evil.example/steal", revealed.url);
    try testing.expectEqual(grid.Session.LinkSource.osc8, revealed.source);
}

test "nonvisual mismatch resolution retains the visible URL without a rendered receipt" {
    const session = try createSession(60, 6);
    defer session.destroy();
    session.setMeasuredCell(10, 20);
    session.feed(open_link ++ "https://evil.example/steal" ++ st ++ "https://bank.example" ++ close_link ++ "\r\n");
    session.refreshScreenText();

    // This is the route available to keyboard/accessibility callers today:
    // no pointer paint receipt exists, so explicit indirection stays disabled.
    const link = linkAtCell(session, 5, 0) orelse return error.TestExpectedLink;
    try testing.expectEqualStrings("https://bank.example", link.url);
    try testing.expectEqual(grid.Session.LinkSource.text, link.source);
}

fn previewGround(display_list: canvas.DisplayList, pane_index: usize) ?geometry.RectF {
    for (display_list.commands) |command| switch (command) {
        .fill_rect => |fill| if (fill.id == app.link_preview_ground_command_id_base + pane_index) return fill.rect,
        else => {},
    };
    return null;
}

fn previewText(display_list: canvas.DisplayList, pane_index: usize) ?[]const u8 {
    for (display_list.commands) |command| switch (command) {
        .draw_text => |text| if (text.id == app.link_preview_text_command_id_base + pane_index) return text.text,
        else => {},
    };
    return null;
}

fn previewAuthority(display_list: canvas.DisplayList, pane_index: usize) ?canvas.DrawText {
    for (display_list.commands) |command| switch (command) {
        .draw_text => |text| if (text.id == app.link_preview_authority_command_id_base + pane_index) return text,
        else => {},
    };
    return null;
}

test "a display text that AGREES with its href stays an explicit link" {
    // The control for the test above. Same shape, same code path, and the
    // only difference is that the two destinations match — so a guard that
    // simply refused every URL-shaped display text would fail here.
    const session = try createSession(60, 6);
    defer session.destroy();
    session.setMeasuredCell(10, 20);
    session.feed(open_link ++ "https://bank.example" ++ st ++ "https://bank.example" ++ close_link ++ "\r\n");
    session.refreshScreenText();

    const link = linkAtCell(session, 5, 0) orelse return error.TestExpectedLink;
    try testing.expectEqualStrings("https://bank.example", link.url);
    try testing.expectEqual(grid.Session.LinkSource.osc8, link.source);
}

test "an OSC 8 link beats the URL its own display text happens to contain" {
    // Not a mismatch, because the display text is not a URL at all: the
    // heuristic finds nothing to disagree with, so the explicit target wins
    // over text that merely mentions a host.
    const session = try createSession(60, 6);
    defer session.destroy();
    session.setMeasuredCell(10, 20);
    session.feed(open_link ++ "https://example.com/issue/7" ++ st ++ "issue #7" ++ close_link ++ "\r\n");
    session.refreshScreenText();

    const link = linkAtCell(session, 2, 0) orelse return error.TestExpectedLink;
    try testing.expectEqualStrings("https://example.com/issue/7", link.url);
}

test "hovering an OSC 8 link underlines its whole run and nothing else" {
    const session = try createSession(40, 6);
    defer session.destroy();
    session.feed("ab" ++ open_link ++ "https://example.com/a" ++ st ++ "click here" ++ close_link ++ "cd\r\n");
    session.refreshScreenText();

    // ABSENT: nothing is hovered, so nothing on the row is underlined.
    var before_commands: [512]canvas.CanvasCommand = undefined;
    var before_builder = canvas.Builder.init(&before_commands);
    const before = try paintGrid(session, &before_builder);
    try expectUnderlinedRange(before, 0, 0, 0, 14);

    // ACT: put the pointer inside the link's display text.
    _ = session.setHoverPoint(cellPoint(session, 4, 0));

    // PRESENT: exactly the link's cells, columns 2..12 ("click here"). The
    // "ab" before it and the "cd" after it stay bare.
    var after_commands: [512]canvas.CanvasCommand = undefined;
    var after_builder = canvas.Builder.init(&after_commands);
    const after = try paintGrid(session, &after_builder);
    try expectUnderlinedRange(after, 0, 2, 12, 14);
}

test "hovering a bare URL underlines the run the click would open" {
    const session = try createSession(60, 6);
    defer session.destroy();
    const url_text = "https://example.com/docs";
    session.feed("see " ++ url_text ++ " now\r\n");
    session.refreshScreenText();

    var before_commands: [512]canvas.CanvasCommand = undefined;
    var before_builder = canvas.Builder.init(&before_commands);
    const before = try paintGrid(session, &before_builder);
    try expectUnderlinedRange(before, 0, 0, 0, 32);

    _ = session.setHoverPoint(cellPoint(session, 8, 0));

    var after_commands: [512]canvas.CanvasCommand = undefined;
    var after_builder = canvas.Builder.init(&after_commands);
    const after = try paintGrid(session, &after_builder);
    // "see " is 4 columns; the URL is ASCII so its columns are its bytes.
    try expectUnderlinedRange(after, 0, 4, 4 + url_text.len, 32);
}

test "a hover over ordinary text underlines nothing" {
    const session = try createSession(60, 6);
    defer session.destroy();
    session.feed("see https://example.com/docs now\r\n");
    session.refreshScreenText();

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    _ = try paintGrid(session, &builder);
    // Column 1 is inside the word "see". The pointer is armed and over the
    // same ROW as a link — only the cell it is actually on decides.
    _ = session.setHoverPoint(cellPoint(session, 1, 0));

    var after_commands: [512]canvas.CanvasCommand = undefined;
    var after_builder = canvas.Builder.init(&after_commands);
    const after = try paintGrid(session, &after_builder);
    try expectUnderlinedRange(after, 0, 0, 0, 32);
}

test "a refused OSC 8 href underlines nothing either" {
    // The gate reaches the GLASS, not just the click: a link the app would
    // refuse to open must not be advertised as one it would.
    const session = try createSession(40, 6);
    defer session.destroy();
    session.feed(open_link ++ "file:///etc/passwd" ++ st ++ "passwd" ++ close_link ++ "\r\n");
    session.refreshScreenText();

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    _ = try paintGrid(session, &builder);
    _ = session.setHoverPoint(cellPoint(session, 2, 0));

    var after_commands: [512]canvas.CanvasCommand = undefined;
    var after_builder = canvas.Builder.init(&after_commands);
    const after = try paintGrid(session, &after_builder);
    try expectUnderlinedRange(after, 0, 0, 0, 10);
}

test "the underline follows its text when output scrolls under the pointer" {
    // Why the session stores a POINT and resolves it per frame instead of
    // caching the span the pointer event found: a stationary pointer over a
    // link that scrolls up is no longer over that link, and an underline left
    // behind would be pointing at whatever moved into those cells.
    const session = try createSession(40, 6);
    defer session.destroy();
    session.feed(open_link ++ "https://example.com/a" ++ st ++ "click here" ++ close_link ++ "\r\n");
    session.refreshScreenText();

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    _ = try paintGrid(session, &builder);
    _ = session.setHoverPoint(cellPoint(session, 4, 0));

    var armed_commands: [512]canvas.CanvasCommand = undefined;
    var armed_builder = canvas.Builder.init(&armed_commands);
    const armed = try paintGrid(session, &armed_builder);
    try expectUnderlinedRange(armed, 0, 0, 10, 14);

    // Print a plain line at the top, pushing the link down a row without the
    // pointer moving at all.
    session.feed("\x1b[H\x1b[Lplain\r\n");
    session.refreshScreenText();

    var moved_commands: [512]canvas.CanvasCommand = undefined;
    var moved_builder = canvas.Builder.init(&moved_commands);
    const moved = try paintGrid(session, &moved_builder);
    // Row 0 now holds "plain", which is not a link; the pointer is still on
    // row 0, so nothing is underlined anywhere.
    try expectUnderlinedRange(moved, 0, 0, 0, 14);
    try expectUnderlinedRange(moved, 1, 0, 0, 14);
}

// ------------------------------------------------------- through the app
//
// The three tests below drive the real pointer path. Everything above proves
// the session answers correctly; these prove the app ASKS — the hover chord
// and the click both run through `handleTerminalPointer`, and a session that
// resolves links perfectly is worth nothing if nothing arms it.

/// Repaint the main window's canvas from the model, the way the runtime does,
/// and hand back the focused pane's lattice.
fn paintHostGrid(host: *app.CockpitHost, builder: *canvas.Builder) !support.CellGridView {
    try app.buildChromeWindow(&host.inner.model, builder, support.mainChromeContext());
    return support.expectCellGrid(builder.displayList());
}

test "cmd+click follows an OSC 8 href, not the words it is wrapped around" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    const model = &host.inner.model;
    const pane = model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;

    try host.inner.effects.feedPtyOutput(
        pane.pty_key,
        open_link ++ "https://example.com/docs" ++ st ++ "read the docs" ++ close_link ++ "\r\n",
    );
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "read the docs") orelse
        return error.TestExpectedTerminalInteractionSurface;

    // ABSENT: nothing has been opened yet, so the count below is not carrying
    // over from the fixture.
    try testing.expectEqual(@as(u32, 0), model.opened_url_count);

    // "read the docs" is ordinary prose — the heuristic finds nothing here, so
    // the only way this opens anything at all is the explicit channel.
    const on_link = terminalCellPoint(pane, frame, 4, 0);
    try pointerInput(harness, app_iface, .pointer_down, on_link, 0, .{ .command = true }, 0);
    try testing.expectEqual(@as(u32, 1), model.opened_url_count);
    try testing.expectEqualStrings("https://example.com/docs", model.openedUrl());
}

// GUARD: osc8-pane-local-target-preview
test "quick Cmd-click on an unpreviewed OSC 8 mismatch opens only visible text" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    const model = &host.inner.model;
    const pane = model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;

    try host.inner.effects.feedPtyOutput(
        pane.pty_key,
        open_link ++ "https://evil.example/steal" ++ st ++ "https://bank.example" ++ close_link ++ "\r\n",
    );
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "https://bank.example") orelse return error.TestExpectedTerminalInteractionSurface;
    const on_link = terminalCellPoint(pane, frame, 5, 0);

    // No move/hover and therefore no rendered receipt precedes this press.
    try pointerInput(harness, app_iface, .pointer_down, on_link, 0, .{ .command = true }, 0);
    try testing.expectEqual(@as(u32, 1), model.opened_url_count);
    try testing.expectEqualStrings("https://bank.example", model.openedUrl());
}

test "long OSC 8 userinfo cannot hide the rendered effective authority" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    const pane = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;

    const userinfo = try gpa.alloc(u8, url.max_url_bytes - 320);
    defer gpa.free(userinfo);
    @memset(userinfo, 'b');
    const host_prefix = try gpa.alloc(u8, 223);
    defer gpa.free(host_prefix);
    @memset(host_prefix, 'a');
    host_prefix[55] = '.';
    host_prefix[111] = '.';
    host_prefix[167] = '.';
    const href = try std.fmt.allocPrint(gpa, "https://{s}@{s}.EVIL.Example/steal", .{ userinfo, host_prefix });
    defer gpa.free(href);
    try testing.expect(href.len <= url.max_url_bytes);
    const output = try std.fmt.allocPrint(gpa, "{s}{s}{s}{s}{s}\r\n", .{ open_link, href, st, "https://bank.example", close_link });
    defer gpa.free(output);
    try host.inner.effects.feedPtyOutput(pane.pty_key, output);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "https://bank.example") orelse return error.TestExpectedTerminalInteractionSurface;
    try pointerInput(harness, app_iface, .pointer_move, terminalCellPoint(pane, frame, 5, 0), 0, .{}, 0);

    var commands: [native_sdk.runtime.max_canvas_commands_per_view]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    _ = try paintHostGrid(host, &builder);
    const authority = previewAuthority(builder.displayList(), 0) orelse return error.TestExpectedLinkPreview;
    try testing.expect(std.mem.startsWith(u8, authority.text, "..."));
    try testing.expect(std.mem.endsWith(u8, authority.text, ".evil.example"));
    const layout_options = authority.text_layout orelse return error.TestExpectedLinkPreview;
    try testing.expectEqual(canvas.TextOverflow.clip, layout_options.overflow);
    try testing.expect(canvas.measureTextWidthForFont(layout_options.measure, authority.font_id, authority.text, authority.size) <= layout_options.max_width);
}

test "no preview permanently taxes a saturated terminal command budget" {
    const session = try createSession(80, 24);
    defer session.destroy();
    for (0..24) |_| {
        for (0..80) |_| session.feed("\u{256c}");
    }
    // This is the exact reserve subtracted from the cumulative command
    // envelope. Zero is stronger than hoping a particular row still fits.
    try testing.expectEqual(@as(usize, 0), app.linkPreviewCommandReserve(session));

    session.feed("\x1b[H" ++ open_link ++ "https://evil.example" ++ st ++ "x" ++ close_link);
    session.setMeasuredCell(10, 20);
    _ = session.setPointerPoint(cellPoint(session, 0, 0));
    try testing.expectEqual(@as(usize, 3), app.linkPreviewCommandReserve(session));
}

test "the link chord arms the hover underline, and a bare pointer does not" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    const model = &host.inner.model;
    const pane = model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;

    try host.inner.effects.feedPtyOutput(pane.pty_key, "see https://example.com/docs now\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "https://example.com/docs") orelse
        return error.TestExpectedTerminalInteractionSurface;
    const on_link = terminalCellPoint(pane, frame, 10, 0);

    // ABSENT: the pointer is over the link, with no modifier held. Hovering a
    // URL must not underline it on its own — the underline advertises a chord,
    // and one that is not being held promises a click that would only select.
    try pointerInput(harness, app_iface, .pointer_move, on_link, 0, .{}, 0);
    var bare_commands: [native_sdk.runtime.max_canvas_commands_per_view]canvas.CanvasCommand = undefined;
    var bare_builder = canvas.Builder.init(&bare_commands);
    const bare = try paintHostGrid(host, &bare_builder);
    try expectUnderlinedRange(bare, 0, 0, 0, 32);

    // ACT: same point, chord held.
    try pointerInput(harness, app_iface, .pointer_move, on_link, 0, .{ .command = true }, 0);
    var armed_commands: [native_sdk.runtime.max_canvas_commands_per_view]canvas.CanvasCommand = undefined;
    var armed_builder = canvas.Builder.init(&armed_commands);
    const armed = try paintHostGrid(host, &armed_builder);
    try expectUnderlinedRange(armed, 0, 4, 4 + "https://example.com/docs".len, 32);

    // ...and letting the chord go takes it away again.
    try pointerInput(harness, app_iface, .pointer_move, on_link, 0, .{}, 0);
    var released_commands: [native_sdk.runtime.max_canvas_commands_per_view]canvas.CanvasCommand = undefined;
    var released_builder = canvas.Builder.init(&released_commands);
    const released = try paintHostGrid(host, &released_builder);
    try expectUnderlinedRange(released, 0, 0, 0, 32);
}

test "window blur takes the hover underline with it" {
    // The chord is a HELD key, and a blur is exactly how it stops being held
    // without this app ever seeing the release.
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    const model = &host.inner.model;
    const pane = model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;

    try host.inner.effects.feedPtyOutput(pane.pty_key, "see https://example.com/docs now\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "https://example.com/docs") orelse
        return error.TestExpectedTerminalInteractionSurface;
    const on_link = terminalCellPoint(pane, frame, 10, 0);
    try pointerInput(harness, app_iface, .pointer_move, on_link, 0, .{ .command = true }, 0);

    var armed_commands: [native_sdk.runtime.max_canvas_commands_per_view]canvas.CanvasCommand = undefined;
    var armed_builder = canvas.Builder.init(&armed_commands);
    const armed = try paintHostGrid(host, &armed_builder);
    try expectUnderlinedRange(armed, 0, 4, 4 + "https://example.com/docs".len, 32);

    app.update(&host.inner.model, .{ .focus_changed = false }, &host.inner.effects);
    var blurred_commands: [native_sdk.runtime.max_canvas_commands_per_view]canvas.CanvasCommand = undefined;
    var blurred_builder = canvas.Builder.init(&blurred_commands);
    const blurred = try paintHostGrid(host, &blurred_builder);
    try expectUnderlinedRange(blurred, 0, 0, 0, 32);
}
