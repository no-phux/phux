//! One libghostty emulator session and its canvas-grid projection.
//!
//! libghostty-vt owns cell state, damage, scrollback, and selection. This
//! module owns the session and PROJECTS its viewport into a
//! `canvas.TerminalGrid` (see `Session.snapshot`), which
//! `canvas.terminal_grid.paint` turns into pixels. The palette is
//! theme-derived where the emulator still holds its defaults (the honest
//! ANSI-16 story) and exact everywhere an application chose a color: the
//! 256-color cube, the grayscale ramp, and truecolor pass through
//! untouched.
//!
//! It used to emit canvas commands itself — run merging, per-row ids,
//! glyph and text accounting, box-drawing geometry, roughly 375 lines of
//! it. All of that is the framework's now. What stays here is the local
//! provider's libghostty session and its byte-injection path. The framework's
//! own session store (`runtime/terminal_session.zig`) is capped at four ptys and
//! has no inbound byte-injection path. Other providers project their
//! authoritative model straight into the shared painter below.

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

/// OSC 0/2 title ceiling. libghostty-vt already clamps a title at 1024 bytes
/// (`stream_terminal.zig` `windowTitle`) purely as a DoS bound; this is the
/// display bound. Nothing renders a 256-byte tab label, and a title is a
/// hint, not a document — so the copy truncates (at a scalar boundary) rather
/// than failing and leaving the pane titleless.
pub const max_title_bytes: usize = 256;

/// Scrollback-search needle ceiling. A needle is a phrase, not a document:
/// 128 bytes holds any real one and keeps the whole search state inline on
/// the session rather than adding another heap allocation per terminal.
pub const max_search_needle_bytes: usize = 128;

/// The tags handed to `RenderState.updateHighlightsFlattened`. Their VALUES
/// are opaque to the emulator — these two are this projection's own
/// vocabulary — but the ORDER they are applied in is load-bearing: the cell
/// loop takes the FIRST range covering a cell, so the current match must be
/// pushed before the full match list or a cell that is both would paint as
/// merely a match. See `applySearchHighlights`.
pub const search_current_tag: u8 = 1;
pub const search_match_tag: u8 = 2;

/// One terminal's scrollback search.
///
/// It lives on the SESSION, not on the model, for two reasons. The engine
/// holds pins and page pointers into THIS emulator's `PageList`, so a search
/// that outlived its screen would be reading freed pages; and per-session
/// storage is what makes search state terminal-local BY CONSTRUCTION rather
/// than by a lookup a new tab or pane could get wrong.
pub const Search = struct {
    /// The field is showing. Independent of whether anything is typed —
    /// cmd+F opens it empty, and an empty field is not a failed search.
    open: bool = false,
    needle_buf: [max_search_needle_bytes]u8 = undefined,
    needle_len: usize = 0,
    /// The live engine, rebuilt whenever the needle changes. Null while the
    /// needle is empty: libghostty's own sliding window treats an empty
    /// needle as an inactive search, and "no search" must not read as "no
    /// matches".
    engine: ?vt.search.Screen = null,
    /// Which SCREEN the engine pinned into. An application entering or
    /// leaving the alternate screen swaps `screens.active` and can DESTROY
    /// the screen it left, taking that screen's pin pool with it — so the
    /// engine has to be rebuilt, and torn down through the teardown the
    /// screen's state actually allows (see `discardSearchEngine`).
    screen_key: vt.ScreenSet.Key = .primary,
    screen_generation: usize = 0,
    /// The engine has not finished walking the scrollback yet, so a caller
    /// should keep pumping it. See `Session.searchPump`.
    incomplete: bool = false,
    /// Whether this needle has already been landed on a match. The first match
    /// to appear takes the selection; later ones stream in without yanking the
    /// viewport out from under someone who is already reading.
    landed: bool = false,
    /// Where the viewport was when the field opened. Escape puts it back.
    restore_row: usize = 0,
    /// ...and whether that was the LIVE bottom, which is not the same
    /// absolute row once output has arrived underneath the search.
    restore_bottom: bool = true,
};

/// A MEASURED terminal cell, in canvas points. Both fields are finite and
/// strictly positive by construction: `Session.setMeasuredCell` is the only
/// way to build one, and it refuses anything else.
///
/// This exists as a distinct type, reachable only through an optional, because
/// the alternative cost real bugs. The fields used to be
/// `cell_width: f32 = 8, cell_height: f32 = 18` directly on `Session`, and the
/// PAINTER was their only writer — so every reader that ran before the first
/// paint silently got "the estimate for font-size 13" while believing it had a
/// measurement. `round(13 * 1.4) = 18` and `round(13 * 0.6) = 8` are exactly
/// those defaults, which is why the defect was invisible at the default font
/// and drew the prompt at three different widths at `font-size = 26`.
///
/// Three separate `if (cell_width <= 0 or cell_height <= 0)` guards had been
/// written to catch precisely that, and not one of them could ever fire: 8 and
/// 18 are both positive. Making the default `0` so the guards fire would have
/// been worse — several sites DIVIDE by these fields, so a plausible-looking
/// zero is a division by zero waiting for the one caller who forgets. The
/// optional moves the check from something every future caller must remember
/// into something the compiler will not let them skip, and the validating
/// setter means "not null" already implies "safe to divide by".
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
    /// Terminal answers to queries (DSR, DA1, XTVERSION, ...) produced
    /// while feeding output; the app drains this after every feed and
    /// writes it back to the pty. Heap-allocated and GROWN TO FIT (up to
    /// `response_capacity_max`): replies may sit retained here while the
    /// app's outbound ring is full, and further output keeps feeding —
    /// its replies must accumulate, not evaporate. A reply past the max
    /// (a child that ignored the whole pending ring while pipelining
    /// queries) is dropped WHOLE (never cut, which would desync the
    /// child's parser) and counted — the honest record that a reply was
    /// lost, checkable by the app.
    response_buffer: []u8 = &.{},
    response_len: usize = 0,
    response_bytes_dropped: u64 = 0,
    /// Cached viewport plain text (see `screenText`): the grid's
    /// accessibility surface and the fingerprint's cell-state coverage.
    /// Heap-owned, GROWN TO FIT, and reused across refreshes — the
    /// serialization is O(viewport) and used to run on every pty output
    /// batch, for background tabs nobody paints, reallocating each time.
    /// It is now computed LAZILY: writers mark `screen_text_dirty` and
    /// only `screenText` pays, so a session that is never read never
    /// serializes. The stored text is EXACT (never truncated or cut
    /// mid-scalar): what paints is what assistive tech reads and what
    /// the fingerprint hashes. A zero length means "unknown", never
    /// "same as before".
    screen_text_buf: []u8 = &.{},
    screen_text_len: usize = 0,
    /// The screen changed since the cached text was produced. Starts set
    /// so a session read before any feed still serializes once.
    screen_text_dirty: bool = true,
    /// Keyboard-selection state: the anchor stays put, the head moves.
    select_anchor: ?CellPos = null,
    select_head: CellPos = .{},
    select_block: bool = false,
    /// Ghostty's native pointer-selection gesture. Its pins survive output
    /// and viewport movement while a drag is active; the model only owns
    /// which terminal/generation receives subsequent pointer phases.
    pointer_selection: vt.SelectionGesture = .init,
    /// The mono face's cell box at the terminal type size, or NULL until the
    /// painter has actually measured it. See `CellBox` for why this is an
    /// optional rather than a pair of floats with plausible defaults.
    measured_cell: ?CellBox = null,
    font_size: f32 = 13,
    /// The WCAG contrast ratio a cell's foreground must clear against its
    /// background before `snapshot` replaces it with pure white or pure black.
    /// See `Palette.contrasted`.
    ///
    /// DEFAULT 1 — no floor — even though the user-facing default is 3. This
    /// field is the RENDERING policy, and a session that nobody is painting
    /// (a test, a headless feed, a fingerprint) has no rendering policy to
    /// speak of. Defaulting it to the config value would silently colour-shift
    /// every such caller, and the shift is invisible in exactly the places
    /// that check colours least. `render.paint` writes the real value from the
    /// live config on every frame; that is the ONE path to glass.
    minimum_contrast: f32 = 1,
    /// Reused projection buffers for `snapshot` (see that function). The
    /// first-party painter takes a RESOLVED snapshot whose every slice
    /// must outlive the build referencing it, so these are owned per
    /// session and rewritten in place each frame — never allocated per
    /// paint. Sized to the painter's own ceilings, so a snapshot this
    /// session can produce is always one the painter can hold.
    snap_rows: []canvas.TerminalRow = &.{},
    snap_cells: []canvas.TerminalCell = &.{},
    snap_text: []u8 = &.{},
    snap_text_len: usize = 0,
    /// The child's last OSC 0/2 title, COPIED out of the emulator at the
    /// moment it changed. The emulator's own storage is only valid until the
    /// next `setTitle` or reset, and a tab label is read on every paint —
    /// long after the feed that set it — so the session owns a snapshot
    /// instead of holding a borrowed slice. Fixed-size and truncating: see
    /// `max_title_bytes`. Zero length means "never reported", never "empty
    /// title" — the two are indistinguishable to a caller choosing a
    /// fallback, and the fallback is right for both.
    title_buf: [max_title_bytes]u8 = undefined,
    title_len: usize = 0,
    /// The child's last OSC 7 directory, DECODED to a plain filesystem path
    /// (see `decodePwdUrl`). Sized to the platform path ceiling because that
    /// is exactly what it holds: a path a spawn can `cd` into. Zero length
    /// means "unknown" — never reported, or reported in a form this decoder
    /// refused.
    pwd_buf: [std.fs.max_path_bytes]u8 = undefined,
    pwd_len: usize = 0,
    /// BEL arrived since the app last cleared it. A latch, not a sound: the
    /// session makes no noise and has no timer — the app reads it to mark a
    /// tab as wanting attention and clears it when the user looks.
    bell_rung: bool = false,
    /// This terminal's scrollback search. See `Search`.
    search: Search = .{},
    /// The last link `linkAtPoint` resolved, COPIED here.
    ///
    /// Both sources need it. An OSC 8 href lives in PAGE memory, which the
    /// next feed may reflow, prune, or move outright; the heuristic's slice
    /// points into the cached screen text, which the next screen change
    /// rewrites. Neither is a lifetime a caller can reason about, and both
    /// callers (the click that opens, the frame that underlines) read after
    /// the fact. One session-owned copy gives every caller the same rule: the
    /// slice is valid until the next `linkAtPoint`.
    link_buf: [url_module.max_url_bytes]u8 = undefined,
    link_len: usize = 0,
    /// Where the pointer is while the link chord is held over this pane, in
    /// widget-relative canvas points, or null when nothing should be
    /// underlined.
    ///
    /// A POINT, not a resolved span, and that is the whole design. The
    /// underline is resolved inside `snapshot`, against the same live state
    /// the frame is painting, so output that scrolls under a stationary
    /// pointer moves the underline with the text instead of leaving it on the
    /// cells the link used to occupy.
    hover_point: ?HoverPoint = null,
    /// Pointer location independent of modifiers. The OSC 8 target preview is
    /// ordinary hover UI, so pressing Cmd while stationary cannot leave policy
    /// dependent on whether the platform happens to synthesize a move event.
    pointer_point: ?HoverPoint = null,
    /// Receipt for the exact OSC 8 target that completed display-list emission.
    /// A mismatched visible URL may use its href only while this cell+target
    /// receipt still matches; hover state by itself is never proof of display.
    preview_rendered_cell: ?CellPos = null,
    preview_rendered_buf: [url_module.max_url_bytes]u8 = undefined,
    preview_rendered_len: usize = 0,
    /// Separate from `link_buf`: snapshot resolves the underline after the
    /// preview target is borrowed and may resolve visible-text fallback into
    /// `link_buf`. Sharing storage silently spliced the two URLs.
    preview_target_buf: [url_module.max_url_bytes]u8 = undefined,
    preview_target_len: usize = 0,

    pub const CellPos = struct { x: u16 = 0, y: u16 = 0 };

    pub const HoverPoint = struct { x: f32, y: f32 };

    /// Where a resolved link came from.
    ///
    /// Worth carrying rather than collapsing to a URL, because the two have
    /// different trust stories and the caller may want to say so: `.osc8` is a
    /// target the PROGRAM named, `.text` is one this app recognised in bytes
    /// the user can see.
    pub const LinkSource = enum { osc8, text };

    /// A link under a point: what would open, and which cells say so.
    ///
    /// `url` is session-owned (see `link_buf`). The column range is the
    /// viewport row and half-open column span the link OCCUPIES, which for
    /// `.text` is the matched run and for `.osc8` is only the cell that was
    /// hit — an OSC 8 run is found by walking cells, not by measuring text,
    /// so `snapshot` re-derives the full extent by target equality rather
    /// than trusting a range computed at hover time.
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

    /// The growth ceiling — matched to the app's pending-outbound ring:
    /// retained replies past this could never be enqueued whole anyway,
    /// so growing further would only defer the same counted drop.
    pub const response_capacity_max: usize = 64 * 1024;

    /// The app feeds output in sub-slices no larger than this, draining
    /// answers after each, so a burst of pipelined query replies cannot
    /// outrun the response buffer within one feed.
    ///
    /// Matched to the response buffer rather than a fraction of it: the
    /// slice size is a PARSER-THROUGHPUT knob, and a 1 KiB slice split a
    /// routine 64 KiB pty batch into 64 parser entries, each followed by
    /// a full response drain and outbound flush — 64x the per-slice
    /// overhead on the hottest path in the app. The response buffer no
    /// longer needs the safety factor a fixed-capacity buffer did: it
    /// GROWS TO FIT up to `response_capacity_max` (see `writePtyResponse`),
    /// so a slice that does out-answer the initial capacity reallocates
    /// instead of dropping, and `response_bytes_dropped` stays the honest
    /// count should a reply ever overflow the ceiling anyway.
    pub const feed_slice_bytes: usize = response_capacity;

    /// Scrollback ceiling in BYTES (what libghostty's PageList takes),
    /// not lines. The previous 1 MB was roughly a couple of PageList
    /// pages — a few hundred rows — which is not scrollback, it is a
    /// slightly taller screen. 50 MB is Ghostty's own default and holds
    /// the hundreds of thousands of rows a build log or a `git log`
    /// actually produces. The pages are allocated on demand, so an idle
    /// session still costs one page.
    pub const max_scrollback: usize = 50_000_000;

    /// `create` with the default ceiling. Tests and fixtures that do not care
    /// about scrollback size use this; anything holding a user's config should
    /// call `createWithScrollback` so `scrollback-limit` is not silently
    /// ignored.
    pub fn create(gpa: std.mem.Allocator, io: std.Io, initial_cols: u16, initial_rows: u16) !*Session {
        return createWithScrollback(gpa, io, initial_cols, initial_rows, max_scrollback);
    }

    /// `max_scrollback_bytes` is a per-SESSION parameter, not the comptime
    /// constant it used to be. It was a `const` that happened to equal the
    /// config default, which made `scrollback-limit` parse, store, and do
    /// nothing at all.
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

    /// Project the emulator's live viewport into a `canvas.TerminalGrid`
    /// — the resolved, allocation-free snapshot the first-party painter
    /// consumes (`canvas.terminal_grid.paint`).
    ///
    /// The local provider keeps its own `Session` rather than using the
    /// framework terminal widget because the latter has no inbound
    /// byte-injection API. A provider with its own authoritative terminal model
    /// instead supplies `canvas.TerminalGrid` directly to
    /// `paintTerminalGrid`; it never creates a `Session` or second emulator.
    ///
    /// Every returned slice points into this session's own buffers and
    /// stays valid until the next `snapshot` call, which satisfies the
    /// painter's producer-owned-lifetime contract for one frame.
    ///
    /// Box drawing is deliberately NOT special-cased here: the painter
    /// renders box code points as geometry at exact cell bounds under
    /// its own `path_reserve`, so the cluster is left empty for them and
    /// `box.zig`'s hand-rolled segments are no longer needed.
    pub fn snapshot(
        session: *Session,
        tokens: canvas.DesignTokens,
        running: bool,
        selecting: bool,
    ) !canvas.TerminalGrid {
        // Push theme colors into the emulator's DEFAULTS (not the OSC
        // overrides) so ghostty itself composes the final foreground,
        // background, and cursor. Unchanged from the forked painter:
        // one color policy, owned by the emulator.
        //
        // The ANSI-16 palette is deliberately NOT pushed from here. It is
        // the emulator's own (`vt.color.default`, installed by
        // `Terminal.init`) and the projection reads it back verbatim: a
        // terminal red must be a terminal red, not the UI's destructive
        // token. Theming the palette is a `DynamicPalette.changeDefault`
        // call away (it preserves OSC 4 overrides) whenever a real
        // terminal color scheme exists to install; design tokens are not
        // one.
        session.term.colors.foreground.default = themeRgb(tokens.colors.text);
        session.term.colors.background.default = themeRgb(tokens.colors.background);
        session.term.colors.cursor.default = themeRgb(tokens.colors.accent);

        try session.render.update(session.gpa, &session.term);
        // AFTER the render update (which rebuilds — and so clears — the rows
        // it touched) and BEFORE the cell loop reads them.
        session.applySearchHighlights();
        // The hover underline, resolved HERE rather than carried in from the
        // pointer event that armed it, so it is answered against the very
        // state this frame paints. `applySearchHighlights` is the model the
        // bead named, and this deliberately does NOT go through
        // `RenderState.updateHighlightsFlattened`: that channel takes
        // `highlight.Flattened` values, and the pinned libghostty-vt cannot
        // build one. MEASURED, by calling it from a throwaway test:
        //
        //   highlight.zig:151:14: error: no field named 'serial' in struct
        //   'terminal.PageList.PageIterator.Chunk'
        //
        // `Flattened.init` stamps `.serial = chunk.node.serial` onto a chunk
        // type that has no such field, and the same literal goes on to name an
        // `end_x` field where the struct declares `bot_x`. The function is
        // simply never instantiated upstream, so neither error has surfaced.
        // The search path reaches that channel only because the SEARCH engine
        // hands out highlights it built itself. A hover span has no such
        // source, and inventing one to satisfy a channel we then read back out
        // in the same function would be ceremony, not safety.
        const hover = session.hoverLink();
        // Split by SOURCE because the two are found differently. A text link
        // is a column range on one row — the run that was matched. An OSC 8
        // link is wherever its target is, which is a property of the CELLS:
        // the run may wrap, and the same target may appear more than once, so
        // the extent is re-derived per cell by comparing targets rather than
        // trusting a range measured at hover time.
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
                // The projected cell, assembled in place. Everything that
                // does not depend on the code point is resolved up front;
                // `cp` and `cluster` land at the bottom because the
                // invisible flag can still zero the code point.
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
                    // Two kinds of attribute, and the split is the whole
                    // shape of this block.
                    //
                    // COLOR decisions are folded into the resolved
                    // foreground by `resolveFg`, because they have no
                    // separate destination and never will: inverse (fg/bg
                    // swap), faint (blend toward the background), and
                    // bold-as-bright over the ANSI-8 range.
                    //
                    // Everything else is a real field on
                    // `canvas.TerminalCell` and is PROJECTED, not dropped:
                    // bold, italic, strikethrough, overline, the underline
                    // STYLE (single/double/curly/dotted/dashed) and the
                    // underline COLOR. What each one turns into on the
                    // glass is the SDK renderer's call — the decorations
                    // are drawn from `canvas.CellDecoration` geometry,
                    // while bold and italic need registered companion
                    // faces to change the GLYPH and the bundled face is a
                    // single mono one. Carrying the flag is still right:
                    // the cell state is the truth, and a renderer that
                    // grows a weight axis reads it without this projection
                    // changing.
                    //
                    // Cells with no code point are skipped for the same
                    // reason their foreground is: an unwritten cell has no
                    // style to speak of. A styled BLANK is a space
                    // (`cp == 0x20`) in every stream that produces one, so
                    // it lands here like any other character.
                    projected.fg = palette.resolveFg(style, projected.bg, cp);
                    projected.bold = style.flags.bold;
                    projected.italic = style.flags.italic;
                    projected.strikethrough = style.flags.strikethrough;
                    projected.overline = style.flags.overline;
                    projected.underline = style.flags.underline != .none;
                    // The SDK reads `underline_style` only when
                    // `underline` is set, so libghostty's `.none` maps to
                    // the SDK's own default rather than inventing a
                    // "styled but not underlined" state the packed cell
                    // deliberately cannot represent.
                    projected.underline_style = switch (style.flags.underline) {
                        .none, .single => .single,
                        .double => .double,
                        .curly => .curly,
                        .dotted => .dotted,
                        .dashed => .dashed,
                    };
                    projected.underline_color = palette.resolveUnderlineColor(style);
                    // An invisible cell resolves to "no ink" HERE so the
                    // painter's measure and paint cannot disagree — the
                    // contract `TerminalCell.cp == 0` states. The
                    // decorations above survive it, which matches what the
                    // SGR attribute means: the GLYPH is hidden, the line
                    // through the cell is not.
                    if (style.flags.invisible) cp = 0;
                }

                // A search wash overrides the cell's own colors. It lands
                // HERE, on `TerminalCell.bg`, rather than on
                // `TerminalRow.selection`, because that field carries ONE
                // range per row: a row routinely holds several matches, and
                // may also be selected, and one range cannot say both.
                //
                // The ranges libghostty flattens are INCLUSIVE at both ends,
                // and the FIRST entry covering a cell wins — the current
                // match was pushed first (see `applySearchHighlights`), so a
                // cell that is both "a match" and "the match" paints as the
                // latter.
                for (row.highlights.items) |hl| {
                    if (x < hl.range[0] or x > hl.range[1]) continue;
                    const current = hl.tag == search_current_tag;
                    projected.bg = if (current) palette.search_current else palette.search_match;
                    projected.fg = if (current) palette.search_current_text else palette.search_match_text;
                    break;
                }

                // The hover affordance. An underline rather than a colour
                // because it is the convention every terminal and browser
                // already trained the user on, and because it composes: a
                // hovered cell keeps its own foreground, its own background,
                // and any search wash it happens to be under.
                //
                // Only the FLAG is forced. A cell that already carries an SGR
                // underline keeps its own style and colour — a curly
                // spell-check underline under the pointer should not
                // straighten out because the pointer arrived.
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

                // A box-drawing cell carries no cluster: the painter
                // draws it as geometry. Everything else stages its full
                // grapheme (primary plus combining marks) into the
                // session's text arena.
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
                        // The valid UTF-8 sentinel exceeds the painter ceiling,
                        // forcing this row to degrade whole. An empty cluster
                        // would erase ink without charging any text budget.
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
        // The cursor comes from the render snapshot, not the emulator
        // directly: `rs.cursor.viewport` is null when the cursor sits
        // outside the visible viewport (scrolled into history), which is
        // exactly the painter's "no cursor" case.
        const cursor: ?canvas.TerminalCursor = blk: {
            if (!rs.cursor.visible) break :blk null;
            const vp = rs.cursor.viewport orelse break :blk null;
            const x: u16 = @intCast(vp.x);
            break :blk .{
                // A cursor whose left neighbor is a wide cell is sitting
                // on that cell's SPACER tail — the half with no ink. Draw
                // it on the primary instead, or a block cursor covers the
                // blank half of a CJK character and the glyph itself
                // stays unmarked.
                .x = if (vp.wide_tail and x > 0) x - 1 else x,
                .y = @intCast(vp.y),
                // Ghostty's hollow block is a shape the EMULATOR asked
                // for (it is not a DECSCUSR value; it arrives through the
                // configured default style). It maps to the SDK's own
                // `block_hollow` rather than collapsing onto `.block`,
                // because the painter has two independent reasons to
                // outline a cursor — this one, and the unfocused-window
                // cue it drives itself from `TerminalPaintOptions.focused`
                // — and they have to stay independent. Collapsing made a
                // program's explicit hollow cursor paint solid whenever
                // the window happened to be focused.
                .shape = switch (rs.cursor.visual_style) {
                    .bar => .bar,
                    .underline => .underline,
                    .block => .block,
                    .block_hollow => .block_hollow,
                },
                // DECSCUSR's blink bit, which libghostty resolves into
                // mode 12 (`rs.cursor.blinking`). Carried as STATE: the
                // painter draws the visible phase and the host owns
                // arming an animation, so a producer that reports it and
                // a renderer that ignores it still agree about the
                // cursor.
                .blinking = rs.cursor.blinking,
                // A cursor on a double-width glyph covers BOTH of its
                // columns — either because it sits on the wide cell
                // itself, or because it sits on that cell's spacer tail
                // and `.x` above already walked it back to the primary.
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

    /// Wire the stream handler's effect callbacks. Called at create and
    /// after `reset` rebuilds the stream.
    ///
    /// Four are live: `write_pty` (query answers routed back to the pty),
    /// `title_changed` and `pwd_changed` (the child naming itself and its
    /// directory), and `bell`. All four only ever COPY into session-owned
    /// storage — the callbacks run inside the parser, so none of them may
    /// allocate, block, or reach back into the app.
    ///
    /// The rest stay null (the emulator's read-only defaults). Notably
    /// `clipboard_write`: OSC 52 lets any program running in the terminal
    /// write the user's clipboard, and routing that needs a consent story
    /// the session does not have.
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
        // BEFORE the terminal: the engine untracks pins in the screen's own
        // pool, which `term.deinit` is about to free. This is also what makes
        // closing a terminal (or a whole tab) with its search field open
        // leave nothing behind — there is no separate teardown to forget.
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
        // Output is the overwhelmingly common screen change; marking here
        // (rather than serializing here) is what keeps the semantic text
        // off the hot path entirely for panes nobody reads.
        session.screen_text_dirty = true;
    }

    /// Hard-reset the emulator for a fresh session (a RIS): clears the
    /// screen, scrollback, modes (application-cursor, reverse video),
    /// palette overrides, and — by rebuilding the stream — any partial
    /// escape sequence left mid-parse. Without this, restarting a shell
    /// after the previous one exited mid-sequence or in a non-default
    /// mode would misencode the new shell's keys or misparse its first
    /// output as a continuation of the old stream.
    pub fn reset(session: *Session) void {
        // The engine pins into pages `fullReset` is about to free, and a
        // restarted shell inherits none of the previous one's scrollback —
        // so there is nothing for a surviving search to be a search OF.
        session.discardSearchEngine();
        session.search = .{};
        session.pointer_selection.reset(&session.term);
        session.term.fullReset();
        // `fullReset` (a RIS) leaves the OSC color state alone, so clear
        // it here: a shell that overrode palette entries (OSC 4) or the
        // foreground/background/cursor colors (OSC 10/11/12) and exited
        // must not tint the next session. Overrides drop; the theme
        // defaults stay (paint refreshes them every frame anyway).
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
        // `fullReset` clears the emulator's own title and pwd, so the cached
        // copies must go with them: a restarted shell must not inherit the
        // dead one's tab label or hand a new pane the directory the previous
        // process was in. The bell latch drops for the same reason — an
        // attention cue for a session that no longer exists.
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
            // Grow to fit (doubling), up to the ceiling: replies may be
            // retained here across feeds while the app's outbound ring
            // is full, so accumulation is normal, not exceptional. Past
            // the ceiling — or under allocation failure — the reply
            // drops WHOLE and counted, never cut.
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

    /// OSC 0/2 landed. libghostty-vt has already stored the title on the
    /// terminal and hands us nothing but the notification, so the value
    /// comes back through `getTitle` (the contract stated on
    /// `Effects.title_changed`).
    fn titleChanged(handler: *vt.TerminalStream.Handler) void {
        const session: *Session = @alignCast(@fieldParentPtr("term", handler.terminal));
        const reported = handler.terminal.getTitle() orelse "";
        const kept = truncateUtf8(reported, session.title_buf.len);
        @memcpy(session.title_buf[0..kept.len], kept);
        session.title_len = kept.len;
    }

    /// OSC 7 landed. What `getPwd` returns is the payload VERBATIM —
    /// libghostty-vt's `reportPwd` states it stores the raw payload unparsed
    /// and that "embedders read it via getPwd() and are responsible for
    /// decoding any URI scheme". In practice every shell integration emits a
    /// `file://<host>/<percent-encoded path>` URL, which is useless to a
    /// spawn until it is decoded, so the decode happens HERE, once per
    /// report, rather than at each of the readers.
    fn pwdChanged(handler: *vt.TerminalStream.Handler) void {
        const session: *Session = @alignCast(@fieldParentPtr("term", handler.terminal));
        const reported = handler.terminal.getPwd() orelse "";
        session.pwd_len = decodePwdUrl(reported, &session.pwd_buf);
    }

    /// BEL. Latch it and return: a callback running inside the parser is the
    /// wrong place to make noise, and the app owns whether an attention cue
    /// is even wanted for this pane.
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

    /// Read AND clear the bell latch — the app's cue is edge-triggered, and a
    /// reader that had to remember to clear separately would eventually
    /// leave a tab marked forever.
    pub fn takeBell(session: *Session) bool {
        const rung = session.bell_rung;
        session.bell_rung = false;
        return rung;
    }

    /// Whether the cursor sits at a shell prompt rather than in command
    /// output. This is the emulator's own OSC 133 answer (`cursorIsAtPrompt`)
    /// — a couple of field reads off the active cursor, no walk — and it is
    /// false for every shell without prompt-mark integration, which is the
    /// honest "unknown" for a cue that must never guess.
    pub fn atPrompt(session: *Session) bool {
        return session.term.cursorIsAtPrompt();
    }

    /// Clamp `bytes` to `limit` WITHOUT cutting a UTF-8 scalar in half. A cut
    /// sequence is not a shorter title, it is an invalid one — it paints as
    /// replacement junk and reads as junk to assistive tech.
    fn truncateUtf8(bytes: []const u8, limit: usize) []const u8 {
        if (bytes.len <= limit) return bytes;
        // `bytes[end]` is the first DROPPED byte. While it is a continuation
        // byte (0b10xxxxxx) the cut lands inside a sequence, so walk back to
        // that sequence's lead byte and drop it whole.
        var end = limit;
        while (end > 0 and bytes[end] & 0xC0 == 0x80) end -= 1;
        return bytes[0..end];
    }

    /// Decode an OSC 7 payload into a plain filesystem path, returning the
    /// number of bytes written to `out` (0 = unusable, which the readers
    /// surface as "unknown").
    ///
    /// The shape is `file://<host>/<percent-encoded path>`; `kitty-shell-cwd`
    /// is the other scheme in the wild and carries the same authority/path
    /// split. A payload with no scheme but a leading `/` is taken verbatim
    /// (some integrations emit a bare path).
    ///
    /// The HOST is deliberately ignored rather than validated as local: this
    /// module has no hostname API (ghostty's own `isLocal` check lives in its
    /// app layer, not in libghostty-vt), and the consequence of an ssh
    /// session's remote path is bounded — the spawn's `cd` simply fails and
    /// falls back (see `paneArgvIn`), never runs anything.
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
            // The authority ends at the first '/', which is also the path's
            // own first byte. No slash means no path at all.
            const path_start = std.mem.indexOfScalar(u8, authority, '/') orelse return 0;
            path = authority[path_start..];
        }
        // Anything not absolute names nothing a spawn could cd into: a
        // relative path is relative to a directory we do not know.
        if (path.len == 0 or path[0] != '/') return 0;
        return percentDecode(path, out);
    }

    /// Percent-decode in place into `out`. Returns 0 — the whole value, never
    /// a prefix — on a malformed escape, an embedded NUL, or an overrun: a
    /// half-decoded path is a DIFFERENT directory, and handing one to a
    /// spawn is worse than admitting the pwd is unknown.
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
                // A decoded NUL would cut the path at the C boundary, so the
                // child would receive a path shorter than the one checked.
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

    /// Resize the emulator grid (reflow included). Returns whether the
    /// grid now matches the request: a no-op (already that size) and a
    /// successful reflow both return true; an allocation failure returns
    /// false so the caller leaves its model dimensions unchanged and
    /// retries on the next frame, keeping the emulator and the pty from
    /// disagreeing about the size under memory pressure.
    pub fn resize(session: *Session, new_cols: u16, new_rows: u16) bool {
        const c: vt.size.CellCountInt = @intCast(std.math.clamp(@as(usize, new_cols), 2, max_cols));
        const r: vt.size.CellCountInt = @intCast(std.math.clamp(@as(usize, new_rows), 2, max_rows));
        if (c == session.term.cols and r == session.term.rows) return true;
        session.term.resize(session.gpa, .{ .cols = c, .rows = r }) catch return false;
        // Reflow rewrites the viewport: the cached semantic text describes
        // a grid that no longer exists.
        session.screen_text_dirty = true;
        // Reflow moves every cell, so keyboard-selection coordinates
        // into the OLD grid are meaningless (and a caret past the new
        // edge would strand Shift+Arrow and copy on cells that no
        // longer exist). Re-anchor at the clamped head: selection mode
        // stays armed, the caret lands inside the new grid, and the
        // stale range is dropped rather than copied.
        if (session.select_anchor != null) {
            session.select_head = .{
                .x = @intCast(@min(@as(usize, session.select_head.x), @as(usize, session.term.cols) - 1)),
                .y = @intCast(@min(@as(usize, session.select_head.y), @as(usize, session.term.rows) - 1)),
            };
            session.select_anchor = session.select_head;
            session.applySelection();
        }
        // Reflow can replace every page node the search flattened its results
        // over. `ScreenSearch` re-inits itself on `select`/`feed`, but the
        // PROJECTION reads those results every frame and nothing calls either
        // between a resize and the next paint — so the search is rebuilt here,
        // against the screen that now exists.
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

    /// Put `pin`'s row at the top of the viewport. The other wrappers only
    /// expose `.active`/`.top`/`.delta_row`, and scroll-into-view needs an
    /// ABSOLUTE destination that survives the viewport moving under it.
    /// Routed through `scrollTracked` like every other scroll so the cached
    /// screen text is invalidated with the move.
    pub fn scrollToPin(session: *Session, pin: vt.Pin) void {
        session.scrollTracked(.{ .pin = pin });
    }

    /// Put the viewport back on the absolute row `offset` — the coordinate
    /// `scrollbar().offset` reports, so a saved offset restores exactly.
    pub fn scrollToRow(session: *Session, offset: usize) void {
        session.scrollTracked(.{ .row = offset });
    }

    /// Every scroll goes through here: a scroll that actually MOVED the
    /// viewport changes what the screen shows, so the cached semantic
    /// text is invalidated with it — scrollback browsing must read (to
    /// assistive tech) and fingerprint as the rows it paints, never the
    /// bottom viewport it left. The offset compare keeps the common
    /// no-op (`scrollToBottom` before typing while already pinned) from
    /// invalidating the screen text every keystroke.
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
        // The ROW alone is not enough: a viewport pinned to the live bottom
        // sits at a row that OUTPUT moves, so "put it back" means "back to
        // the bottom" there and "back to that row" everywhere else.
        session.search.restore_bottom = bar.offset + bar.len >= bar.total;
        session.search.restore_row = bar.offset;
        session.search.open = true;
    }

    /// Dismiss the field, drop the engine, and restore the pre-search
    /// viewport. The washes go with it: `applySearchHighlights` clears every
    /// row before it decides whether to apply anything.
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

    /// Append committed text to the needle and re-run the search. False means
    /// the needle did not change — a press at the ceiling is a no-op, never a
    /// silent truncation that would search for a different phrase than the
    /// one on screen.
    pub fn searchInput(session: *Session, text: []const u8) bool {
        if (!session.search.open or text.len == 0) return false;
        // Control bytes are not part of a phrase, and one that landed in the
        // needle would search for something the user cannot see they typed.
        for (text) |byte| if (byte < 0x20 or byte == 0x7f) return false;
        if (session.search.needle_len + text.len > session.search.needle_buf.len) return false;
        @memcpy(session.search.needle_buf[session.search.needle_len..][0..text.len], text);
        session.search.needle_len += text.len;
        session.searchRefresh();
        return true;
    }

    /// Insert CLIPBOARD text into the needle.
    ///
    /// Separate from `searchInput` because typed text and pasted text fail
    /// differently. A typed control byte is a key that had no business in a
    /// phrase, so rejecting the press is right. A PASTE carrying one is
    /// ordinary — copying a word out of a terminal picks up its trailing
    /// newline almost every time — and rejecting the whole paste for it would
    /// leave cmd+V looking as inert as it was before it was implemented.
    ///
    /// So this takes the first line and drops control bytes from it. A needle
    /// cannot span rows anyway: the engine matches within a row, so the bytes
    /// after a newline could never contribute to a match, and silently keeping
    /// them would search for a phrase the field cannot display.
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

    /// Delete the last SCALAR of the needle. Cutting a single byte off a
    /// multi-byte character leaves an invalid needle that matches nothing and
    /// paints as replacement junk.
    pub fn searchBackspace(session: *Session) bool {
        if (!session.search.open or session.search.needle_len == 0) return false;
        var end = session.search.needle_len - 1;
        while (end > 0 and session.search.needle_buf[end] & 0xC0 == 0x80) end -= 1;
        session.search.needle_len = end;
        session.searchRefresh();
        return true;
    }

    /// Step to the next (`forward`) or previous match and bring it on screen.
    /// False means there was nothing to step to — which the chrome says out
    /// loud rather than swallowing.
    pub fn searchStep(session: *Session, forward: bool) bool {
        if (!session.search.open) return false;
        if (session.searchScreenStale()) session.searchRefresh();
        const engine = if (session.search.engine) |*value| value else return false;
        const moved = engine.select(if (forward) .next else .prev) catch return false;
        if (!moved) return false;
        session.revealCurrentMatch();
        return true;
    }

    /// Matches found for the current needle. Zero with a NON-EMPTY needle is
    /// the honest "nothing here"; zero with an empty one only means nothing
    /// has been asked yet.
    pub fn searchMatchCount(session: *const Session) usize {
        const engine = if (session.search.engine) |*value| value else return 0;
        return engine.matchesLen();
    }

    /// The current match's position in READING order (1 = the oldest match
    /// found), or 0 when none is selected. The engine indexes from the
    /// NEWEST match, which is the right order to step in and the wrong one to
    /// show a person.
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
        // Recorded BEFORE the search runs, so a teardown on the failure path
        // below already knows which screen the pins belong to.
        session.search.screen_key = screens.active_key;
        session.search.screen_generation = screens.generation(screens.active_key);
        // `vt.search.Thread` is `void` in a libghostty-vt built as a LIBRARY
        // (`GhosttyZig.zig` pins `artifact = .lib`), so there is no background
        // searcher to hand this to. This used to call `searchAll`, which walks
        // the WHOLE scrollback on the dispatch thread on every keystroke — fine
        // for ordinary history, a visible stutter while typing against a full
        // 50 MB one.
        //
        // So it does one bounded slice here and leaves the rest to
        // `searchPump`, driven a slice per frame. The engine's own `tick` is
        // built for exactly this: it makes incremental progress, asks for a
        // `feed` when it needs more data, and reports `SearchComplete`.
        //
        // The first slice covers the ACTIVE screen before history, so the
        // matches on the text someone is looking at appear on the same frame
        // they typed into — the rest stream in behind.
        session.search.incomplete = true;
        session.search.landed = false;
        _ = session.searchPump(search_first_slice_steps);
    }

    /// Steps of incremental search per slice.
    ///
    /// MEASURED, not guessed: against a 4000-row scrollback the whole search
    /// takes 14 ticks, so one tick covers roughly 285 rows — about a PageList
    /// page. Re-measure before changing either number; the granularity is the
    /// engine's, not ours.
    ///
    /// The first slice runs INLINE on the keystroke and is deliberately small.
    /// One tick covers the active screen, and the step after a `FeedRequired`
    /// is spent on the feed itself, so four is the smallest budget that still
    /// lets an ORDINARY history — the common case, a terminal nowhere near its
    /// scrollback ceiling — finish inline and never wake the frame pump at
    /// all. A deep history stays off the keystroke either way.
    pub const search_first_slice_steps: usize = 4;
    /// ...and the frame slices are sized so a 500k-row history finishes in
    /// well under a second of frames (~9k rows a frame) without any single
    /// frame paying for the whole walk.
    pub const search_frame_slice_steps: usize = 32;

    /// Make bounded progress on the incremental search. True means work
    /// remains and the caller should pump again next frame.
    ///
    /// Every failure DISCARDS the engine rather than leaving a half-walked one
    /// live: a partial result set that has stopped growing would read as "these
    /// are all the matches", which is a worse answer than no search.
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
        // Land on the FIRST match to appear, once, rather than on every slice:
        // re-selecting as later matches stream in would drag the viewport
        // around while someone is already reading the hit they were given.
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

    /// Push the live matches into the render state's per-row highlight lists.
    ///
    /// Clear-then-apply, in ghostty's own order (`renderer/generic.zig`
    /// ~1306): `updateHighlightsFlattened` APPENDS and never clears, and the
    /// current match goes in FIRST because the cell loop takes the first
    /// range covering a cell.
    fn applySearchHighlights(session: *Session) void {
        const row_data = session.render.row_data.slice();
        for (row_data.items(.highlights), row_data.items(.dirty)) |*hls, *dirty| {
            if (hls.items.len == 0) continue;
            hls.clearRetainingCapacity();
            // The renderer's own "this row changed" flag. Nothing on this
            // path reads it (the projection is rebuilt whole every frame),
            // but leaving a row whose washes just vanished marked CLEAN would
            // be a lie to any consumer that does.
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
        // The SLICE is caller-owned; the highlights inside it stay owned by
        // the search, so only the slice is freed here.
        const all = engine.matches(session.gpa) catch return;
        defer session.gpa.free(all);
        session.render.updateHighlightsFlattened(session.gpa, search_match_tag, all) catch {};
    }

    /// Bring the current match on screen, and ONLY when it is not already
    /// there: stepping between two matches inside one viewport must not jump
    /// the text out from under the reader.
    fn revealCurrentMatch(session: *Session) void {
        const engine = if (session.search.engine) |*value| value else return;
        const match = engine.selectedMatch() orelse return;
        const screen = session.term.screens.active;
        const start = match.startPin();
        if (screen.pages.pointFromPin(.viewport, start) != null) return;
        session.scrollToPin(start);
    }

    /// Tear the engine down with the teardown the SCREEN's state allows.
    /// `deinit` untracks pins in that screen's PageList pool; once the screen
    /// itself is gone — an application leaving the alternate screen frees it
    /// — the pool went with it and only `deinitScreenInvalid` is safe.
    fn discardSearchEngine(session: *Session) void {
        // Cleared even when there is no engine: an engine that never got built
        // must not leave the session claiming it owes the frame pump work.
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

    /// Begin a keyboard selection at the terminal cursor (or extend the
    /// existing one). `block` selects a rectangle; otherwise the
    /// selection flows line-wise like every text surface.
    pub fn beginSelection(session: *Session, block: bool) void {
        // Anchor from LIVE terminal state, never the last painted
        // snapshot: output that moved the cursor since the previous
        // paint (or a session that has not painted yet) must anchor at
        // the cell the cursor actually occupies. A cursor scrolled out
        // of the viewport anchors at the origin, the render snapshot's
        // own fallback.
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

    /// Select the ENTIRE scrollback, not merely the visible screen — what
    /// Ghostty's `select_all` does, and what someone arriving from it expects
    /// cmd+A to mean. The engine's own `Screen.selectAll` walks from
    /// `.screen = .{}` (the top of history, not the top of the viewport) and
    /// omits surrounding whitespace, so a mostly-empty screen does not hand
    /// back a copy padded with blank rows.
    ///
    /// Deliberately does NOT arm keyboard-selection mode. That mode carries a
    /// VIEWPORT anchor and head, and the reason scrollback chords pause while
    /// it is armed is that scrolling would leave the painted caret naming
    /// different text than a copy returns. This selection is expressed purely
    /// in absolute pins with no caret at all, so scrolling under it stays
    /// truthful and the chords can stay live.
    ///
    /// False means there was nothing to select — an empty screen — and the
    /// previous selection is left alone rather than being silently dropped.
    pub fn selectAllHistory(session: *Session) bool {
        const screen = session.term.screens.active;
        const selection = screen.selectAll() orelse return false;
        session.pointer_selection.reset(&session.term);
        screen.select(selection) catch return false;
        session.select_anchor = null;
        return true;
    }

    /// The cell box the painter last MEASURED for this session, or null if
    /// nothing has painted it yet.
    ///
    /// Null is a real answer, not a missing one: before the first paint the
    /// app genuinely does not know how wide a cell is, because the width is
    /// the mono face's advance as resolved by the platform's text-measure
    /// provider and only the painter is handed that provider. Callers must
    /// decide what "not measured yet" means for them — the viewport proposer
    /// declares itself incomplete and proposes nothing, the pointer paths
    /// decline the gesture. None of them may substitute a guess.
    pub fn measuredCell(session: *const Session) ?CellBox {
        return session.measured_cell;
    }

    /// Record a measurement. The PAINTER is the only legitimate caller: it is
    /// the only place holding tokens with the runtime's text-measure provider
    /// stamped on, which is what makes `width` an advance rather than an
    /// estimate.
    ///
    /// A non-finite or non-positive box is REFUSED rather than stored, which
    /// is what lets every reader treat a non-null `measured_cell` as safe to
    /// divide by. Refusing leaves the previous value in place: a single bad
    /// frame should not strand a session that was already measured.
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
        // Nothing has measured this pane yet, so there is no mapping from a
        // point to a cell. Declining is the only honest answer.
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
                // Ghostty's geometry wants whole device columns, so this one
                // rounds where the painter deliberately does not (see the
                // SDK's `cellMetrics`). `@max(1, ...)` keeps the integer at
                // least one cell wide for a sub-point cell at a tiny font;
                // it is NOT a guard against zero, which `measuredCell` has
                // already ruled out.
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

    /// Point (in widget-relative canvas points) to viewport cell.
    ///
    /// THE divide by the cell box, and the reason `measured_cell` is an
    /// optional: this is reached from pointer selection, autoscroll, and URL
    /// hover, and each of those used to carry its own `cell_width <= 0` guard
    /// to protect this division. Three copies of a check that could never fire
    /// is three chances for a fourth caller to arrive without one. The check
    /// now lives HERE, once, at the division it actually protects, and the
    /// optional makes it unskippable.
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

    /// Re-derive the viewport-relative selection coordinates from the
    /// emulator's ABSOLUTE pins after content moved (an output feed
    /// scrolled the live screen). The pins track the selected TEXT —
    /// the painted highlight already follows it — so the caret and the
    /// next Shift+Arrow must follow the same cells, or a copy would
    /// return text the caret no longer names. Returns whether a
    /// selection is still armed: a range that scrolled out of the
    /// viewport (or that the emulator dropped) clears to the honest
    /// no-selection instead of desynchronizing.
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
        // Any failure below CLEARS the emulator selection rather than
        // leaving the previous range live: the model caret has already
        // moved, so a copy against the stale range would return text
        // the painted outline no longer describes. No-selection is the
        // honest degraded state — the caret keeps painting from
        // `select_head`, and the next successful move re-establishes
        // the highlight.
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

    /// The selected text, caller-owned (freed with the sentinel).
    /// Null means exactly "nothing is selected" — a serialization
    /// failure over an ACTIVE selection is an error, never a silent
    /// null: the caller owes the user a failure signal when a copy
    /// cannot be produced.
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

    /// INVALIDATE the cached viewport text — the grid's ACCESSIBILITY
    /// surface (a terminal's semantic content IS its text) and, through
    /// the a11y tree, the session-fingerprint coverage of real cell
    /// state: two screens with identical byte counters but different
    /// cells must never fingerprint alike.
    ///
    /// This used to SERIALIZE the whole viewport, and it is called from
    /// every path that changes the screen — every pty output batch
    /// included, for every session, painted or not. That put an
    /// O(viewport) walk plus a fresh heap allocation on the hot path for
    /// terminals nobody was looking at. It now only sets a flag;
    /// `screenText` pays, once, and only when something actually reads.
    pub fn refreshScreenText(session: *Session) void {
        session.screen_text_dirty = true;
    }

    /// The URL under a pointer at view-relative `x`/`y`, or null.
    ///
    /// The convenience form of `linkAtPoint` for callers that only need
    /// somewhere to go; the same lifetime rule applies (see `link_buf`).
    pub fn urlAtPoint(session: *Session, x: f32, y: f32) ?[]const u8 {
        const link = session.linkAtPoint(x, y) orelse return null;
        return link.url;
    }

    /// The link under a pointer at view-relative `x`/`y`, from either channel.
    ///
    /// Two channels, and they are not equal. An OSC 8 hyperlink is EXPLICIT —
    /// the program told the emulator "these cells are this target" — so where
    /// one exists it beats a shape this app guessed from the text. The
    /// heuristic (`url.zig`) is the fallback for the overwhelming majority of
    /// output, which carries no markup at all.
    ///
    /// Two things stop the explicit channel from becoming a way for program
    /// output to pick what the OS opens:
    ///
    ///  1. The href passes `url.isAllowedTarget` — the same allowlist the
    ///     heuristic can only ever produce by construction. A refused href is
    ///     not repaired and does not disable linking on that cell; the row
    ///     falls back to the heuristic, so a `javascript:` href wrapped around
    ///     visible link text still opens the VISIBLE link and nothing else.
    ///
    ///  2. Hover exposes the REAL
    ///     href in a pane-local preview and in the terminal's accessibility
    ///     label. That closes OSC 8's phishing gap before its indirection is
    ///     honored: `https://bank.example` may display over an href of
    ///     `https://evil.example`, but the target is visible before the click
    ///     and is the target the click opens. For URL-shaped display text that
    ///     disagrees, the exact cell+href must have completed preview emission
    ///     before the href wins. A quick click, keyboard/accessibility action,
    ///     failed paint, or stale preview falls back to the visible URL.
    ///
    /// The emulator is read LIVE here rather than through the render state:
    /// this runs on pointer input, arbitrarily long after the last paint, and
    /// the render state's row pins are only valid until the terminal next
    /// changes.
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

    /// The OSC 8 target on a viewport cell, as a slice into PAGE memory.
    ///
    /// Valid only until the terminal state next changes, which is why every
    /// caller copies. Reads the LIVE screen, so it is safe to call at any
    /// time — unlike `cellHyperlinkUri`, which goes through a render-state
    /// pin and is only safe inside a paint.
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

    /// The OSC 8 target on the cell at column `x` of the row `pin` names, as a
    /// slice into PAGE memory.
    ///
    /// Takes a render-state row pin, so it is only safe to call while that
    /// state is current — inside `snapshot`, between the render update and the
    /// next terminal change. This is the same contract libghostty's own
    /// `RenderState.linkCells` states, and for the same reason: the pin's node
    /// must not be dereferenced once the terminal has moved on.
    fn cellHyperlinkUri(pin: vt.Pin, x: usize) ?[]const u8 {
        const page = pin.node.page();
        if (x >= page.size.cols) return null;
        const rac = page.getRowAndCell(@intCast(x), pin.y);
        if (!rac.cell.hyperlink) return null;
        const id = page.lookupHyperlink(rac.cell) orelse return null;
        return page.hyperlink_set.get(page.memory, id).uri.slice(page.memory);
    }

    /// Copy a resolved target into session storage. Null when it does not fit,
    /// which is also a refusal to return a link at all — a truncated URL is a
    /// DIFFERENT destination, never a shorter version of the same one.
    fn rememberLink(session: *Session, value: []const u8) ?[]const u8 {
        if (value.len == 0 or value.len > session.link_buf.len) return null;
        @memcpy(session.link_buf[0..value.len], value);
        session.link_len = value.len;
        return session.link_buf[0..session.link_len];
    }

    /// Arm the hover underline at a widget-relative point, or disarm it.
    ///
    /// The caller owns the POLICY (Ghostty's is: pointer over a pane with the
    /// link chord held); the session owns only where the pointer is. Returns
    /// whether the armed state changed, so a caller can decide whether the
    /// frame is worth repainting.
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

    /// The real OSC 8 destination under the pointer.
    ///
    /// This deliberately does not return heuristic links: their target is
    /// already the text on glass. The returned explicit target has passed the
    /// same allowlist as a click and is copied into session storage, so both
    /// the visual preview and accessibility surface announce exactly what the
    /// click path can hand to the OS.
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

    /// The link the hover point resolves to right now, or null.
    ///
    /// Called from `snapshot` immediately after the render update, so the
    /// answer is about the state being painted.
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

    /// Serialize the viewport into the reused, grown-to-fit text buffer.
    /// `Writer.Allocating` keeps the previous frame's allocation and only
    /// grows when a screen needs more than it already holds, so a steady
    /// terminal serializes without touching the allocator at all.
    fn renderScreenText(session: *Session) void {
        const screen = session.term.screens.active;
        var writer: std.Io.Writer.Allocating = .initOwnedSlice(session.gpa, session.screen_text_buf);
        // The buffer is reclaimed on EVERY exit path — the allocating
        // writer owns it while it runs and would free it on `deinit`.
        defer session.screen_text_buf = writer.writer.buffer;
        session.screen_text_dirty = false;
        session.screen_text_len = 0;
        const br = screen.pages.getBottomRight(.viewport) orelse return;
        // Unknown beats stale: a screen we could not render must not keep
        // reading (to assistive tech) or fingerprinting as the previous
        // one — the emulator and the painted grid have already advanced.
        // Empty is the loud degraded state (the view falls back to its
        // static label, and any checkpoint over it diverges rather than
        // false-verifying), and it persists until the next invalidation
        // rather than retrying on every read.
        screen.dumpString(&writer.writer, .{
            .tl = screen.pages.getTopLeft(.viewport),
            .br = br,
            .unwrap = false,
        }) catch return;
        session.screen_text_len = writer.writer.end;
    }
};
