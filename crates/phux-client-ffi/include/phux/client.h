#ifndef PHUX_CLIENT_H
#define PHUX_CLIENT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PHUX_CLIENT_ABI_VERSION 1u
#define PHUX_CLIENT_MAX_OUTBOUND_BYTES (64u * 1024u)
#define PHUX_CLIENT_RELEASE_CARGO_PROFILE "ffi-release"
#define PHUX_CLIENT_CELL_BOLD (1u << 0)
#define PHUX_CLIENT_CELL_ITALIC (1u << 1)
#define PHUX_CLIENT_CELL_FAINT (1u << 2)
#define PHUX_CLIENT_CELL_BLINK (1u << 3)
#define PHUX_CLIENT_CELL_INVERSE (1u << 4)
#define PHUX_CLIENT_CELL_INVISIBLE (1u << 5)
#define PHUX_CLIENT_CELL_STRIKETHROUGH (1u << 6)
#define PHUX_CLIENT_CELL_OVERLINE (1u << 7)
#define PHUX_CLIENT_CELL_SELECTED (1u << 8)
#define PHUX_CLIENT_CELL_PROTECTED (1u << 9)
#define PHUX_CLIENT_CELL_HYPERLINK (1u << 10)

typedef enum PhuxKeyAction {
    PHUX_KEY_RELEASE = 0,
    PHUX_KEY_PRESS = 1,
    PHUX_KEY_REPEAT = 2
} PhuxKeyAction;

typedef enum PhuxKeyModifier {
    PHUX_MOD_SHIFT = 1u << 0,
    PHUX_MOD_CONTROL = 1u << 1,
    PHUX_MOD_ALT = 1u << 2,
    PHUX_MOD_SUPER = 1u << 3,
    PHUX_MOD_CAPS_LOCK = 1u << 4,
    PHUX_MOD_NUM_LOCK = 1u << 5,
    PHUX_MOD_SHIFT_RIGHT = 1u << 6,
    PHUX_MOD_CONTROL_RIGHT = 1u << 7,
    PHUX_MOD_ALT_RIGHT = 1u << 8,
    PHUX_MOD_SUPER_RIGHT = 1u << 9
} PhuxKeyModifier;

typedef enum PhuxMouseAction {
    PHUX_MOUSE_PRESS = 0,
    PHUX_MOUSE_RELEASE = 1,
    PHUX_MOUSE_MOTION = 2
} PhuxMouseAction;

typedef enum PhuxMouseButton {
    PHUX_MOUSE_BUTTON_UNKNOWN = 0,
    PHUX_MOUSE_BUTTON_LEFT = 1,
    PHUX_MOUSE_BUTTON_RIGHT = 2,
    PHUX_MOUSE_BUTTON_MIDDLE = 3,
    PHUX_MOUSE_BUTTON_FOUR = 4,
    PHUX_MOUSE_BUTTON_FIVE = 5,
    PHUX_MOUSE_BUTTON_SIX = 6,
    PHUX_MOUSE_BUTTON_SEVEN = 7,
    PHUX_MOUSE_BUTTON_EIGHT = 8,
    PHUX_MOUSE_BUTTON_NINE = 9,
    PHUX_MOUSE_BUTTON_TEN = 10,
    PHUX_MOUSE_BUTTON_ELEVEN = 11
} PhuxMouseButton;

typedef enum PhuxCellContentTag {
    PHUX_CELL_CODEPOINT = 0,
    PHUX_CELL_CODEPOINT_GRAPHEME = 1,
    PHUX_CELL_BACKGROUND_PALETTE = 2,
    PHUX_CELL_BACKGROUND_RGB = 3
} PhuxCellContentTag;

typedef enum PhuxCellWide {
    PHUX_CELL_NARROW = 0,
    PHUX_CELL_WIDE = 1,
    PHUX_CELL_SPACER_TAIL = 2,
    PHUX_CELL_SPACER_HEAD = 3
} PhuxCellWide;

typedef enum PhuxCellSemanticContent {
    PHUX_CELL_SEMANTIC_OUTPUT = 0,
    PHUX_CELL_SEMANTIC_INPUT = 1,
    PHUX_CELL_SEMANTIC_PROMPT = 2
} PhuxCellSemanticContent;

typedef enum PhuxUnderlineStyle {
    PHUX_UNDERLINE_NONE = 0,
    PHUX_UNDERLINE_SINGLE = 1,
    PHUX_UNDERLINE_DOUBLE = 2,
    PHUX_UNDERLINE_CURLY = 3,
    PHUX_UNDERLINE_DOTTED = 4,
    PHUX_UNDERLINE_DASHED = 5
} PhuxUnderlineStyle;

typedef enum PhuxCursorStyle {
    PHUX_CURSOR_BAR = 0,
    PHUX_CURSOR_BLOCK = 1,
    PHUX_CURSOR_UNDERLINE = 2,
    PHUX_CURSOR_BLOCK_HOLLOW = 3
} PhuxCursorStyle;

/** Physical-key values are stable and match phux protocol 0.7/libghostty. */
typedef enum PhuxPhysicalKey {
    PHUX_KEY_UNIDENTIFIED = 0,
    PHUX_KEY_BACKQUOTE = 1,
    PHUX_KEY_BACKSLASH = 2,
    PHUX_KEY_BRACKET_LEFT = 3,
    PHUX_KEY_BRACKET_RIGHT = 4,
    PHUX_KEY_COMMA = 5,
    PHUX_KEY_DIGIT0 = 6,
    PHUX_KEY_DIGIT1 = 7,
    PHUX_KEY_DIGIT2 = 8,
    PHUX_KEY_DIGIT3 = 9,
    PHUX_KEY_DIGIT4 = 10,
    PHUX_KEY_DIGIT5 = 11,
    PHUX_KEY_DIGIT6 = 12,
    PHUX_KEY_DIGIT7 = 13,
    PHUX_KEY_DIGIT8 = 14,
    PHUX_KEY_DIGIT9 = 15,
    PHUX_KEY_EQUAL = 16,
    PHUX_KEY_INTL_BACKSLASH = 17,
    PHUX_KEY_INTL_RO = 18,
    PHUX_KEY_INTL_YEN = 19,
    PHUX_KEY_A = 20,
    PHUX_KEY_B = 21,
    PHUX_KEY_C = 22,
    PHUX_KEY_D = 23,
    PHUX_KEY_E = 24,
    PHUX_KEY_F = 25,
    PHUX_KEY_G = 26,
    PHUX_KEY_H = 27,
    PHUX_KEY_I = 28,
    PHUX_KEY_J = 29,
    PHUX_KEY_K = 30,
    PHUX_KEY_L = 31,
    PHUX_KEY_M = 32,
    PHUX_KEY_N = 33,
    PHUX_KEY_O = 34,
    PHUX_KEY_P = 35,
    PHUX_KEY_Q = 36,
    PHUX_KEY_R = 37,
    PHUX_KEY_S = 38,
    PHUX_KEY_T = 39,
    PHUX_KEY_U = 40,
    PHUX_KEY_V = 41,
    PHUX_KEY_W = 42,
    PHUX_KEY_X = 43,
    PHUX_KEY_Y = 44,
    PHUX_KEY_Z = 45,
    PHUX_KEY_MINUS = 46,
    PHUX_KEY_PERIOD = 47,
    PHUX_KEY_QUOTE = 48,
    PHUX_KEY_SEMICOLON = 49,
    PHUX_KEY_SLASH = 50,
    PHUX_KEY_ALT_LEFT = 51,
    PHUX_KEY_ALT_RIGHT = 52,
    PHUX_KEY_BACKSPACE = 53,
    PHUX_KEY_CAPS_LOCK = 54,
    PHUX_KEY_CONTEXT_MENU = 55,
    PHUX_KEY_CONTROL_LEFT = 56,
    PHUX_KEY_CONTROL_RIGHT = 57,
    PHUX_KEY_ENTER = 58,
    PHUX_KEY_META_LEFT = 59,
    PHUX_KEY_META_RIGHT = 60,
    PHUX_KEY_SHIFT_LEFT = 61,
    PHUX_KEY_SHIFT_RIGHT = 62,
    PHUX_KEY_SPACE = 63,
    PHUX_KEY_TAB = 64,
    PHUX_KEY_CONVERT = 65,
    PHUX_KEY_KANA_MODE = 66,
    PHUX_KEY_NON_CONVERT = 67,
    PHUX_KEY_DELETE = 68,
    PHUX_KEY_END = 69,
    PHUX_KEY_HELP = 70,
    PHUX_KEY_HOME = 71,
    PHUX_KEY_INSERT = 72,
    PHUX_KEY_PAGE_DOWN = 73,
    PHUX_KEY_PAGE_UP = 74,
    PHUX_KEY_ARROW_DOWN = 75,
    PHUX_KEY_ARROW_LEFT = 76,
    PHUX_KEY_ARROW_RIGHT = 77,
    PHUX_KEY_ARROW_UP = 78,
    PHUX_KEY_NUM_LOCK = 79,
    PHUX_KEY_NUMPAD0 = 80,
    PHUX_KEY_NUMPAD1 = 81,
    PHUX_KEY_NUMPAD2 = 82,
    PHUX_KEY_NUMPAD3 = 83,
    PHUX_KEY_NUMPAD4 = 84,
    PHUX_KEY_NUMPAD5 = 85,
    PHUX_KEY_NUMPAD6 = 86,
    PHUX_KEY_NUMPAD7 = 87,
    PHUX_KEY_NUMPAD8 = 88,
    PHUX_KEY_NUMPAD9 = 89,
    PHUX_KEY_NUMPAD_ADD = 90,
    PHUX_KEY_NUMPAD_BACKSPACE = 91,
    PHUX_KEY_NUMPAD_CLEAR = 92,
    PHUX_KEY_NUMPAD_CLEAR_ENTRY = 93,
    PHUX_KEY_NUMPAD_COMMA = 94,
    PHUX_KEY_NUMPAD_DECIMAL = 95,
    PHUX_KEY_NUMPAD_DIVIDE = 96,
    PHUX_KEY_NUMPAD_ENTER = 97,
    PHUX_KEY_NUMPAD_EQUAL = 98,
    PHUX_KEY_NUMPAD_MEMORY_ADD = 99,
    PHUX_KEY_NUMPAD_MEMORY_CLEAR = 100,
    PHUX_KEY_NUMPAD_MEMORY_RECALL = 101,
    PHUX_KEY_NUMPAD_MEMORY_STORE = 102,
    PHUX_KEY_NUMPAD_MEMORY_SUBTRACT = 103,
    PHUX_KEY_NUMPAD_MULTIPLY = 104,
    PHUX_KEY_NUMPAD_PAREN_LEFT = 105,
    PHUX_KEY_NUMPAD_PAREN_RIGHT = 106,
    PHUX_KEY_NUMPAD_SUBTRACT = 107,
    PHUX_KEY_NUMPAD_SEPARATOR = 108,
    PHUX_KEY_NUMPAD_UP = 109,
    PHUX_KEY_NUMPAD_DOWN = 110,
    PHUX_KEY_NUMPAD_RIGHT = 111,
    PHUX_KEY_NUMPAD_LEFT = 112,
    PHUX_KEY_NUMPAD_BEGIN = 113,
    PHUX_KEY_NUMPAD_HOME = 114,
    PHUX_KEY_NUMPAD_END = 115,
    PHUX_KEY_NUMPAD_INSERT = 116,
    PHUX_KEY_NUMPAD_DELETE = 117,
    PHUX_KEY_NUMPAD_PAGE_UP = 118,
    PHUX_KEY_NUMPAD_PAGE_DOWN = 119,
    PHUX_KEY_ESCAPE = 120,
    PHUX_KEY_F1 = 121,
    PHUX_KEY_F2 = 122,
    PHUX_KEY_F3 = 123,
    PHUX_KEY_F4 = 124,
    PHUX_KEY_F5 = 125,
    PHUX_KEY_F6 = 126,
    PHUX_KEY_F7 = 127,
    PHUX_KEY_F8 = 128,
    PHUX_KEY_F9 = 129,
    PHUX_KEY_F10 = 130,
    PHUX_KEY_F11 = 131,
    PHUX_KEY_F12 = 132,
    PHUX_KEY_F13 = 133,
    PHUX_KEY_F14 = 134,
    PHUX_KEY_F15 = 135,
    PHUX_KEY_F16 = 136,
    PHUX_KEY_F17 = 137,
    PHUX_KEY_F18 = 138,
    PHUX_KEY_F19 = 139,
    PHUX_KEY_F20 = 140,
    PHUX_KEY_F21 = 141,
    PHUX_KEY_F22 = 142,
    PHUX_KEY_F23 = 143,
    PHUX_KEY_F24 = 144,
    PHUX_KEY_F25 = 145,
    PHUX_KEY_FN = 146,
    PHUX_KEY_FN_LOCK = 147,
    PHUX_KEY_PRINT_SCREEN = 148,
    PHUX_KEY_SCROLL_LOCK = 149,
    PHUX_KEY_PAUSE = 150,
    PHUX_KEY_BROWSER_BACK = 151,
    PHUX_KEY_BROWSER_FAVORITES = 152,
    PHUX_KEY_BROWSER_FORWARD = 153,
    PHUX_KEY_BROWSER_HOME = 154,
    PHUX_KEY_BROWSER_REFRESH = 155,
    PHUX_KEY_BROWSER_SEARCH = 156,
    PHUX_KEY_BROWSER_STOP = 157,
    PHUX_KEY_EJECT = 158,
    PHUX_KEY_LAUNCH_APP1 = 159,
    PHUX_KEY_LAUNCH_APP2 = 160,
    PHUX_KEY_LAUNCH_MAIL = 161,
    PHUX_KEY_MEDIA_PLAY_PAUSE = 162,
    PHUX_KEY_MEDIA_SELECT = 163,
    PHUX_KEY_MEDIA_STOP = 164,
    PHUX_KEY_MEDIA_TRACK_NEXT = 165,
    PHUX_KEY_MEDIA_TRACK_PREVIOUS = 166,
    PHUX_KEY_POWER = 167,
    PHUX_KEY_SLEEP = 168,
    PHUX_KEY_AUDIO_VOLUME_DOWN = 169,
    PHUX_KEY_AUDIO_VOLUME_MUTE = 170,
    PHUX_KEY_AUDIO_VOLUME_UP = 171,
    PHUX_KEY_WAKE_UP = 172,
    PHUX_KEY_COPY = 173,
    PHUX_KEY_CUT = 174,
    PHUX_KEY_PASTE = 175
} PhuxPhysicalKey;

/** Opaque, owning-thread-only session kernel. Never Send/Sync. */
typedef struct PhuxClient PhuxClient;

typedef enum PhuxClientResult {
    PHUX_CLIENT_OK = 0,
    PHUX_CLIENT_NO_VALUE = 1,
    PHUX_CLIENT_INVALID_ARGUMENT = -1,
    PHUX_CLIENT_INVALID_STATE = -2,
    PHUX_CLIENT_PROTOCOL_ERROR = -3,
    PHUX_CLIENT_ENGINE_ERROR = -4,
    PHUX_CLIENT_OUT_OF_MEMORY = -5,
    PHUX_CLIENT_PANIC = -6
} PhuxClientResult;

typedef enum PhuxClientState {
    PHUX_CLIENT_STATE_NEW = 0,
    PHUX_CLIENT_STATE_HELLO_QUEUED = 1,
    PHUX_CLIENT_STATE_NEGOTIATED = 2,
    PHUX_CLIENT_STATE_ATTACHED = 3,
    PHUX_CLIENT_STATE_DETACHED = 4,
    PHUX_CLIENT_STATE_FAILED = 5
} PhuxClientState;

typedef struct PhuxBytes {
    const uint8_t *data;
    size_t len;
} PhuxBytes;

typedef enum PhuxTerminalIdKind {
    PHUX_TERMINAL_LOCAL = 0,
    PHUX_TERMINAL_SATELLITE = 1
} PhuxTerminalIdKind;

/** For satellite IDs, host is UTF-8 and borrowed for the duration of the call. */
typedef struct PhuxTerminalId {
    uint32_t kind;
    uint32_t id;
    PhuxBytes host;
} PhuxTerminalId;

typedef struct PhuxClientOptions {
    size_t size;
    uint32_t version;
    uint32_t max_bootstrap_chunk_bytes;
    uint32_t max_history_page_bytes;
    uint32_t max_history_page_rows;
    size_t max_history_cache_bytes;
    size_t max_history_materialized_rows;
    size_t history_prefetch_rows;
} PhuxClientOptions;

typedef void (*PhuxClientAttachedCallback)(void *userdata);
typedef void (*PhuxClientFailureCallback)(
    void *userdata,
    PhuxClientResult result,
    PhuxBytes message
);

/**
 * Optional lifecycle callbacks, copied by phux_client_set_callbacks.
 * Callbacks run synchronously on the owning thread only after kernel mutation
 * and effect staging finish. They are strictly non-reentrant: every FFI call
 * made from a callback is rejected (void free is ignored; scalar getters
 * return their failure sentinel). message is borrowed only for the duration
 * of on_failure. NULL callbacks disable that notification.
 */
typedef struct PhuxClientCallbacks {
    size_t size;
    uint32_t version;
    void *userdata;
    PhuxClientAttachedCallback on_attached;
    PhuxClientFailureCallback on_failure;
} PhuxClientCallbacks;

typedef enum PhuxAttachTargetKind {
    PHUX_ATTACH_LAST = 0,
    PHUX_ATTACH_BY_NAME = 1,
    PHUX_ATTACH_BY_ID = 2,
    PHUX_ATTACH_CREATE_IF_MISSING = 3
} PhuxAttachTargetKind;

typedef struct PhuxAttachOptions {
    size_t size;
    uint32_t version;
    uint32_t attach_id;
    uint32_t target_kind;
    uint32_t session_id;
    PhuxBytes name;
    uint16_t cols;
    uint16_t rows;
    bool has_pixel_size;
    uint16_t pixel_width;
    uint16_t pixel_height;
    bool request_scrollback;
    uint32_t scrollback_limit_lines;
} PhuxAttachOptions;

typedef enum PhuxClientEffectKind {
    PHUX_CLIENT_EFFECT_DAMAGE = 1,
    PHUX_CLIENT_EFFECT_STATUS = 2,
    PHUX_CLIENT_EFFECT_JOB = 3
} PhuxClientEffectKind;

typedef enum PhuxClientJobKind {
    PHUX_CLIENT_JOB_WAKEUP = 1
} PhuxClientJobKind;

typedef enum PhuxClientDamageKind {
    PHUX_CLIENT_DAMAGE_FULL = 1,
    PHUX_CLIENT_DAMAGE_ROWS = 2,
    PHUX_CLIENT_DAMAGE_REMOVED = 3
} PhuxClientDamageKind;

typedef enum PhuxClientStatusKind {
    PHUX_CLIENT_STATUS_BELL = 1,
    PHUX_CLIENT_STATUS_TITLE = 2,
    PHUX_CLIENT_STATUS_RESYNC_REQUIRED = 3,
    PHUX_CLIENT_STATUS_SERVER_ERROR = 4,
    PHUX_CLIENT_STATUS_DETACHED = 5,
    PHUX_CLIENT_STATUS_HISTORY = 6,
    PHUX_CLIENT_STATUS_HISTORY_UNAVAILABLE = 7
} PhuxClientStatusKind;
/**
 * status_code on a PHUX_CLIENT_STATUS_DETACHED effect: the DETACHED frame's
 * DetachReason wire value (proto.md 7.2), or PHUX_CLIENT_DETACH_REASON_UNSTATED
 * when the server stated none. bytes carries the frame's human-readable
 * message, which may be empty and must not be parsed. Do not treat UNSTATED as
 * REQUESTED: a server may end an attach without saying why.
 */
typedef enum PhuxClientDetachReason {
    PHUX_CLIENT_DETACH_REQUESTED = 0,
    PHUX_CLIENT_DETACH_SERVER_SHUTDOWN = 1,
    PHUX_CLIENT_DETACH_SESSION_KILLED = 2,
    PHUX_CLIENT_DETACH_REPLACED = 3,
    PHUX_CLIENT_DETACH_PROTOCOL_ERROR = 4,
    PHUX_CLIENT_DETACH_INTERNAL_ERROR = 255,
    PHUX_CLIENT_DETACH_REASON_UNSTATED = 0xFFFF
} PhuxClientDetachReason;

typedef enum PhuxClientHistoryLoadCode {
    PHUX_CLIENT_HISTORY_IDLE = 0,
    PHUX_CLIENT_HISTORY_LOADING = 1,
    PHUX_CLIENT_HISTORY_COMPLETE = 2,
    PHUX_CLIENT_HISTORY_GAP = 3,
    PHUX_CLIENT_HISTORY_STALE = 4,
    PHUX_CLIENT_HISTORY_PRUNED = 5,
    PHUX_CLIENT_HISTORY_TOMBSTONED = 6
} PhuxClientHistoryLoadCode;

typedef enum PhuxClientHistoryUnavailableCode {
    PHUX_CLIENT_HISTORY_UNAVAILABLE_STALE = 0,
    PHUX_CLIENT_HISTORY_UNAVAILABLE_PRUNED = 1,
    PHUX_CLIENT_HISTORY_UNAVAILABLE_RESET = 2,
    PHUX_CLIENT_HISTORY_UNAVAILABLE_RESIZE = 3,
    PHUX_CLIENT_HISTORY_UNAVAILABLE_EXPIRED = 4,
    PHUX_CLIENT_HISTORY_UNAVAILABLE_RELEASED = 5,
    PHUX_CLIENT_HISTORY_UNAVAILABLE_LIMIT = 6,
    PHUX_CLIENT_HISTORY_UNAVAILABLE_CODEC_FAILURE = 7
} PhuxClientHistoryUnavailableCode;

/**
 * status_code is a stable TombstoneReason wire value for RESYNC_REQUIRED,
 * PhuxClientHistoryLoadCode for HISTORY, PhuxClientHistoryUnavailableCode for
 * HISTORY_UNAVAILABLE, PhuxClientDetachReason for DETACHED, and zero
 * otherwise.
 */

/** Borrowed effect. bytes contains title/error detail when defined by kind. Emulator PTY replies never appear here: when HELLO_OK advertises TERMINAL_REPLY they are queued as exact outgoing INPUT_TERMINAL_REPLY frames; without that feature, feed_frame returns PHUX_CLIENT_ENGINE_ERROR and queues no reply. */
typedef struct PhuxClientEffect {
    uint32_t kind;
    uint32_t detail;
    uint32_t status_code;
    PhuxTerminalId terminal_id;
    uint64_t stream_id;
    uint64_t bootstrap_id;
    uint64_t seq;
    uint16_t first_row;
    uint16_t last_row;
    PhuxBytes bytes;
} PhuxClientEffect;

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
    PhuxTerminalId terminal_id;
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

/**
 * Production artifacts that guarantee panic containment MUST be built with
 * `cargo build --profile ffi-release -p phux-client-ffi`; the workspace's
 * ordinary release profile aborts and is not a supported host-library build.
 * Every API then contains Rust panics and returns PHUX_CLIENT_PANIC. A client
 * and every pointer obtained from it are owning-thread-only. feed_frame borrows
 * input only for the call. Returned frame/effect/grid/search/selection buffers
 * are owned by the bridge and remain valid until the next mutable PhuxClient
 * call. Opaque document anchors remain valid until explicitly released or
 * their terminal generation is replaced. count/get/state/last_error and
 * terminal_mouse_tracking are read-only and do not invalidate borrowed
 * pointers; clear calls are mutable. Outbound caller-provided byte fields must
 * not exceed PHUX_CLIENT_MAX_OUTBOUND_BYTES.
 */
PhuxClientResult phux_client_new(const PhuxClientOptions *options, PhuxClient **out_client);
PhuxClientResult phux_client_set_callbacks(PhuxClient *client, const PhuxClientCallbacks *callbacks);
void phux_client_free(PhuxClient *client);
PhuxClientState phux_client_state(const PhuxClient *client);
PhuxClientResult phux_client_last_error(const PhuxClient *client, PhuxBytes *out_error);
PhuxClientResult phux_client_queue_hello(PhuxClient *client, PhuxBytes client_name);
PhuxClientResult phux_client_queue_attach(PhuxClient *client, const PhuxAttachOptions *options);
PhuxClientResult phux_client_feed_frame(PhuxClient *client, const uint8_t *data, size_t len);
size_t phux_client_outgoing_count(const PhuxClient *client);
PhuxClientResult phux_client_outgoing_get(const PhuxClient *client, size_t index, PhuxBytes *out_frame);
PhuxClientResult phux_client_outgoing_clear(PhuxClient *client);
size_t phux_client_effect_count(const PhuxClient *client);
PhuxClientResult phux_client_effect_get(const PhuxClient *client, size_t index, PhuxClientEffect *out_effect);
PhuxClientResult phux_client_effect_clear(PhuxClient *client);
PhuxClientResult phux_client_terminal_grid(PhuxClient *client, const PhuxTerminalId *terminal_id, PhuxTerminalGridView *out_view);
/**
 * Reports whether the published Ghostty terminal has DEC mouse tracking mode
 * 9, 1000, 1002, or 1003 enabled. Returns PHUX_CLIENT_INVALID_STATE before
 * publication or after detach, and PHUX_CLIENT_INVALID_ARGUMENT for null or
 * malformed arguments. This read-only query preserves borrowed bridge views.
 */
PhuxClientResult phux_client_terminal_mouse_tracking(const PhuxClient *client, const PhuxTerminalId *terminal_id, bool *out_enabled);
PhuxClientResult phux_client_send_key(PhuxClient *client, const PhuxTerminalId *terminal_id, const PhuxKeyEvent *event);
PhuxClientResult phux_client_send_mouse(PhuxClient *client, const PhuxTerminalId *terminal_id, const PhuxMouseEvent *event);
PhuxClientResult phux_client_send_focus(PhuxClient *client, const PhuxTerminalId *terminal_id, bool focused);
PhuxClientResult phux_client_send_paste(PhuxClient *client, const PhuxTerminalId *terminal_id, const uint8_t *data, size_t len, bool trusted);
PhuxClientResult phux_client_terminal_resize(PhuxClient *client, const PhuxTerminalId *terminal_id, uint16_t cols, uint16_t rows);
PhuxClientResult phux_client_viewport_resize(PhuxClient *client, uint16_t cols, uint16_t rows, bool has_pixel_size, uint16_t pixel_width, uint16_t pixel_height);
PhuxClientResult phux_client_scroll_viewport(PhuxClient *client, const PhuxTerminalId *terminal_id, uint32_t kind, int64_t value);
PhuxClientResult phux_client_anchor_create(PhuxClient *client, const PhuxTerminalId *terminal_id, PhuxDocumentPoint point, PhuxDocumentAnchor *out_anchor);
PhuxClientResult phux_client_anchor_release(PhuxClient *client, const PhuxTerminalId *terminal_id, PhuxDocumentAnchor anchor);
PhuxClientResult phux_client_history_viewport_pin(PhuxClient *client, const PhuxTerminalId *terminal_id, PhuxDocumentAnchor anchor);
PhuxClientResult phux_client_history_follow_live(PhuxClient *client, const PhuxTerminalId *terminal_id);
PhuxClientResult phux_client_selection_set(PhuxClient *client, const PhuxTerminalId *terminal_id, PhuxDocumentAnchor start, PhuxDocumentAnchor end, bool rectangle);
PhuxClientResult phux_client_selection_clear(PhuxClient *client, const PhuxTerminalId *terminal_id);
PhuxClientResult phux_client_selection_text(PhuxClient *client, const PhuxTerminalId *terminal_id, PhuxBytes *out_text);
/**
 * Every returned anchor handle is transferred to the caller and remains valid
 * until explicitly released or its terminal generation is replaced. Before
 * the next mutable client call invalidates this borrowed array, callers must
 * either copy the handles for later individual release or call
 * phux_client_search_results_release to release the entire set atomically.
 */
PhuxClientResult phux_client_search(PhuxClient *client, const PhuxTerminalId *terminal_id, PhuxBytes query_utf8, bool case_sensitive, const PhuxSearchResult **out_results, size_t *out_count);
PhuxClientResult phux_client_search_results_release(PhuxClient *client);

#ifdef __cplusplus
}
#endif
#endif
