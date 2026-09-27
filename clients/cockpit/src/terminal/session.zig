//! One libghostty emulator session and its projection into a
//! `canvas.TerminalGrid` (see `Session.snapshot`). libghostty-vt owns cell
//! state, scrollback and selection; the SDK painter owns pixels. The SDK's own
//! terminal session store is not used for product panes because it has no
//! inbound byte feed (docs/DECISIONS.md).

const std = @import("std");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const url_module = @import("url.zig");
const palette_module = @import("palette.zig");

const canvas = native_sdk.canvas;
const Palette = palette_module.Palette;
const cellBackground = palette_module.cellBackground;
const themeRgb = palette_module.themeRgb;

/// Ghostty's standard terminal word boundaries. The pinned VT module exports
/// SelectionGesture but keeps its default codepoint policy private.
const pointer_word_boundaries = [_]u21{
    0,   ' ', '\t', '\'', '"',
    '│',
    '`', '|', ':',  ';',  ',',
    '(', ')', '[',  ']',  '{',
    '}', '<', '>',  '$',
};

/// Grid ceilings bound allocation and command-id geometry. Painter resources
/// are content-dependent: a screen full of repeated ASCII does not consume one
/// glyph-atlas entry per cell, so cell count must never shrink the PTY below its
/// visible pane.
pub const max_cols: usize = 320;
pub const max_rows: usize = 96;
pub const max_cells: usize = max_cols * max_rows;
/// Four bytes per cell preserves every primary Unicode scalar across the full
/// viewport. Combining data that exceeds this bound becomes an atomic
/// paint-budget stop rather than silently blank cells.
pub const snapshot_text_capacity: usize = max_cells * 4;
const snapshot_text_overflow_cluster = " " ** (canvas.max_display_list_text_bytes + 1);

/// Display bound for an OSC 0/2 title; longer titles truncate at a scalar
/// boundary rather than leaving the pane titleless.
pub const max_title_bytes: usize = 256;

/// Scrollback-search needle ceiling, keeping search state inline.
pub const max_search_needle_bytes: usize = 128;

/// Highlight tags for `RenderState.updateHighlightsFlattened`. The cell loop
/// takes the FIRST covering range, so the current match must be pushed before
/// the match list (see `applySearchHighlights`).
pub const search_current_tag: u8 = 1;
pub const search_match_tag: u8 = 2;

/// One terminal's scrollback search. It lives on the session because the
/// engine pins into this emulator's `PageList`.
pub const Search = struct {
    /// The field is showing; an empty field is not a failed search.
    open: bool = false,
    needle_buf: [max_search_needle_bytes]u8 = undefined,
    needle_len: usize = 0,
    /// Rebuilt whenever the needle changes; null while the needle is empty.
    engine: ?vt.search.Screen = null,
    /// The screen the engine pinned into. Leaving the alternate screen can
    /// destroy it, so the engine must be rebuilt (see `discardSearchEngine`).
    screen_key: vt.ScreenSet.Key = .primary,
    screen_generation: usize = 0,
    /// The engine has not finished walking scrollback; keep pumping.
    incomplete: bool = false,
    /// Only the first match takes the selection; later ones do not move the
    /// viewport.
    landed: bool = false,
    /// Viewport position when the field opened, restored on Escape.
    restore_row: usize = 0,
    restore_bottom: bool = true,
};

/// A measured terminal cell in canvas points, finite and positive by
/// construction (`Session.setMeasuredCell`). Readers get it through an
/// optional so nothing can divide by a plausible-looking default before the
/// painter has measured.
pub const CellBox = struct {
    width: f32,
    height: f32,
};

/// One live emulator session. Heap-owned by the app (the model holds a
/// pointer): the emulator allocates internally and its state is derived
/// entirely from journaled inputs — fed pty bytes, resizes, and
/// selection edits — so a replayed session rebuilds it byte-identical.
pub const Session = struct {
    gpa: std.mem.Allocator,
    term: vt.Terminal,
    stream: vt.TerminalStream,
    render: vt.RenderState,
    /// Query answers (DSR, DA1, ...) produced while feeding, drained by the
    /// app to the pty. Grows to fit up to `response_capacity_max` because
    /// replies accumulate while the outbound ring is full; past that a reply
    /// is dropped whole (never cut) and counted.
    response_buffer: []u8 = &.{},
    response_len: usize = 0,
    response_bytes_dropped: u64 = 0,
    /// Lazily serialized viewport text (see `screenText`), the accessibility
    /// surface. Exact, never truncated; recomputed only when dirty.
    screen_text_buf: []u8 = &.{},
    screen_text_len: usize = 0,
    screen_text_dirty: bool = true,
    /// Keyboard-selection state: the anchor stays put, the head moves.
    select_anchor: ?CellPos = null,
    select_head: CellPos = .{},
    select_block: bool = false,
    /// Ghostty's native pointer-selection gesture. Its pins survive output
    /// and viewport movement while a drag is active; the model only owns
    /// which terminal/generation receives subsequent pointer phases.
    pointer_selection: vt.SelectionGesture = .init,
    /// Null until the painter has measured the mono face (see `CellBox`).
    measured_cell: ?CellBox = null,
    font_size: f32 = 13,
    /// WCAG contrast floor applied by `snapshot` (see `Palette.contrasted`).
    /// Defaults to 1 (no floor) so unpainted sessions are not colour-shifted;
    /// `render.paint` writes the configured value every frame.
    minimum_contrast: f32 = 1,
    /// Per-session projection buffers rewritten by every `snapshot`, sized to
    /// the painter's ceilings.
    snap_rows: []canvas.TerminalRow = &.{},
    snap_cells: []canvas.TerminalCell = &.{},
    snap_text: []u8 = &.{},
    snap_text_len: usize = 0,
    /// The child's last OSC 0/2 title, copied because the emulator's storage
    /// is only valid until the next title change. Empty means never reported.
    title_buf: [max_title_bytes]u8 = undefined,
    title_len: usize = 0,
    /// The child's last OSC 7 directory, decoded to a path (see
    /// `decodePwdUrl`). Empty means unknown.
    pwd_buf: [std.fs.max_path_bytes]u8 = undefined,
    pwd_len: usize = 0,
    /// BEL latch; the app reads and clears it as an attention cue.
    bell_rung: bool = false,
    search: Search = .{},
    /// Copy of the last link `linkAtPoint` resolved; OSC 8 hrefs live in page
    /// memory the next feed may move. Valid until the next `linkAtPoint`.
    link_buf: [url_module.max_url_bytes]u8 = undefined,
    link_len: usize = 0,
    /// Pointer position while the link chord is held, or null. A point rather
    /// than a span so `snapshot` resolves the underline against live state.
    hover_point: ?HoverPoint = null,
    /// Pointer location independent of modifiers, for the OSC 8 preview.
    pointer_point: ?HoverPoint = null,
    /// Receipt for the exact OSC 8 target that was last displayed. A
    /// mismatched visible URL may use its href only while this still matches.
    preview_rendered_cell: ?CellPos = null,
    preview_rendered_buf: [url_module.max_url_bytes]u8 = undefined,
    preview_rendered_len: usize = 0,
    /// Separate from `link_buf`, which `snapshot` may overwrite with a
    /// visible-text fallback while the preview target is borrowed.
    preview_target_buf: [url_module.max_url_bytes]u8 = undefined,
    preview_target_len: usize = 0,

    pub const CellPos = struct { x: u16 = 0, y: u16 = 0 };

    pub const HoverPoint = struct { x: f32, y: f32 };

    /// `.osc8` is a target the program named; `.text` one this app recognised
    /// in visible bytes. They carry different trust.
    pub const LinkSource = enum { osc8, text };

    /// A link under a point. `url` is session-owned (see `link_buf`). The
    /// column span is the matched run for `.text`, but only the hit cell for
    /// `.osc8`; `snapshot` re-derives OSC 8 extents by target equality.
    pub const Link = struct {
        url: []const u8,
        source: LinkSource,
        row: u16,
        start_col: u16,
        end_col: u16,
    };

    pub const PointerSelectionEvent = struct {
        phase: canvas.WidgetPointerPhase,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        click_count: u8 = 1,
    };

    pub const PointerAutoscrollEvent = struct {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    };

    /// Query-answer buffer's INITIAL size (it grows to fit).
    pub const response_capacity: usize = 16 * 1024;

    /// Matched to the app's pending-outbound ring: a larger retained reply
    /// could never be enqueued whole anyway.
    pub const response_capacity_max: usize = 64 * 1024;

    /// Output is fed in sub-slices of at most this size, draining answers
    /// after each. Large slices keep parser throughput high; the response
    /// buffer grows to fit, so it needs no safety factor.
    pub const feed_slice_bytes: usize = response_capacity;

    /// Default scrollback ceiling in bytes (Ghostty's default); pages are
    /// allocated on demand.
    pub const max_scrollback: usize = 50_000_000;

    /// `create` with the default scrollback. Config-driven callers use
    /// `createWithScrollback` so `scrollback-limit` applies.
    pub fn create(gpa: std.mem.Allocator, io: std.Io, initial_cols: u16, initial_rows: u16) !*Session {
        return createWithScrollback(gpa, io, initial_cols, initial_rows, max_scrollback);
    }

    pub fn createWithScrollback(
        gpa: std.mem.Allocator,
        io: std.Io,
        initial_cols: u16,
        initial_rows: u16,
        max_scrollback_bytes: usize,
    ) !*Session {
        const session = try gpa.create(Session);
        errdefer gpa.destroy(session);
        session.* = .{
            .gpa = gpa,
            .term = try vt.Terminal.init(io, gpa, .{
                .cols = @intCast(@min(initial_cols, max_cols)),
                .rows = @intCast(@min(initial_rows, max_rows)),
                .max_scrollback = max_scrollback_bytes,
            }),
            .stream = undefined,
            .render = .empty,
        };
        errdefer session.term.deinit(gpa);
        session.response_buffer = try gpa.alloc(u8, response_capacity);
        session.snap_rows = try gpa.alloc(canvas.TerminalRow, max_rows);
        session.snap_cells = try gpa.alloc(canvas.TerminalCell, max_cells);
        session.snap_text = try gpa.alloc(u8, snapshot_text_capacity);
        session.stream = .initAlloc(gpa, .init(&session.term));
        session.installStreamEffects();
        return session;
    }

    /// Project the live viewport into an allocation-free `canvas.TerminalGrid`.
    /// Every slice points into this session's buffers and stays valid until
    /// the next `snapshot`. Box-drawing cells carry no cluster; the painter
    /// draws them as geometry.
    pub fn snapshot(
        session: *Session,
        tokens: canvas.DesignTokens,
        running: bool,
        selecting: bool,
    ) !canvas.TerminalGrid {
        // Theme colors go into the emulator's defaults (not OSC overrides) so
        // ghostty composes fg/bg/cursor. The ANSI-16 palette is left as the
        // emulator's own: a terminal red is not the UI's destructive token.
        session.term.colors.foreground.default = themeRgb(tokens.colors.text);
        session.term.colors.background.default = themeRgb(tokens.colors.background);
        session.term.colors.cursor.default = themeRgb(tokens.colors.accent);

        try session.render.update(session.gpa, &session.term);
        // After the render update (which clears touched rows), before the
        // cell loop reads them.
        session.applySearchHighlights();
        // The hover underline is resolved against this frame's state. It does
        // not use `updateHighlightsFlattened`: the pinned libghostty-vt cannot
        // construct a `highlight.Flattened` outside the search engine.
        const hover = session.hoverLink();
        // A text link is one row's column range; an OSC 8 link is re-derived
        // per cell by target equality because its run may wrap or repeat.
        const hover_span: ?Link = if (hover) |link|
            (if (link.source == .text) link else null)
        else
            null;
        const hover_href: ?[]const u8 = if (hover) |link|
            (if (link.source == .osc8) link.url else null)
        else
            null;
        const rs = &session.render;
        const palette = Palette.init(tokens, &rs.colors, &session.term.colors.palette, session.minimum_contrast);

        session.snap_text_len = 0;
        var row_count: usize = 0;
        var cell_cursor: usize = 0;

        var row_index: usize = 0;
        while (row_index < rs.row_data.len and row_count < session.snap_rows.len) : (row_index += 1) {
            const row = rs.row_data.get(row_index);
            const width = @min(row.cells.len, session.snap_cells.len - cell_cursor);
            const out = session.snap_cells[cell_cursor..][0..width];

            var x: usize = 0;
            while (x < width) : (x += 1) {
                const cell = row.cells.get(x);
                var cp: u21 = switch (cell.raw.content_tag) {
                    .codepoint, .codepoint_grapheme => cell.raw.content.codepoint.data,
                    else => 0,
                };
                // `cp` and `cluster` land last: the invisible flag can zero `cp`.
                var projected: canvas.TerminalCell = .{
                    .fg = palette.foreground,
                    .bg = cellBackground(cell, &palette),
                    .wide = switch (cell.raw.wide) {
                        .wide => .wide,
                        .spacer_tail, .spacer_head => .spacer,
                        else => .narrow,
                    },
                };
                if (cp != 0 and cell.raw.style_id != 0) {
                    const style = cell.style;
                    // Colour attributes (inverse, faint, bold-as-bright) fold
                    // into `resolveFg`; every other attribute is projected as
                    // a cell field for the renderer. Unwritten cells have no
                    // style; a styled blank is a space and lands here.
                    projected.fg = palette.resolveFg(style, projected.bg, cp);
                    projected.bold = style.flags.bold;
                    projected.italic = style.flags.italic;
                    projected.strikethrough = style.flags.strikethrough;
                    projected.overline = style.flags.overline;
                    projected.underline = style.flags.underline != .none;
                    // `underline_style` is read only when `underline` is set.
                    projected.underline_style = switch (style.flags.underline) {
                        .none, .single => .single,
                        .double => .double,
                        .curly => .curly,
                        .dotted => .dotted,
                        .dashed => .dashed,
                    };
                    projected.underline_color = palette.resolveUnderlineColor(style);
                    // Invisible hides the glyph but keeps decorations.
                    if (style.flags.invisible) cp = 0;
                }

                // Search washes go on the cell (a row's single selection
                // range cannot hold several matches). Ranges are inclusive and
                // the first covering entry wins; the current match is first.
                for (row.highlights.items) |hl| {
                    if (x < hl.range[0] or x > hl.range[1]) continue;
                    const current = hl.tag == search_current_tag;
                    projected.bg = if (current) palette.search_current else palette.search_match;
                    projected.fg = if (current) palette.search_current_text else palette.search_match_text;
                    break;
                }

                // Hover forces only the underline flag, keeping any SGR
                // underline style and colour the cell already has.
                if (hover_span) |span| {
                    if (row_index == @as(usize, span.row) and
                        x >= @as(usize, span.start_col) and
                        x < @as(usize, span.end_col))
                    {
                        projected.underline = true;
                    }
                } else if (hover_href) |href| {
                    if (cell.raw.hyperlink) {
                        if (cellHyperlinkUri(row.pin, x)) |uri| {
                            if (std.mem.eql(u8, uri, href)) projected.underline = true;
                        }
                    }
                }

                // Box drawing is painted as geometry; everything else stages
                // its full grapheme into the text arena.
                var cluster: []const u8 = "";
                if (cp != 0 and !canvas.terminal_box.isBoxDrawing(cp)) {
                    const start = session.snap_text_len;
                    var overflow = false;
                    const primary_len = std.unicode.utf8CodepointSequenceLength(cp) catch 0;
                    if (primary_len == 0 or primary_len > session.snap_text.len - session.snap_text_len) {
                        overflow = true;
                    } else {
                        session.snap_text_len += std.unicode.utf8Encode(
                            cp,
                            session.snap_text[session.snap_text_len..],
                        ) catch 0;
                    }
                    if (!overflow and cell.raw.content_tag == .codepoint_grapheme) {
                        for (cell.grapheme) |extra| {
                            const extra_len = std.unicode.utf8CodepointSequenceLength(extra) catch 0;
                            if (extra_len == 0 or extra_len > session.snap_text.len - session.snap_text_len) {
                                overflow = true;
                                break;
                            }
                            session.snap_text_len += std.unicode.utf8Encode(
                                extra,
                                session.snap_text[session.snap_text_len..],
                            ) catch 0;
                        }
                    }
                    if (overflow) {
                        // An over-ceiling sentinel degrades the row whole; an
                        // empty cluster would silently erase ink.
                        session.snap_text_len = start;
                        cluster = snapshot_text_overflow_cluster;
                    } else {
                        cluster = session.snap_text[start..session.snap_text_len];
                    }
                }

                projected.cp = cp;
                projected.cluster = cluster;
                out[x] = projected;
            }

            session.snap_rows[row_count] = .{
                .cells = out,
                .selection = if (row.selection) |range| .{
                    @intCast(range[0]),
                    @intCast(range[1]),
                } else null,
            };
            cell_cursor += width;
            row_count += 1;
        }

        const bar = session.scrollbar();
        // `rs.cursor.viewport` is null when scrolled into history: no cursor.
        const cursor: ?canvas.TerminalCursor = blk: {
            if (!rs.cursor.visible) break :blk null;
            const vp = rs.cursor.viewport orelse break :blk null;
            const x: u16 = @intCast(vp.x);
            break :blk .{
                // A cursor on a wide cell's spacer tail is drawn on the primary.
                .x = if (vp.wide_tail and x > 0) x - 1 else x,
                .y = @intCast(vp.y),
                // Kept distinct from `.block`: the painter also hollows the
                // cursor for an unfocused window, independently.
                .shape = switch (rs.cursor.visual_style) {
                    .bar => .bar,
                    .underline => .underline,
                    .block => .block,
                    .block_hollow => .block_hollow,
                },
                // Blink is carried as state; the host owns any animation.
                .blinking = rs.cursor.blinking,
                .wide = rs.cursor.cell.wide == .wide or vp.wide_tail,
            };
        };

        return .{
            .rows = session.snap_rows[0..row_count],
            .background = palette.background,
            .foreground = palette.foreground,
            .cursor_color = palette.cursor,
            .selection_color = palette.selection,
            .cursor = cursor,
            .running = running,
            .select_head = if (selecting)
                .{ .x = session.select_head.x, .y = session.select_head.y }
            else
                null,
            .scrollbar = .{
                .offset = @intCast(bar.offset),
                .len = @intCast(bar.len),
                .total = @intCast(bar.total),
            },
            .screen_text = session.screenText(),
        };
    }

    /// Wire the parser's effect callbacks; they only copy into session
    /// storage. OSC 52 `clipboard_write` stays null: writing the user's
    /// clipboard from any program needs a consent story this does not have.
    fn installStreamEffects(session: *Session) void {
        session.stream.handler.effects = .{
            .bell = bellRang,
            .clipboard_write = null,
            .color_scheme = null,
            .device_attributes = null,
            .enquiry = null,
            .size = null,
            .title_changed = titleChanged,
            .pwd_changed = pwdChanged,
            .write_pty = writePtyResponse,
            .xtversion = null,
        };
    }

    pub fn destroy(session: *Session) void {
        const gpa = session.gpa;
        // Before the terminal: the engine untracks pins in the screen's pool.
        session.discardSearchEngine();
        session.pointer_selection.deinit(&session.term);
        session.render.deinit(gpa);
        session.stream.deinit();
        session.term.deinit(gpa);
        gpa.free(session.response_buffer);
        gpa.free(session.snap_rows);
        gpa.free(session.snap_cells);
        gpa.free(session.snap_text);
        if (session.screen_text_buf.len > 0) gpa.free(session.screen_text_buf);
        gpa.destroy(session);
    }

    /// Feed one pty output batch through the VT stream. Parser state
    /// persists across batches (escape sequences split at a chunk
    /// boundary keep parsing).
    pub fn feed(session: *Session, bytes: []const u8) void {
        session.invalidateRenderedPreview();
        session.stream.nextSlice(bytes);
        session.screen_text_dirty = true;
    }

    /// Hard-reset (RIS) for a fresh shell: screen, scrollback, modes, colour
    /// overrides, and any half-parsed escape sequence, so a restarted shell
    /// inherits nothing from the previous one.
    pub fn reset(session: *Session) void {
        session.discardSearchEngine();
        session.search = .{};
        session.pointer_selection.reset(&session.term);
        session.term.fullReset();
        // RIS leaves OSC 4/10/11/12 overrides alone.
        session.term.colors.foreground.override = null;
        session.term.colors.background.override = null;
        session.term.colors.cursor.override = null;
        session.term.colors.palette.resetAll();
        session.stream.deinit();
        session.stream = .initAlloc(session.gpa, .init(&session.term));
        session.installStreamEffects();
        session.response_len = 0;
        session.response_bytes_dropped = 0;
        session.clearSelection();
        session.select_head = .{};
        session.select_block = false;
        session.screen_text_dirty = true;
        session.title_len = 0;
        session.pwd_len = 0;
        session.bell_rung = false;
    }

    /// Terminal query answers accumulated by the last feeds; the caller
    /// writes them to the pty and calls `clearResponses`.
    pub fn pendingResponses(session: *const Session) []const u8 {
        return session.response_buffer[0..session.response_len];
    }

    pub fn clearResponses(session: *Session) void {
        session.response_len = 0;
    }

    fn writePtyResponse(handler: *vt.TerminalStream.Handler, bytes: [:0]const u8) void {
        const session: *Session = @alignCast(@fieldParentPtr("term", handler.terminal));
        const needed = session.response_len + bytes.len;
        if (needed > session.response_buffer.len) {
            // Double up to the ceiling; past it, or on allocation failure,
            // the reply drops whole and counted.
            if (needed > response_capacity_max) {
                session.response_bytes_dropped +|= bytes.len;
                return;
            }
            var new_cap = @max(session.response_buffer.len * 2, response_capacity);
            while (new_cap < needed) new_cap *= 2;
            if (new_cap > response_capacity_max) new_cap = response_capacity_max;
            if (session.gpa.realloc(session.response_buffer, new_cap)) |grown| {
                session.response_buffer = grown;
            } else |_| {
                session.response_bytes_dropped +|= bytes.len;
                return;
            }
        }
        @memcpy(session.response_buffer[session.response_len..needed], bytes);
        session.response_len += bytes.len;
    }

    // ------------------------------------------- child-reported identity

    /// OSC 0/2 landed; the value comes back through `getTitle`.
    fn titleChanged(handler: *vt.TerminalStream.Handler) void {
        const session: *Session = @alignCast(@fieldParentPtr("term", handler.terminal));
        const reported = handler.terminal.getTitle() orelse "";
        const kept = truncateUtf8(reported, session.title_buf.len);
        @memcpy(session.title_buf[0..kept.len], kept);
        session.title_len = kept.len;
    }

    /// OSC 7 landed. `getPwd` returns the raw payload, decoded here once.
    fn pwdChanged(handler: *vt.TerminalStream.Handler) void {
        const session: *Session = @alignCast(@fieldParentPtr("term", handler.terminal));
        const reported = handler.terminal.getPwd() orelse "";
        session.pwd_len = decodePwdUrl(reported, &session.pwd_buf);
    }

    /// BEL: latch only; the app owns attention cues.
    fn bellRang(handler: *vt.TerminalStream.Handler) void {
        const session: *Session = @alignCast(@fieldParentPtr("term", handler.terminal));
        session.bell_rung = true;
    }

    /// The child's OSC 0/2 title, or "" when it never set one. The slice
    /// points into the session and stays valid until the next title report.
    pub fn title(session: *const Session) []const u8 {
        return session.title_buf[0..session.title_len];
    }

    /// The child's OSC 7 directory as a plain absolute path, or "" when it is
    /// unknown. The slice points into the session and stays valid until the
    /// next pwd report.
    pub fn pwd(session: *const Session) []const u8 {
        return session.pwd_buf[0..session.pwd_len];
    }

    /// OSC 133: whether the cursor sits at a shell prompt. False without
    /// prompt-mark integration.
    pub fn atPrompt(session: *Session) bool {
        return session.term.cursorIsAtPrompt();
    }

    /// Clamp `bytes` to `limit` without cutting a UTF-8 scalar in half.
    fn truncateUtf8(bytes: []const u8, limit: usize) []const u8 {
        if (bytes.len <= limit) return bytes;
        var end = limit;
        while (end > 0 and bytes[end] & 0xC0 == 0x80) end -= 1;
        return bytes[0..end];
    }

    /// Decode an OSC 7 payload (`file://` or `kitty-shell-cwd://`, or a bare
    /// absolute path) into `out`; returns bytes written, 0 when unusable.
    /// The host is ignored: a remote path only makes the spawn's `cd` fail
    /// and fall back (see `paneArgvIn`).
    fn decodePwdUrl(raw: []const u8, out: []u8) usize {
        var path = raw;
        if (std.mem.indexOf(u8, raw, "://")) |scheme_end| {
            const scheme = raw[0..scheme_end];
            if (!std.ascii.eqlIgnoreCase(scheme, "file") and
                !std.ascii.eqlIgnoreCase(scheme, "kitty-shell-cwd"))
            {
                return 0;
            }
            const authority = raw[scheme_end + 3 ..];
            const path_start = std.mem.indexOfScalar(u8, authority, '/') orelse return 0;
            path = authority[path_start..];
        }
        if (path.len == 0 or path[0] != '/') return 0;
        return percentDecode(path, out);
    }

    /// Percent-decode into `out`. Returns 0 on a malformed escape, an
    /// embedded NUL, or an overrun: a half-decoded path is a different
    /// directory.
    fn percentDecode(raw: []const u8, out: []u8) usize {
        var written: usize = 0;
        var index: usize = 0;
        while (index < raw.len) {
            if (written == out.len) return 0;
            const byte = raw[index];
            if (byte == '%') {
                if (index + 2 >= raw.len) return 0;
                const hi = std.fmt.charToDigit(raw[index + 1], 16) catch return 0;
                const lo = std.fmt.charToDigit(raw[index + 2], 16) catch return 0;
                const decoded = hi * 16 + lo;
                if (decoded == 0) return 0;
                out[written] = decoded;
                index += 3;
            } else {
                if (byte == 0) return 0;
                out[written] = byte;
                index += 1;
            }
            written += 1;
        }
        return written;
    }

    pub fn cols(session: *const Session) u16 {
        return @intCast(session.term.cols);
    }

    pub fn rows(session: *const Session) u16 {
        return @intCast(session.term.rows);
    }

    /// Resize (with reflow). False on allocation failure, so the caller keeps
    /// its dimensions and retries rather than disagreeing with the pty.
    pub fn resize(session: *Session, new_cols: u16, new_rows: u16) bool {
        const c: vt.size.CellCountInt = @intCast(std.math.clamp(@as(usize, new_cols), 2, max_cols));
        const r: vt.size.CellCountInt = @intCast(std.math.clamp(@as(usize, new_rows), 2, max_rows));
        if (c == session.term.cols and r == session.term.rows) return true;
        session.term.resize(session.gpa, .{ .cols = c, .rows = r }) catch return false;
        session.screen_text_dirty = true;
        // Reflow invalidates keyboard-selection coordinates: re-anchor at the
        // clamped head and drop the stale range.
        if (session.select_anchor != null) {
            session.select_head = .{
                .x = @intCast(@min(@as(usize, session.select_head.x), @as(usize, session.term.cols) - 1)),
                .y = @intCast(@min(@as(usize, session.select_head.y), @as(usize, session.term.rows) - 1)),
            };
            session.select_anchor = session.select_head;
            session.applySelection();
        }
        // Reflow can replace the pages search results point into.
        if (session.search.open) session.searchRefresh();
        return true;
    }

    /// Clamp a proposed grid to allocation and command-id geometry bounds.
    /// Distinct-glyph, text, path, and command budgets are fenced at paint.
    pub fn clampGrid(proposed_cols: usize, proposed_rows: usize) Session.CellPos {
        return .{
            .x = @intCast(std.math.clamp(proposed_cols, 2, max_cols)),
            .y = @intCast(std.math.clamp(proposed_rows, 2, max_rows)),
        };
    }

    // ---------------------------------------------------- scrollback

    /// Scroll the viewport into history (negative = toward the top).
    pub fn scrollLines(session: *Session, delta: isize) void {
        session.scrollTracked(.{ .delta_row = delta });
    }

    pub fn scrollToBottom(session: *Session) void {
        session.scrollTracked(.{ .active = {} });
    }

    pub fn scrollToTop(session: *Session) void {
        session.scrollTracked(.{ .top = {} });
    }

    /// Put `pin`'s row at the top of the viewport.
    pub fn scrollToPin(session: *Session, pin: vt.Pin) void {
        session.scrollTracked(.{ .pin = pin });
    }

    /// Put the viewport back on the absolute row `offset` — the coordinate
    /// `scrollbar().offset` reports, so a saved offset restores exactly.
    pub fn scrollToRow(session: *Session, offset: usize) void {
        session.scrollTracked(.{ .row = offset });
    }

    /// Every scroll goes through here so a scroll that moved the viewport
    /// invalidates the cached screen text (and a no-op does not).
    fn scrollTracked(session: *Session, behavior: vt.PageList.Scroll) void {
        const before = session.scrollbar().offset;
        session.term.screens.active.pages.scroll(behavior);
        if (session.scrollbar().offset != before) session.refreshScreenText();
    }

    /// Rows of history above the viewport (0 = pinned to the live
    /// screen) plus the total row count, for the scroll indicator.
    pub fn scrollbar(session: *Session) vt.PageList.Scrollbar {
        return session.term.screens.active.pages.scrollbar();
    }

    // ---------------------------------------------- scrollback search

    /// Open the search field. Remembers where the viewport was so Escape can
    /// put it back.
    pub fn searchOpen(session: *Session) void {
        if (session.search.open) return;
        const bar = session.scrollbar();
        // A viewport at the live bottom restores to the bottom, not a row.
        session.search.restore_bottom = bar.offset + bar.len >= bar.total;
        session.search.restore_row = bar.offset;
        session.search.open = true;
    }

    /// Dismiss the field, drop the engine, and restore the pre-search viewport.
    pub fn searchClose(session: *Session) void {
        if (!session.search.open) return;
        session.search.open = false;
        session.search.needle_len = 0;
        session.discardSearchEngine();
        if (session.search.restore_bottom) {
            session.scrollToBottom();
        } else {
            session.scrollToRow(session.search.restore_row);
        }
    }

    pub fn searchNeedle(session: *const Session) []const u8 {
        return session.search.needle_buf[0..session.search.needle_len];
    }

    /// Append typed text and re-run the search. False (never a truncation)
    /// at the ceiling or for control bytes.
    pub fn searchInput(session: *Session, text: []const u8) bool {
        if (!session.search.open or text.len == 0) return false;
        for (text) |byte| if (byte < 0x20 or byte == 0x7f) return false;
        if (session.search.needle_len + text.len > session.search.needle_buf.len) return false;
        @memcpy(session.search.needle_buf[session.search.needle_len..][0..text.len], text);
        session.search.needle_len += text.len;
        session.searchRefresh();
        return true;
    }

    /// Insert clipboard text: the first line with control bytes dropped
    /// (pastes routinely carry a trailing newline, and matches never span rows).
    pub fn searchPaste(session: *Session, text: []const u8) bool {
        if (!session.search.open) return false;
        var end: usize = 0;
        while (end < text.len and text[end] != '\n' and text[end] != '\r') end += 1;
        var wrote = false;
        for (text[0..end]) |byte| {
            if (byte < 0x20 or byte == 0x7f) continue;
            if (session.search.needle_len + 1 > session.search.needle_buf.len) break;
            session.search.needle_buf[session.search.needle_len] = byte;
            session.search.needle_len += 1;
            wrote = true;
        }
        if (wrote) session.searchRefresh();
        return wrote;
    }

    /// Delete the last scalar (not byte) of the needle.
    pub fn searchBackspace(session: *Session) bool {
        if (!session.search.open or session.search.needle_len == 0) return false;
        var end = session.search.needle_len - 1;
        while (end > 0 and session.search.needle_buf[end] & 0xC0 == 0x80) end -= 1;
        session.search.needle_len = end;
        session.searchRefresh();
        return true;
    }

    /// Step to the next or previous match; false when there is none.
    pub fn searchStep(session: *Session, forward: bool) bool {
        if (!session.search.open) return false;
        if (session.searchScreenStale()) session.searchRefresh();
        const engine = if (session.search.engine) |*value| value else return false;
        const moved = engine.select(if (forward) .next else .prev) catch return false;
        if (!moved) return false;
        session.revealCurrentMatch();
        return true;
    }

    /// Matches found for the current needle.
    pub fn searchMatchCount(session: *const Session) usize {
        const engine = if (session.search.engine) |*value| value else return 0;
        return engine.matchesLen();
    }

    /// The current match in reading order (1 = oldest), or 0. The engine
    /// indexes from the newest.
    pub fn searchMatchOrdinal(session: *const Session) usize {
        const engine = if (session.search.engine) |*value| value else return 0;
        const selected = engine.selected orelse return 0;
        const total = engine.matchesLen();
        if (selected.idx >= total) return 0;
        return total - selected.idx;
    }

    /// Rebuild the engine for the current needle and land on a match.
    fn searchRefresh(session: *Session) void {
        session.discardSearchEngine();
        const needle = session.searchNeedle();
        if (needle.len == 0) return;
        const screens = &session.term.screens;
        session.search.engine = vt.search.Screen.init(session.gpa, screens.active, needle) catch return;
        // Recorded first so a failure-path teardown knows the screen.
        session.search.screen_key = screens.active_key;
        session.search.screen_generation = screens.generation(screens.active_key);
        // Library-built libghostty-vt has no search thread, so walk one bounded
        // slice (active screen first) inline and let `searchPump` finish per
        // frame; a whole-scrollback walk per keystroke stutters.
        session.search.incomplete = true;
        session.search.landed = false;
        _ = session.searchPump(search_first_slice_steps);
    }

    /// Search ticks per slice (one tick is roughly one PageList page). Four
    /// inline steps finish an ordinary history on the keystroke; 32 per frame
    /// finishes a 500k-row history in well under a second.
    pub const search_first_slice_steps: usize = 4;
    pub const search_frame_slice_steps: usize = 32;

    /// Make bounded search progress; true means pump again next frame. A
    /// failure discards the engine so a stalled partial result never reads
    /// as complete.
    pub fn searchPump(session: *Session, budget: usize) bool {
        if (!session.search.open or !session.search.incomplete) return false;
        const engine = if (session.search.engine) |*value| value else {
            session.search.incomplete = false;
            return false;
        };
        var steps: usize = 0;
        while (steps < budget) : (steps += 1) {
            engine.tick() catch |err| switch (err) {
                error.OutOfMemory => {
                    session.discardSearchEngine();
                    session.search.incomplete = false;
                    return false;
                },
                error.FeedRequired => engine.feed() catch {
                    session.discardSearchEngine();
                    session.search.incomplete = false;
                    return false;
                },
                error.SearchComplete => {
                    session.search.incomplete = false;
                    break;
                },
            };
        }
        // Land on the first match once; later matches must not move the viewport.
        if (!session.search.landed and engine.matchesLen() > 0) {
            _ = engine.select(.next) catch {};
            session.search.landed = true;
            session.revealCurrentMatch();
        }
        return session.search.incomplete;
    }

    /// Whether this session owes the frame pump more search work.
    pub fn searchPending(session: *const Session) bool {
        return session.search.open and session.search.incomplete;
    }

    /// Clear then push match highlights, current match first (ghostty's
    /// order): `updateHighlightsFlattened` appends and never clears.
    fn applySearchHighlights(session: *Session) void {
        const row_data = session.render.row_data.slice();
        for (row_data.items(.highlights), row_data.items(.dirty)) |*hls, *dirty| {
            if (hls.items.len == 0) continue;
            hls.clearRetainingCapacity();
            dirty.* = true;
        }
        if (!session.search.open or session.searchScreenStale()) return;
        const engine = if (session.search.engine) |*value| value else return;
        if (engine.selectedMatch()) |current| {
            session.render.updateHighlightsFlattened(
                session.gpa,
                search_current_tag,
                &.{current},
            ) catch {};
        }
        // Only the slice is ours; the highlights stay owned by the search.
        const all = engine.matches(session.gpa) catch return;
        defer session.gpa.free(all);
        session.render.updateHighlightsFlattened(session.gpa, search_match_tag, all) catch {};
    }

    /// Scroll to the current match only when it is off screen.
    fn revealCurrentMatch(session: *Session) void {
        const engine = if (session.search.engine) |*value| value else return;
        const match = engine.selectedMatch() orelse return;
        const screen = session.term.screens.active;
        const start = match.startPin();
        if (screen.pages.pointFromPin(.viewport, start) != null) return;
        session.scrollToPin(start);
    }

    /// `deinit` untracks pins in the screen's pool; once the screen is gone
    /// (left alternate screen) only `deinitScreenInvalid` is safe.
    fn discardSearchEngine(session: *Session) void {
        session.search.incomplete = false;
        session.search.landed = false;
        const engine = if (session.search.engine) |*value| value else return;
        if (session.searchScreenAlive()) engine.deinit() else engine.deinitScreenInvalid();
        session.search.engine = null;
    }

    /// Whether the screen the engine pinned into still exists, unreplaced.
    fn searchScreenAlive(session: *const Session) bool {
        const screens = &session.term.screens;
        if (screens.get(session.search.screen_key) == null) return false;
        return screens.generation(session.search.screen_key) == session.search.screen_generation;
    }

    /// Whether the engine no longer describes the screen being PAINTED —
    /// an application swapped to (or back from) the alternate screen under it.
    fn searchScreenStale(session: *const Session) bool {
        if (session.search.engine == null) return true;
        const screens = &session.term.screens;
        return screens.active_key != session.search.screen_key or
            screens.generation(screens.active_key) != session.search.screen_generation;
    }

    // ---------------------------------------------------- selection

    pub fn selectionActive(session: *const Session) bool {
        return session.select_anchor != null or session.term.screens.active.selection != null;
    }

    /// Begin a keyboard selection at the live cursor (origin when scrolled
    /// out of view). `block` selects a rectangle.
    pub fn beginSelection(session: *Session, block: bool) void {
        const screen = session.term.screens.active;
        const anchor: CellPos = blk: {
            if (screen.pages.pointFromPin(.viewport, screen.cursor.page_pin.*)) |point| {
                const coord = point.coord();
                break :blk .{ .x = @intCast(coord.x), .y = @intCast(coord.y) };
            }
            break :blk .{ .x = 0, .y = 0 };
        };
        session.select_anchor = anchor;
        session.select_head = anchor;
        session.select_block = block;
        session.applySelection();
    }

    pub fn toggleSelectionBlock(session: *Session) void {
        if (session.select_anchor == null) return;
        session.select_block = !session.select_block;
        session.applySelection();
    }

    /// Move the selection head one step; `extend` keeps the anchor
    /// (shift held), otherwise anchor follows head (caret move).
    pub fn moveSelection(session: *Session, dx: i32, dy: i32, extend: bool) void {
        if (session.select_anchor == null) return;
        const grid_cols: i32 = @intCast(session.term.cols);
        const grid_rows: i32 = @intCast(session.term.rows);
        var x: i32 = @as(i32, session.select_head.x) + dx;
        var y: i32 = @as(i32, session.select_head.y) + dy;
        x = std.math.clamp(x, 0, grid_cols - 1);
        y = std.math.clamp(y, 0, grid_rows - 1);
        session.select_head = .{ .x = @intCast(x), .y = @intCast(y) };
        if (!extend) session.select_anchor = session.select_head;
        session.applySelection();
    }

    pub fn clearSelection(session: *Session) void {
        session.pointer_selection.reset(&session.term);
        session.select_anchor = null;
        session.term.screens.active.clearSelection();
    }

    /// Select all scrollback (Ghostty's `select_all`) in absolute pins, without
    /// arming keyboard-selection mode, so scrolling stays live. False on an
    /// empty screen, leaving any previous selection alone.
    pub fn selectAllHistory(session: *Session) bool {
        const screen = session.term.screens.active;
        const selection = screen.selectAll() orelse return false;
        session.pointer_selection.reset(&session.term);
        screen.select(selection) catch return false;
        session.select_anchor = null;
        return true;
    }

    /// The painter's last measured cell box, or null before the first paint.
    /// Callers must decline rather than guess.
    pub fn measuredCell(session: *const Session) ?CellBox {
        return session.measured_cell;
    }

    /// Record the painter's measurement. Non-finite or non-positive boxes are
    /// refused (keeping the previous value), so a non-null box is safe to
    /// divide by.
    pub fn setMeasuredCell(session: *Session, width: f32, height: f32) void {
        if (!std.math.isFinite(width) or !std.math.isFinite(height)) return;
        if (width <= 0 or height <= 0) return;
        session.measured_cell = .{ .width = width, .height = height };
    }

    /// Primary-pointer selection using Ghostty's own cell/word/line gesture.
    /// Coordinates are relative to the exact retained terminal-widget frame;
    /// captured drags may extend beyond it and clamp to the nearest edge.
    pub fn pointerSelection(session: *Session, event: PointerSelectionEvent) bool {
        if (!std.math.isFinite(event.x) or !std.math.isFinite(event.y) or
            !std.math.isFinite(event.width) or !std.math.isFinite(event.height) or
            event.width <= 0 or event.height <= 0)
        {
            return false;
        }
        const cell = session.measuredCell() orelse return false;

        const screen = session.term.screens.active;
        switch (event.phase) {
            .down => {
                const pin = session.pointerPin(event) orelse return false;
                session.select_anchor = null;
                session.pointer_selection.reset(&session.term);
                const behavior: vt.SelectionGesture.Behavior = if (event.click_count >= 3)
                    .line
                else if (event.click_count == 2)
                    .word
                else
                    .cell;
                const behaviors = [3]vt.SelectionGesture.Behavior{ behavior, behavior, behavior };
                const selected = session.pointer_selection.press(&session.term, .{
                    .time = null,
                    .pin = pin,
                    .xpos = event.x,
                    .ypos = event.y,
                    .max_distance = cell.width,
                    .repeat_interval = 0,
                    .word_boundary_codepoints = &pointer_word_boundaries,
                    .behaviors = &behaviors,
                }) catch {
                    screen.clearSelection();
                    return true;
                };
                if (selected) |selection| {
                    screen.select(selection) catch screen.clearSelection();
                } else {
                    screen.clearSelection();
                }
                return true;
            },
            .move => return session.applyPointerDrag(event),
            .up => {
                const pin = session.pointerPin(event);
                const changed = session.applyPointerDrag(event);
                session.pointer_selection.release(&session.term, .{ .pin = pin });
                return changed;
            },
            .cancel => {
                session.pointer_selection.reset(&session.term);
                return false;
            },
            .hover, .wheel => return false,
        }
    }

    fn applyPointerDrag(session: *Session, event: PointerSelectionEvent) bool {
        const cell = session.measuredCell() orelse return false;
        const pin = session.pointerPin(event) orelse return false;
        const selection = session.pointer_selection.drag(&session.term, .{
            .pin = pin,
            .xpos = event.x,
            .ypos = event.y,
            .rectangle = false,
            .word_boundary_codepoints = &pointer_word_boundaries,
            .geometry = .{
                .columns = session.cols(),
                // Ghostty wants whole columns; at least one for tiny fonts.
                .cell_width = @intFromFloat(@max(1, @round(cell.width))),
                .padding_left = 0,
                .screen_height = @intFromFloat(@max(1, @round(event.height))),
            },
        }) orelse return false;
        session.term.screens.active.select(selection) catch return false;
        return true;
    }

    pub fn pointerAutoscrollActive(session: *const Session) bool {
        return session.pointer_selection.left_drag_autoscroll != .none;
    }

    /// Advance an edge drag by one Ghostty-owned row. The host supplies the
    /// cadence; one invocation is deliberately bounded to one viewport row.
    pub fn pointerAutoscroll(session: *Session, event: PointerAutoscrollEvent) bool {
        if (!session.pointerAutoscrollActive() or
            !std.math.isFinite(event.x) or !std.math.isFinite(event.y) or
            !std.math.isFinite(event.width) or !std.math.isFinite(event.height) or
            event.width <= 0 or event.height <= 0)
        {
            return false;
        }
        const cell = session.measuredCell() orelse return false;
        const coordinate = session.pointerViewportCoordinate(event.x, event.y) orelse return false;
        const selection = session.pointer_selection.autoscrollTick(&session.term, .{
            .viewport = coordinate,
            .xpos = event.x,
            .ypos = event.y,
            .rectangle = false,
            .word_boundary_codepoints = &pointer_word_boundaries,
            .geometry = .{
                .columns = session.cols(),
                .cell_width = @intFromFloat(@max(1, @round(cell.width))),
                .padding_left = 0,
                .screen_height = @intFromFloat(@max(1, @round(event.height))),
            },
        }) orelse return false;
        session.term.screens.active.select(selection) catch return false;
        session.refreshScreenText();
        return true;
    }

    fn pointerPin(session: *Session, event: PointerSelectionEvent) ?vt.Pin {
        const coordinate = session.pointerViewportCoordinate(event.x, event.y) orelse return null;
        return session.term.screens.active.pages.pin(.{ .viewport = coordinate });
    }

    /// Widget-relative point to viewport cell; null until measured.
    fn pointerViewportCoordinate(session: *const Session, x: f32, y: f32) ?vt.Coordinate {
        const cell = session.measuredCell() orelse return null;
        const cols_count = session.cols();
        const rows_count = session.rows();
        if (cols_count == 0 or rows_count == 0) return null;
        const max_x: f32 = @floatFromInt(cols_count - 1);
        const max_y: f32 = @floatFromInt(rows_count - 1);
        const cell_x = std.math.clamp(@floor(x / cell.width), 0, max_x);
        const cell_y = std.math.clamp(@floor(y / cell.height), 0, max_y);
        return .{
            .x = @intFromFloat(cell_x),
            .y = @intFromFloat(cell_y),
        };
    }

    /// Re-derive viewport selection coordinates from the emulator's absolute
    /// pins after output moved the screen. A range that left the viewport is
    /// cleared. Returns whether a selection is still armed.
    pub fn rebaseSelection(session: *Session) bool {
        if (session.select_anchor == null) return false;
        const screen = session.term.screens.active;
        const selection = screen.selection orelse {
            session.clearSelection();
            return false;
        };
        const anchor_point = screen.pages.pointFromPin(.viewport, selection.start()) orelse {
            session.clearSelection();
            return false;
        };
        const head_point = screen.pages.pointFromPin(.viewport, selection.end()) orelse {
            session.clearSelection();
            return false;
        };
        const anchor_coord = anchor_point.coord();
        const head_coord = head_point.coord();
        session.select_anchor = .{ .x = @intCast(anchor_coord.x), .y = @intCast(anchor_coord.y) };
        session.select_head = .{ .x = @intCast(head_coord.x), .y = @intCast(head_coord.y) };
        return true;
    }

    fn applySelection(session: *Session) void {
        const anchor = session.select_anchor orelse return;
        const screen = session.term.screens.active;
        // On failure clear rather than keep a stale range a copy would read.
        const tl = screen.pages.pin(.{ .viewport = .{ .x = anchor.x, .y = anchor.y } }) orelse {
            screen.clearSelection();
            return;
        };
        const br = screen.pages.pin(.{ .viewport = .{ .x = session.select_head.x, .y = session.select_head.y } }) orelse {
            screen.clearSelection();
            return;
        };
        screen.select(vt.Selection.init(tl, br, session.select_block)) catch screen.clearSelection();
    }

    /// The selected text, caller-owned. Null means nothing is selected; a
    /// serialization failure is an error so the copy can report it.
    pub fn selectionText(session: *Session, gpa: std.mem.Allocator) !?[:0]const u8 {
        const screen = session.term.screens.active;
        const selection = screen.selection orelse return null;
        return try screen.selectionString(gpa, .{ .sel = selection, .trim = true });
    }

    /// The viewport as plain text — the test and automation view of the
    /// grid (real cell state, no pixels).
    pub fn plainText(session: *Session, gpa: std.mem.Allocator) ![]const u8 {
        return session.term.plainString(gpa);
    }

    /// Invalidate the cached viewport text; `screenText` recomputes lazily.
    pub fn refreshScreenText(session: *Session) void {
        session.screen_text_dirty = true;
    }

    /// `linkAtPoint`'s URL only (same lifetime rule).
    pub fn urlAtPoint(session: *Session, x: f32, y: f32) ?[]const u8 {
        const link = session.linkAtPoint(x, y) orelse return null;
        return link.url;
    }

    /// The link under a view-relative point. An OSC 8 href beats the text
    /// heuristic (`url.zig`), with two safeguards against program output
    /// choosing what the OS opens:
    ///  1. The href must pass the same allowlist the heuristic produces; a
    ///     refused href falls back to the visible link.
    ///  2. When the visible text is a different URL, the href wins only if its
    ///     exact cell+target preview was actually rendered (closing OSC 8's
    ///     phishing gap); otherwise the visible URL opens.
    /// Reads the live emulator, not render-state pins.
    pub fn linkAtPoint(session: *Session, x: f32, y: f32) ?Link {
        if (!std.math.isFinite(x) or !std.math.isFinite(y)) return null;
        if (x < 0 or y < 0) return null;
        const coordinate = session.pointerViewportCoordinate(x, y) orelse return null;
        return session.linkAtCell(coordinate);
    }

    /// `linkAtPoint` once the point is already a viewport cell.
    fn linkAtCell(session: *Session, coordinate: vt.Coordinate) ?Link {
        const shown = session.textLinkAtCell(coordinate);
        if (session.hyperlinkUriAtCell(coordinate)) |href| explicit: {
            if (url_module.targetIdentity(href) == null) break :explicit;
            if (shown) |text_link| {
                if (!std.mem.eql(u8, text_link.url, href) and !session.previewWasRendered(coordinate, href)) break :explicit;
            }
            return .{
                .url = session.rememberLink(href) orelse return null,
                .source = .osc8,
                .row = @intCast(coordinate.y),
                .start_col = @intCast(coordinate.x),
                .end_col = @intCast(coordinate.x + 1),
            };
        }
        const text_link = shown orelse return null;
        return .{
            .url = session.rememberLink(text_link.url) orelse return null,
            .source = .text,
            .row = text_link.row,
            .start_col = text_link.start_col,
            .end_col = text_link.end_col,
        };
    }

    /// The heuristic link at a viewport cell. Its `url` points into the cached
    /// screen text and is only safe until the caller copies it.
    fn textLinkAtCell(session: *Session, coordinate: vt.Coordinate) ?Link {
        const text = session.screenText();
        const row = rowSlice(text, coordinate.y) orelse return null;
        const offset = url_module.byteOffsetForColumn(row, coordinate.x) orelse return null;
        const span = url_module.spanAt(row, offset) orelse return null;
        return .{
            .url = span.slice(row),
            .source = .text,
            .row = @intCast(coordinate.y),
            .start_col = @intCast(url_module.columnForByteOffset(row, span.start)),
            .end_col = @intCast(url_module.columnForByteOffset(row, span.end)),
        };
    }

    /// The OSC 8 target on a viewport cell (page memory; callers copy). Reads
    /// the live screen, so it is safe outside a paint.
    fn hyperlinkUriAtCell(session: *Session, coordinate: vt.Coordinate) ?[]const u8 {
        const screen = session.term.screens.active;
        const list_cell = screen.pages.getCell(.{ .viewport = .{
            .x = coordinate.x,
            .y = coordinate.y,
        } }) orelse return null;
        if (!list_cell.cell.hyperlink) return null;
        const page = list_cell.node.page();
        const id = page.lookupHyperlink(list_cell.cell) orelse return null;
        return page.hyperlink_set.get(page.memory, id).uri.slice(page.memory);
    }

    /// The OSC 8 target at column `x` of a render-state row pin. Only safe
    /// inside `snapshot`, while the render state is current.
    fn cellHyperlinkUri(pin: vt.Pin, x: usize) ?[]const u8 {
        const page = pin.node.page();
        if (x >= page.size.cols) return null;
        const rac = page.getRowAndCell(@intCast(x), pin.y);
        if (!rac.cell.hyperlink) return null;
        const id = page.lookupHyperlink(rac.cell) orelse return null;
        return page.hyperlink_set.get(page.memory, id).uri.slice(page.memory);
    }

    /// Copy a target into session storage; null (no link) if it does not fit,
    /// since a truncated URL is a different destination.
    fn rememberLink(session: *Session, value: []const u8) ?[]const u8 {
        if (value.len == 0 or value.len > session.link_buf.len) return null;
        @memcpy(session.link_buf[0..value.len], value);
        session.link_len = value.len;
        return session.link_buf[0..session.link_len];
    }

    /// Arm or disarm the hover underline; returns whether it changed.
    pub fn setHoverPoint(session: *Session, point: ?HoverPoint) bool {
        const before = session.hover_point;
        session.hover_point = point;
        if (before == null and point == null) return false;
        if (before) |old| {
            if (point) |new| return old.x != new.x or old.y != new.y;
        }
        return true;
    }

    /// Track the pane-relative pointer for target preview independently of the
    /// Cmd underline. Moving it invalidates any receipt from the prior cell.
    pub fn setPointerPoint(session: *Session, point: ?HoverPoint) bool {
        const before = session.pointer_point;
        if (before == null and point == null) return false;
        if (before) |old| if (point) |new| {
            if (old.x == new.x and old.y == new.y) return false;
        };
        session.pointer_point = point;
        session.invalidateRenderedPreview();
        return true;
    }

    /// The allowlisted OSC 8 destination under the pointer, copied, for the
    /// preview and accessibility label. Heuristic links are already visible.
    pub fn hoveredOsc8Target(session: *Session) ?[]const u8 {
        const point = session.pointer_point orelse return null;
        const coordinate = session.pointerViewportCoordinate(point.x, point.y) orelse return null;
        const href = session.hyperlinkUriAtCell(coordinate) orelse return null;
        if (url_module.targetIdentity(href) == null) return null;
        if (href.len > session.preview_target_buf.len) return null;
        @memcpy(session.preview_target_buf[0..href.len], href);
        session.preview_target_len = href.len;
        return session.preview_target_buf[0..session.preview_target_len];
    }

    pub fn markOsc8PreviewRendered(session: *Session, target: []const u8) void {
        const identity = url_module.targetIdentity(target) orelse return;
        if (identity.effective_authority == null) return;
        const point = session.pointer_point orelse return;
        const coordinate = session.pointerViewportCoordinate(point.x, point.y) orelse return;
        const live = session.hyperlinkUriAtCell(coordinate) orelse return;
        if (!std.mem.eql(u8, live, target) or target.len > session.preview_rendered_buf.len) return;
        @memcpy(session.preview_rendered_buf[0..target.len], target);
        session.preview_rendered_len = target.len;
        session.preview_rendered_cell = .{ .x = @intCast(coordinate.x), .y = @intCast(coordinate.y) };
    }

    fn previewWasRendered(session: *const Session, coordinate: vt.Coordinate, target: []const u8) bool {
        const cell = session.preview_rendered_cell orelse return false;
        return cell.x == coordinate.x and cell.y == coordinate.y and
            std.mem.eql(u8, session.preview_rendered_buf[0..session.preview_rendered_len], target);
    }

    fn invalidateRenderedPreview(session: *Session) void {
        session.preview_rendered_cell = null;
        session.preview_rendered_len = 0;
    }

    /// The link under the hover point, resolved during `snapshot`.
    fn hoverLink(session: *Session) ?Link {
        const point = session.hover_point orelse return null;
        return session.linkAtPoint(point.x, point.y);
    }

    /// Row `index` of a newline-separated viewport dump, without its newline.
    fn rowSlice(text: []const u8, index: usize) ?[]const u8 {
        var start: usize = 0;
        var row: usize = 0;
        while (row < index) : (row += 1) {
            const newline = std.mem.indexOfScalarPos(u8, text, start, '\n') orelse return null;
            start = newline + 1;
        }
        const end = std.mem.indexOfScalarPos(u8, text, start, '\n') orelse text.len;
        return text[start..end];
    }

    /// The cached viewport text, recomputed on demand when the screen
    /// moved under it (see `refreshScreenText`).
    pub fn screenText(session: *Session) []const u8 {
        if (session.screen_text_dirty) session.renderScreenText();
        return session.screen_text_buf[0..session.screen_text_len];
    }

    /// Serialize the viewport into the reused text buffer.
    fn renderScreenText(session: *Session) void {
        const screen = session.term.screens.active;
        var writer: std.Io.Writer.Allocating = .initOwnedSlice(session.gpa, session.screen_text_buf);
        // Reclaim the buffer on every exit path.
        defer session.screen_text_buf = writer.writer.buffer;
        session.screen_text_dirty = false;
        session.screen_text_len = 0;
        const br = screen.pages.getBottomRight(.viewport) orelse return;
        // On failure the text stays empty (unknown), never the stale screen.
        screen.dumpString(&writer.writer, .{
            .tl = screen.pages.getTopLeft(.viewport),
            .br = br,
            .unwrap = false,
        }) catch return;
        session.screen_text_len = writer.writer.end;
    }
};
