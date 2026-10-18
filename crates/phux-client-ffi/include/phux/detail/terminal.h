#ifndef PHUX_CLIENT_DETAIL_TERMINAL_H
#define PHUX_CLIENT_DETAIL_TERMINAL_H

/* Internal section of phux/client.h; include the umbrella header instead. */

size_t phux_client_outgoing_count(const PhuxClient *client);
PhuxClientResult phux_client_outgoing_get(const PhuxClient *client, size_t index, PhuxBytes *out_frame);
PhuxClientResult phux_client_outgoing_clear(PhuxClient *client);
size_t phux_client_effect_count(const PhuxClient *client);
PhuxClientResult phux_client_effect_get(const PhuxClient *client, size_t index, PhuxClientEffect *out_effect);
/* Accepts staged effects, including committed agent publications. Ending a borrow
 * is not acceptance; transport retirement may drop views without accepting them. */
PhuxClientResult phux_client_effect_clear(PhuxClient *client);
PhuxClientResult phux_client_terminal_grid(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxTerminalGridView *out_view);

/* Read-only companion to terminal_grid. Call immediately after that query,
 * before ANY mutable client call; both borrows remain valid. Returns
 * INVALID_STATE when there is no current grid. Initialize size/version first.
 * Defaults/palette entries come from the same libghostty render pass. Missing
 * default colors use the renderer's configured fallback (also swapped under
 * reverse_colors); missing cursor color uses its configured cursor fallback. */
PhuxClientResult phux_client_terminal_grid_metadata(const PhuxClient *client, const PhuxResourceId *terminal_id, PhuxTerminalGridMetadata *out_metadata);
/* Install (non-NULL) or clear (NULL) the client-side terminal theme
 * (ADR-0157). Additive to ABI version 2. The colours become every replica's
 * defaults now and on later connections; an application's OSC 4/10/11/12
 * still override them and OSC 104/110/111/112 revert to the theme. Cleared,
 * grid metadata reports has_foreground/has_background false again. Visible
 * terminals republish. A wrong size or version is PHUX_CLIENT_INVALID_ARGUMENT. */
PhuxClientResult phux_client_set_terminal_theme(PhuxClient *client, const PhuxTerminalTheme *theme);

/* Draw list (additive to ABI version 3): phux's paint rules applied to the
 * current grid, as rectangles and text in cell coordinates. Paint background,
 * backgrounds, a solid cursor, texts, blocks, decorations, then any other
 * cursor; a faint item paints at half alpha. Colours are final: the client
 * theme, palette provenance, inverse video and selection are applied. */
typedef struct PhuxDrawOptions {
    size_t size; /* initialize to sizeof(PhuxDrawOptions) */
    uint32_t version; /* initialize to PHUX_CLIENT_ABI_VERSION */
    PhuxGridRgb selection_foreground, selection_background;
    bool focused; /* a block cursor is solid; unfocused cursors are hollow */
    bool cursor_visible;
    bool blink_visible; /* the blink phase is on */
} PhuxDrawOptions;

typedef struct PhuxDrawBackground {
    uint16_t row, start, end; /* cells start..end; default background omitted */
    PhuxGridRgb color;
} PhuxDrawBackground;

#define PHUX_DRAW_FACE_BOLD 1u
#define PHUX_DRAW_FACE_ITALIC 2u

/* utf8[utf8_offset..+utf8_len] drawn from col across cells columns, one
 * character per column; a wide character or multi-character grapheme is a
 * run of its own. */
typedef struct PhuxDrawText {
    uint16_t row, col, cells;
    uint8_t face; /* PHUX_DRAW_FACE_* bits */
    bool faint;
    PhuxGridRgb color;
    uint32_t utf8_offset, utf8_len;
} PhuxDrawText;

/* One filled part of a cell, in eighths from its top-left corner. */
typedef struct PhuxDrawRegion {
    uint8_t x, y, width, height;
} PhuxDrawRegion;

/* A block element (U+2580..U+259F): fill regions[0..region_count]. */
typedef struct PhuxDrawBlock {
    uint16_t row, col;
    PhuxGridRgb color;
    bool faint;
    uint8_t region_count;
    PhuxDrawRegion regions[2];
} PhuxDrawBlock;

typedef enum PhuxDrawDecorationKind {
    PHUX_DRAW_UNDERLINE_SINGLE = 1,
    PHUX_DRAW_UNDERLINE_DOUBLE = 2,
    PHUX_DRAW_UNDERLINE_CURLY = 3,
    PHUX_DRAW_UNDERLINE_DOTTED = 4,
    PHUX_DRAW_UNDERLINE_DASHED = 5,
    PHUX_DRAW_STRIKETHROUGH = 6,
    PHUX_DRAW_OVERLINE = 7
} PhuxDrawDecorationKind;

typedef struct PhuxDrawDecoration {
    uint16_t row, start, end; /* merged across adjacent cells */
    uint8_t kind; /* PhuxDrawDecorationKind */
    bool faint;
    PhuxGridRgb color;
} PhuxDrawDecoration;

typedef struct PhuxDrawCursor {
    bool visible;
    bool solid; /* paints under the glyphs, which take the cell background */
    uint16_t row, col, width; /* width 2 on a wide character */
    uint32_t style; /* PhuxCursorStyle */
    PhuxGridRgb color;
} PhuxDrawCursor;

typedef struct PhuxDrawList {
    size_t size; /* initialize to sizeof(PhuxDrawList) */
    uint32_t version; /* initialize to PHUX_CLIENT_ABI_VERSION */
    uint16_t cols, rows;
    PhuxGridRgb background;
    const PhuxDrawBackground *backgrounds;
    size_t background_count;
    const PhuxDrawText *texts;
    size_t text_count;
    const PhuxDrawBlock *blocks;
    size_t block_count;
    const PhuxDrawDecoration *decorations;
    size_t decoration_count;
    PhuxDrawCursor cursor;
    PhuxBytes utf8;
} PhuxDrawList;

/* Build the terminal's current grid and lend its draw list. NULL options are
 * the defaults (focused, cursor and blink visible). Borrowed until the next
 * mutable client call, like terminal_grid's view, which this also refreshes.
 * INVALID_STATE without a published grid; a wrong size or version is
 * PHUX_CLIENT_INVALID_ARGUMENT. */
PhuxClientResult phux_client_terminal_draw_list(PhuxClient *client, const PhuxResourceId *terminal_id, const PhuxDrawOptions *options, PhuxDrawList *out_list);
/**
 * Reports whether the published Ghostty terminal's effective mouse-tracking
 * state is active (X10, normal, button, or any-event). Returns
 * PHUX_CLIENT_INVALID_STATE before publication or after detach, and
 * PHUX_CLIENT_INVALID_ARGUMENT for null or
 * malformed arguments. This read-only query preserves borrowed bridge views.
 */
PhuxClientResult phux_client_terminal_mouse_tracking(const PhuxClient *client, const PhuxResourceId *terminal_id, bool *out_enabled);
/**
 * Reports whether the published Ghostty terminal has DEC 2004 bracketed paste
 * on: the mode the server encodes a paste against, as this replica last saw
 * it. Same errors and borrow rules as terminal_mouse_tracking.
 */
PhuxClientResult phux_client_terminal_bracketed_paste(const PhuxClient *client, const PhuxResourceId *terminal_id, bool *out_enabled);
PhuxClientResult phux_client_send_key(PhuxClient *client, const PhuxResourceId *terminal_id, const PhuxKeyEvent *event);
PhuxClientResult phux_client_send_mouse(PhuxClient *client, const PhuxResourceId *terminal_id, const PhuxMouseEvent *event);
PhuxClientResult phux_client_send_focus(PhuxClient *client, const PhuxResourceId *terminal_id, bool focused);
PhuxClientResult phux_client_send_paste(PhuxClient *client, const PhuxResourceId *terminal_id, const uint8_t *data, size_t len, bool trusted);
/* Queue exact cells only for a live, ready, non-viewer subscription.
 * InvalidState means not ready/allowed; Ok is not an application receipt. */
PhuxClientResult phux_client_terminal_resize(PhuxClient *client, const PhuxResourceId *terminal_id, uint16_t cols, uint16_t rows);
/* Same queue, and when has_cell_px is true the frame also carries
 * cell_width_px/cell_height_px: the integer cell the server divides
 * INPUT_MOUSE pixels by. A true discriminator with a zero axis, or a false
 * discriminator with a nonzero axis, is INVALID_ARGUMENT. Additive; it does
 * not bump PHUX_CLIENT_ABI_VERSION. */
PhuxClientResult phux_client_terminal_resize_with_cell_px(PhuxClient *client, const PhuxResourceId *terminal_id, uint16_t cols, uint16_t rows, bool has_cell_px, uint16_t cell_width, uint16_t cell_height);
/* Retain desired cells/pixels through attach and reconnect. Same-size calls
 * reassert intent. The authoritative grid, not Ok, confirms applied geometry. */
PhuxClientResult phux_client_viewport_resize(PhuxClient *client, uint16_t cols, uint16_t rows, bool has_pixel_size, uint16_t pixel_width, uint16_t pixel_height);
/* kind: 0 top, 1 live tail, 2 `value` rows (negative toward history),
 * 3 absolute row `value`, 4 the `value`th shell prompt (OSC 133) above
 * (negative) or below the viewport's top row. */
PhuxClientResult phux_client_scroll_viewport(PhuxClient *client, const PhuxResourceId *terminal_id, uint32_t kind, int64_t value);
PhuxClientResult phux_client_anchor_create(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxDocumentPoint point, PhuxDocumentAnchor *out_anchor);
PhuxClientResult phux_client_anchor_release(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxDocumentAnchor anchor);
/** Client-only Clear: home the cursor, erase the active display and scrollback,
 * clear selection/document anchors, and cancel older history for this replica.
 * Preserves modes, dimensions, durable work and live sequence; sends no input.
 * Rejects an absent/disconnected terminal or mismatched stream/bootstrap IDs. */
PhuxClientResult phux_client_clear_presentation(PhuxClient *client, const PhuxResourceId *terminal_id, uint64_t stream_id, uint64_t bootstrap_id);
PhuxClientResult phux_client_history_viewport_pin(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxDocumentAnchor anchor);
PhuxClientResult phux_client_history_follow_live(PhuxClient *client, const PhuxResourceId *terminal_id);
PhuxClientResult phux_client_selection_set(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxDocumentAnchor start, PhuxDocumentAnchor end, bool rectangle);

/* Native Ghostty gestures, version 1. phase: press=0, drag=1, release=2.
 * Positions and geometry share surface units; press clicks=1..3. A release
 * ignores the cell. A stale handle is rejected. selection_clear invalidates
 * gestures. Nonzero result anchors belong to the caller (anchor_release). */
typedef struct PhuxSelectionGestureEvent {
    size_t size;
    uint32_t version, phase, clicks;
    uint64_t handle;
    uint16_t column;
    bool rectangle;
    uint8_t reserved;
    uint32_t row;
    double x, y;
    uint32_t columns, cell_width, screen_height, padding_left;
} PhuxSelectionGestureEvent;
typedef struct PhuxSelectionGestureResult {
    uint64_t handle;
    PhuxDocumentAnchor start, end;
} PhuxSelectionGestureResult;
PhuxClientResult phux_client_selection_gesture(PhuxClient *client, const PhuxResourceId *terminal_id, const PhuxSelectionGestureEvent *event, PhuxSelectionGestureResult *out_result);

/* Read-only effective Ghostty mode: off=0, X10=1, normal=2, button=3, any=4.
 * Uses the resolved encoder state, including DECSET/DECRST ordering. */
PhuxClientResult phux_client_terminal_mouse_mode(const PhuxClient *client, const PhuxResourceId *terminal_id, uint32_t *out_mode);
PhuxClientResult phux_client_selection_clear(PhuxClient *client, const PhuxResourceId *terminal_id);
PhuxClientResult phux_client_selection_text(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxBytes *out_text);
/**
 * Snapshots the kernel's always-on performance telemetry as a JSON
 * PerfReport (ADR-0096): frames applied and their bytes, engine apply time,
 * and the echo round trip from a key or paste leaving phux_client_send_* to
 * the first output frame for that terminal, plus the runtime's owner-thread
 * apply round trips and grid publications (runtime.*). The returned bytes
 * are borrowed from the client and stay valid until the next
 * phux_client_perf_json call.
 * Returns PHUX_CLIENT_INVALID_ARGUMENT for a null argument.
 */
PhuxClientResult phux_client_perf_json(PhuxClient *client, PhuxBytes *out_json);
/**
 * Every returned anchor handle is transferred to the caller and remains valid
 * until explicitly released, its terminal generation is replaced, or its
 * presentation is cleared. Before
 * the next mutable client call invalidates this borrowed array, callers must
 * either copy the handles for later individual release or call
 * phux_client_search_results_release to release the entire set atomically.
 */
PhuxClientResult phux_client_search(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxBytes query_utf8, bool case_sensitive, const PhuxSearchResult **out_results, size_t *out_count);
PhuxClientResult phux_client_search_results_release(PhuxClient *client);


#endif
