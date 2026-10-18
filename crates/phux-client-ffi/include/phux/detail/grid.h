#ifndef PHUX_CLIENT_DETAIL_GRID_H
#define PHUX_CLIENT_DETAIL_GRID_H

/* Internal section of phux/client.h; include the umbrella header instead. */

typedef enum PhuxDocumentSpace {
    PHUX_DOCUMENT_HISTORY = 0,
    PHUX_DOCUMENT_VIEWPORT = 1,
    PHUX_DOCUMENT_ACTIVE = 2
} PhuxDocumentSpace;

/** Opaque, generation-bound engine document identity. Never inspect or persist. */
typedef struct PhuxDocumentAnchor {
    uint64_t opaque_id;
} PhuxDocumentAnchor;

typedef struct PhuxDocumentPoint {
    uint32_t space;
    uint32_t row;
    uint16_t column;
    uint16_t reserved;
} PhuxDocumentPoint;

typedef struct PhuxTerminalCell {
    uint32_t utf8_offset;
    uint16_t utf8_len;
    uint16_t content_tag;
    uint32_t hyperlink_offset;
    uint32_t hyperlink_len;
    uint8_t wide;
    uint8_t semantic_content;
    uint32_t flags;
    uint8_t foreground_r;
    uint8_t foreground_g;
    uint8_t foreground_b;
    uint8_t background_r;
    uint8_t background_g;
    uint8_t background_b;
    uint8_t underline;
    uint8_t underline_r;
    uint8_t underline_g;
    uint8_t underline_b;
    uint8_t reserved;
} PhuxTerminalCell;

/**
 * Borrowed dense viewport. cells are row-major. Cell UTF-8 and hyperlink
 * slices address the separate utf8 arena. history_loading means a READY cursor
 * or next page is outstanding. top_anchor is opaque, engine-tracked document
 * identity; release it when the frontend no longer needs it.
 */
typedef struct PhuxTerminalGridView {
    PhuxResourceId terminal_id;
    uint64_t stream_id;
    uint64_t bootstrap_id;
    uint64_t last_seq;
    uint64_t document_revision;
    uint16_t cols;
    uint16_t rows;
    const PhuxTerminalCell *cells;
    size_t cell_count;
    PhuxBytes utf8;
    bool cursor_visible;
    uint16_t cursor_col;
    uint16_t cursor_row;
    uint32_t cursor_style;
    uint64_t history_total_rows;
    uint64_t history_viewport_offset;
    uint64_t history_visible_rows;
    uint64_t history_pages_loaded;
    uint64_t history_unread_rows;
    uint64_t history_bytes_loaded;
    bool history_loading;
    bool history_has_more;
    PhuxDocumentAnchor top_anchor;
} PhuxTerminalGridView;

/* Additive metadata: the v1 PhuxTerminalGridView/PhuxTerminalCell layouts stay
 * unchanged. Color provenance permits renderer policy (such as ANSI-8
 * bold-as-bright) without guessing the palette index from resolved RGB. */
typedef enum PhuxGridColorKind {
    PHUX_GRID_COLOR_DEFAULT = 0,
    PHUX_GRID_COLOR_PALETTE = 1,
    PHUX_GRID_COLOR_RGB = 2
} PhuxGridColorKind;

typedef struct PhuxGridRgb {
    uint8_t r, g, b;
} PhuxGridRgb;

/* Client-side terminal colours (ADR-0157): palette 0..15 plus the default
 * foreground, background, and cursor. Initialize size = sizeof(struct),
 * version = PHUX_CLIENT_ABI_VERSION. */
typedef struct PhuxTerminalTheme {
    size_t size;
    uint32_t version;
    PhuxGridRgb ansi16[16];
    PhuxGridRgb foreground, background, cursor;
} PhuxTerminalTheme;

typedef struct PhuxGridCellMetadata {
    uint8_t foreground_kind;
    uint8_t foreground_palette_index; /* meaningful only for PALETTE */
    bool underline_color_is_default;
    bool background_color_is_default;
} PhuxGridCellMetadata;

typedef struct PhuxTerminalGridMetadata {
    size_t size; /* initialize to sizeof(PhuxTerminalGridMetadata) */
    uint32_t version; /* initialize to PHUX_CLIENT_ABI_VERSION */
    uint64_t stream_id, bootstrap_id, last_seq, document_revision;
    uint16_t cols, rows;
    PhuxGridRgb foreground, background, cursor_color;
    bool has_foreground, has_background; /* false: configured renderer fallback */
    bool reverse_colors; /* DECSCNM; effective colors above already swapped */
    bool has_cursor_color;
    bool cursor_blinking, cursor_wide, cursor_at_wide_tail;
    PhuxGridRgb palette[256];
    const PhuxGridCellMetadata *cells; /* row-major, same grid/cell count */
    size_t cell_count;
} PhuxTerminalGridMetadata;

typedef struct PhuxKeyEvent {
    size_t size;
    uint32_t version;
    uint32_t action;
    uint32_t key;
    uint16_t modifiers;
    uint16_t consumed_modifiers;
    bool composing;
    bool has_text;
    PhuxBytes text;
    bool has_unshifted_codepoint;
    uint32_t unshifted_codepoint;
} PhuxKeyEvent;

typedef struct PhuxMouseEvent {
    size_t size;
    uint32_t version;
    uint32_t action;
    uint32_t button;
    uint16_t modifiers;
    double x;
    double y;
} PhuxMouseEvent;

typedef enum PhuxViewportScrollKind {
    PHUX_VIEWPORT_SCROLL_TOP = 0,
    PHUX_VIEWPORT_SCROLL_BOTTOM = 1,
    PHUX_VIEWPORT_SCROLL_DELTA = 2,
    PHUX_VIEWPORT_SCROLL_ROW = 3
} PhuxViewportScrollKind;

typedef struct PhuxSearchResult {
    PhuxDocumentAnchor start;
    PhuxDocumentAnchor end;
} PhuxSearchResult;


#endif
