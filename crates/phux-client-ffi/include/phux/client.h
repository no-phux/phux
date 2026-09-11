#ifndef PHUX_CLIENT_H
#define PHUX_CLIENT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PHUX_CLIENT_ABI_VERSION 2u
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

typedef enum PhuxResourceIdKind {
    PHUX_RESOURCE_ID_LOCAL = 0,
    PHUX_RESOURCE_ID_SATELLITE = 1
} PhuxResourceIdKind;

/** For satellite IDs, host is UTF-8 and borrowed for the duration of the call. */
typedef struct PhuxResourceId {
    uint32_t kind;
    uint32_t id;
    PhuxBytes host;
} PhuxResourceId;

/** Borrowed server-owned session summary from the latest ATTACHED snapshot. */
typedef struct PhuxSessionInfo {
    uint32_t session_id;
    PhuxBytes name;
    int64_t created_at_unix_secs;
    uint16_t window_count;
    uint16_t attached_client_count;
    bool focused;
} PhuxSessionInfo;

/**
 * Wire ResourceKind tags. PhuxResourceInfo.kind carries the raw tag, so a kind
 * this header does not name still reaches the host; treat it as opaque and
 * never as a terminal.
 */
typedef enum PhuxResourceKind {
    PHUX_RESOURCE_TERMINAL = 0,
    PHUX_RESOURCE_AGENT_SESSION = 1
} PhuxResourceKind;

/**
 * Borrowed resource summary from the latest ATTACHED snapshot. Initialize
 * size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION before
 * phux_client_resource_get. Every resource the snapshot listed appears, across
 * sessions and kinds, minus resources the server has since reported closed.
 * Only PHUX_RESOURCE_TERMINAL resources in the focused session take part in the
 * attach and have grids; other kinds never publish a replica and refuse
 * terminal-facet calls with PHUX_CLIENT_INVALID_STATE. parent is NULL when the
 * resource has no parent, otherwise it borrows bridge storage until the next
 * mutable call. provider/native_id/state are UTF-8 spans, empty for kinds
 * without an agent facet.
 */
typedef struct PhuxResourceInfo {
    size_t size;
    uint32_t version;
    PhuxResourceId terminal_id;
    uint32_t kind;
    const PhuxResourceId *parent;
    PhuxBytes provider;
    PhuxBytes native_id;
    PhuxBytes state;
} PhuxResourceInfo;

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

/**
 * Session selector for phux_client_queue_attach.
 *
 * PHUX_ATTACH_LAST is resolved entirely by the server: prior touched activity
 * wins, otherwise the server may select its configured live seed. It never
 * creates a session. Creation requires the explicit
 * PHUX_ATTACH_CREATE_IF_MISSING selector.
 */
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

#define PHUX_CLIENT_MAX_OPERATIONS 128u
#define PHUX_CLIENT_MAX_DYNAMIC_TERMINALS 256u
#define PHUX_CLIENT_MAX_SPAWN_ARGS 256u
#define PHUX_CLIENT_MAX_SPAWN_BYTES (64u * 1024u)
#define PHUX_CLIENT_MAX_OPERATION_MESSAGE_BYTES 4096u

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * Request IDs are in 1..0x7fffffff and strictly increasing across
 * spawn/attach/detach-terminal and workspace refresh/mutation calls for this
 * client, including after results are cleared. Validation failure
 * does not consume an ID. All operations require completed session ATTACH.
 * Pending requests plus retained completions are bounded by MAX_OPERATIONS;
 * consume/clear completions to free capacity. Spawn is never retried internally.
 * New operations also require fewer than MAX_OPERATIONS queued outgoing frames;
 * drain/clear outgoing frames after handing them to the transport.
 * All input spans are copied by the queue call, UTF-8, and reject embedded NUL.
 * argc == 0 selects the default shell; otherwise argv[0] must be nonempty.
 * Empty cwd/satellite and null owner_terminal mean absent. A nonzero local owner
 * requires satellite absent. A nonzero satellite owner requires an explicit
 * satellite route exactly matching its host; mismatches are rejected. The wire
 * retains the satellite-tagged owner for server-side route translation.
 * Text across argv/cwd/route/owner host is bounded by MAX_SPAWN_BYTES (both host
 * spans count when an owner is present). Both geometry axes must be nonzero.
 * Geometry is an initial hint; older servers and satellite relays may ignore it.
 */
typedef struct PhuxSpawnOptions {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    const PhuxResourceId *owner_terminal;
    PhuxBytes satellite;
    const PhuxBytes *argv;
    size_t argc;
    PhuxBytes cwd;
    uint16_t cols;
    uint16_t rows;
} PhuxSpawnOptions;

/** Explicitly admits this ID before bootstrap can arrive, including before the
 * COMMAND_RESULT acknowledgment. Already admitted or pending IDs are rejected. A successful
 * local spawn is automatically admitted; a satellite spawn needs this call.
 */
typedef struct PhuxAttachResourceOptions {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    PhuxResourceId terminal_id;
} PhuxAttachResourceOptions;

typedef PhuxAttachResourceOptions PhuxDetachResourceOptions;

typedef enum PhuxOperationKind {
    PHUX_OPERATION_SPAWN = 1,
    PHUX_OPERATION_ATTACH_RESOURCE = 2,
    PHUX_OPERATION_DETACH_RESOURCE = 3
} PhuxOperationKind;

typedef enum PhuxOperationStatus {
    PHUX_OPERATION_SUCCESS = 1,
    PHUX_OPERATION_REFUSED = 2,
    PHUX_OPERATION_UNKNOWN_OUTCOME = 3
} PhuxOperationStatus;

typedef enum PhuxOperationErrorDomain {
    PHUX_OPERATION_ERROR_NONE = 0,
    PHUX_OPERATION_ERROR_SPAWN = 1,
    PHUX_OPERATION_ERROR_PROTOCOL = 2
} PhuxOperationErrorDomain;

/** Initialize size/version before operation_get. Success is command acceptance,
 * not stream READY; use terminal_grid to observe a published replica. Spawn
 * errors use wire SpawnError tags (0 group missing, 1 spawn failed, 2 unsupported
 * satellite, 3 satellite unreachable). Protocol errors use ErrorCode wire values.
 * id == 0 means absent; attach/detach results retain their requested ID even on failure.
 * Host/message spans are borrowed until the next mutable client call. Messages
 * are truncated at a UTF-8 boundary to MAX_OPERATION_MESSAGE_BYTES.
 * UNKNOWN_OUTCOME means transport ended before a reply; reconcile against server
 * inventory/identity, never automatically retry creation.
 */
typedef struct PhuxOperationResult {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    uint32_t kind;
    uint32_t status;
    uint32_t error_domain;
    uint32_t error_code;
    PhuxResourceId terminal_id;
    PhuxBytes message;
} PhuxOperationResult;

/**
 * PHUX_CLIENT_EFFECT_AGENT_RECORDS carries AgentEventsJsonlV1 records from an
 * AgentSession resource (PhuxResourceInfo.kind == PHUX_RESOURCE_AGENT_SESSION).
 * terminal_id is the resource id, stream_id/bootstrap_id the generation its
 * BOOTSTRAP_BEGIN opened, detail a PhuxClientAgentRecordsKind, and bytes zero
 * or more complete records, one JSON object per line
 * ({"seq","ts_ms","type","data"}; key order is not significant). The kernel
 * validated every record before it reached the bridge; a stream whose payload
 * fails validation retires its generation with a STATUS RESYNC_REQUIRED effect
 * instead. RETAINED is the whole retained backlog of a generation, delivered
 * once at BOOTSTRAP_READY; LIVE is one live output frame; seq is the newest
 * record's server-assigned sequence for both. CLOSED has empty bytes and
 * retires the resource from the catalog. Hosts must tolerate effect kinds they
 * do not recognise; new kinds are additive.
 */
typedef enum PhuxClientEffectKind {
    PHUX_CLIENT_EFFECT_DAMAGE = 1,
    PHUX_CLIENT_EFFECT_STATUS = 2,
    PHUX_CLIENT_EFFECT_JOB = 3,
    PHUX_CLIENT_EFFECT_AGENT_RECORDS = 4
} PhuxClientEffectKind;

typedef enum PhuxClientAgentRecordsKind {
    PHUX_CLIENT_AGENT_RECORDS_RETAINED = 1,
    PHUX_CLIENT_AGENT_RECORDS_LIVE = 2,
    PHUX_CLIENT_AGENT_RECORDS_CLOSED = 3
} PhuxClientAgentRecordsKind;

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
    PHUX_CLIENT_HISTORY_TOMBSTONED = 6,
    PHUX_CLIENT_HISTORY_CLEARED = 7
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
    PhuxResourceId terminal_id;
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

/**
 * Production artifacts that guarantee panic containment MUST be built with
 * `cargo build --profile ffi-release -p phux-client-ffi`; the workspace's
 * ordinary release profile aborts and is not a supported host-library build.
 * Every API then contains Rust panics and returns PHUX_CLIENT_PANIC. A client
 * and every pointer obtained from it are owning-thread-only. feed_frame borrows
 * input only for the call. Returned frame/effect/grid/search/selection buffers
 * are owned by the bridge and remain valid until the next mutable PhuxClient
 * call. Opaque document anchors remain valid until explicitly released or
 * their terminal generation is replaced or its presentation is cleared.
 * count/get/state/last_error and
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
PhuxClientResult phux_client_queue_spawn(PhuxClient *client, const PhuxSpawnOptions *options);
PhuxClientResult phux_client_queue_attach_resource(PhuxClient *client, const PhuxAttachResourceOptions *options);

/* Withdraw a subscription, never kill durable work. Requires completed session
 * ATTACH and an admitted terminal without a pending attach/detach. Correlated
 * success retires client replica/admission, including initial participation.
 * Refusal retains state; disconnect yields unknown outcome. Never replay
 * automatically. Detach remains available when dynamic admission capacity is full.
 * Shares the monotonically increasing request IDs and bounded result queue.
 * While pending, input/resize are refused and automatic terminal sends are
 * fenced behind withdrawal. At most one unsent history request and latest ACK
 * are retained per detach, resumed only on refusal with a still-live exact
 * generation/cursor. Success/closure/disconnect discard them; input is never
 * retained or replayed. */
PhuxClientResult phux_client_queue_detach_resource(PhuxClient *client, const PhuxDetachResourceOptions *options);
size_t phux_client_operation_count(const PhuxClient *client);
PhuxClientResult phux_client_operation_get(const PhuxClient *client, size_t index, PhuxOperationResult *out_result);
/** Clears completions only, preserving pending correlation and stream admission. */
PhuxClientResult phux_client_operation_clear(PhuxClient *client);
/** Opaque HELLO_OK identity, borrowed until mutation and retained after disconnect. */
PhuxClientResult phux_client_server_id(const PhuxClient *client, PhuxBytes *out_id);
/** Call on transport loss: cancels pending requests as unknown outcome, discards
 * outgoing frames, and permanently detaches this client. Idempotent. */
PhuxClientResult phux_client_disconnect(PhuxClient *client);
PhuxClientResult phux_client_feed_frame(PhuxClient *client, const uint8_t *data, size_t len);
size_t phux_client_session_count(const PhuxClient *client);
PhuxClientResult phux_client_session_get(const PhuxClient *client, size_t index, PhuxSessionInfo *out_session);
/* Read-only resource catalog from the latest ATTACHED (see PhuxResourceInfo).
 * Zero before ATTACHED or for an invalid client. Spans borrowed until the next
 * mutable call. */
size_t phux_client_resource_count(const PhuxClient *client);
PhuxClientResult phux_client_resource_get(const PhuxClient *client, size_t index, PhuxResourceInfo *out_resource);
/* Rust-owned shared topology, ABI version 1. All output records require initialized
 * size/version. Borrowed names/IDs expire on the next mutable client call.
 * Catalog bounds: 256 sessions, 256 terminals; topology: 32 windows, 512 nodes,
 * depth 64. Names/title/cwd/host are bounded to 4096 UTF-8 bytes (refusal, no truncation).
 * Registry window IDs are NOT these durable 128-bit layout window IDs.
 * Initial attachment exposes catalog only (state 0) until metadata/state replies
 * complete. Only confirmed metadata absence yields fallback. Present layouts
 * require schema v3 and stable IDs; unsupported stored data is refused, not reset.
 * No focus is shared; clients preserve their own selection by stable IDs. */
typedef struct PhuxWorkspaceInfo {
    size_t size;
    uint32_t version;
    uint64_t revision;
    uint32_t session_id;
    /* state: 0 unavailable, 1 fallback, 2 authoritative, 3 last-good with error. */
    uint32_t state;
    uint32_t window_count, node_count, terminal_count;
    /* Latest refresh/mutation: 0 idle, 1 pending, 2 confirmed, 3 refused,
     * 4 disconnected/unknown outcome. request_id=0 is automatic initial read. */
    uint32_t request_id, status;
    PhuxBytes message;
} PhuxWorkspaceInfo;
typedef struct PhuxWorkspaceWindow {
    size_t size;
    uint32_t version;
    uint8_t window_id[16];
    PhuxBytes name;
    uint32_t root_node;
} PhuxWorkspaceWindow;
typedef struct PhuxWorkspaceNode {
    size_t size;
    uint32_t version;
    /* leaf=1, side-by-side=2, stacked=3; child indices are snapshot-global. */
    uint32_t kind;
    PhuxResourceId terminal_id;
    uint32_t first, second;
    float ratio;
} PhuxWorkspaceNode;
typedef struct PhuxCatalogTerminal {
    size_t size;
    uint32_t version;
    PhuxResourceId terminal_id;
    /* 0 means unknown ownership (e.g. satellite); never inferred from numeric ID. */
    uint32_t session_id;
    PhuxBytes title, cwd;
} PhuxCatalogTerminal;
typedef struct PhuxWorkspaceMutation {
    size_t size;
    uint32_t version, request_id;
    uint64_t expected_revision;
    uint32_t session_id;
    /* add=1, split=2, remove presentation=3, reorder=4, resize=5, rename=6,
     * remove entire window presentation=7. Neither removal kills terminals. */
    uint32_t kind;
    uint8_t window_id[16];
    /* add: terminal_id seeds a Rust-minted ID (window_id ignored).
     * split: terminal_id is target, new_terminal_id is the new sibling. */
    PhuxResourceId terminal_id, new_terminal_id;
    PhuxBytes name;
    /* split direction: side-by-side=2, stacked=3; resize uses ratio only.
     * Both require a finite ratio strictly between 0 and 1. */
    uint32_t direction, index;
    float ratio;
    /* Root-to-split path, low bit first: 0 first child, 1 second child. */
    uint32_t path_len;
    uint64_t path_bits;
} PhuxWorkspaceMutation;
/* Host request IDs across spawn/subscribe/refresh/mutate must strictly increase,
 * 1..0x7fffffff. The bridge reserves the upper half for internal correlation.
 * One refresh OR mutation may be pending. Poll <=1s and before palette display.
 * Refresh never changes the actual attached session or allocates emulators.
 * Mutation is whole-value LWW SET followed by GET confirmation, NOT CAS:
 * expected_revision fences this client's snapshot, not concurrent server writers.
 * Adopt only the confirmed current snapshot; never retry unknown spawns. */
PhuxClientResult phux_client_workspace_refresh(PhuxClient *client, uint32_t request_id);
PhuxClientResult phux_client_workspace_mutate(PhuxClient *client, const PhuxWorkspaceMutation *mutation);
PhuxClientResult phux_client_workspace_info(const PhuxClient *client, PhuxWorkspaceInfo *out_info);
PhuxClientResult phux_client_workspace_window_get(const PhuxClient *client, size_t index, PhuxWorkspaceWindow *out_window);
PhuxClientResult phux_client_workspace_node_get(const PhuxClient *client, size_t index, PhuxWorkspaceNode *out_node);
PhuxClientResult phux_client_catalog_terminal_get(const PhuxClient *client, size_t index, PhuxCatalogTerminal *out_terminal);
size_t phux_client_outgoing_count(const PhuxClient *client);
PhuxClientResult phux_client_outgoing_get(const PhuxClient *client, size_t index, PhuxBytes *out_frame);
PhuxClientResult phux_client_outgoing_clear(PhuxClient *client);
size_t phux_client_effect_count(const PhuxClient *client);
PhuxClientResult phux_client_effect_get(const PhuxClient *client, size_t index, PhuxClientEffect *out_effect);
PhuxClientResult phux_client_effect_clear(PhuxClient *client);
PhuxClientResult phux_client_terminal_grid(PhuxClient *client, const PhuxResourceId *terminal_id, PhuxTerminalGridView *out_view);

/* Read-only companion to terminal_grid. Call immediately after that query,
 * before ANY mutable client call; both borrows remain valid. Returns
 * INVALID_STATE when there is no current grid. Initialize size/version first.
 * Defaults/palette entries come from the same libghostty render pass. Missing
 * default colors use the renderer's configured fallback (also swapped under
 * reverse_colors); missing cursor color uses its configured cursor fallback. */
PhuxClientResult phux_client_terminal_grid_metadata(const PhuxClient *client, const PhuxResourceId *terminal_id, PhuxTerminalGridMetadata *out_metadata);
/**
 * Reports whether the published Ghostty terminal's effective mouse-tracking
 * state is active (X10, normal, button, or any-event). Returns
 * PHUX_CLIENT_INVALID_STATE before publication or after detach, and
 * PHUX_CLIENT_INVALID_ARGUMENT for null or
 * malformed arguments. This read-only query preserves borrowed bridge views.
 */
PhuxClientResult phux_client_terminal_mouse_tracking(const PhuxClient *client, const PhuxResourceId *terminal_id, bool *out_enabled);
PhuxClientResult phux_client_send_key(PhuxClient *client, const PhuxResourceId *terminal_id, const PhuxKeyEvent *event);
PhuxClientResult phux_client_send_mouse(PhuxClient *client, const PhuxResourceId *terminal_id, const PhuxMouseEvent *event);
PhuxClientResult phux_client_send_focus(PhuxClient *client, const PhuxResourceId *terminal_id, bool focused);
PhuxClientResult phux_client_send_paste(PhuxClient *client, const PhuxResourceId *terminal_id, const uint8_t *data, size_t len, bool trusted);
PhuxClientResult phux_client_terminal_resize(PhuxClient *client, const PhuxResourceId *terminal_id, uint16_t cols, uint16_t rows);
PhuxClientResult phux_client_viewport_resize(PhuxClient *client, uint16_t cols, uint16_t rows, bool has_pixel_size, uint16_t pixel_width, uint16_t pixel_height);
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
 * the first output frame for that terminal. The returned bytes are borrowed
 * from the client and stay valid until the next phux_client_perf_json call.
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

/* ------------------------------------------------- host directory listing
 *
 * LIST_DIRECTORY / DIRECTORY_LISTING (docs/spec/L3.md section 4): the child
 * directories of one path on the serving server's host, for a go-to-directory
 * picker. Additive to ABI version 2; no existing declaration changes. The
 * answer comes from whichever server this client is connected to, local or
 * a remote host reached through a tunnel.
 *
 * Requires an attached client whose HELLO_OK advertised LIST_DIRECTORY
 * (0x00008000); otherwise PHUX_CLIENT_INVALID_STATE and nothing is queued,
 * because an older server drops the frame and a reply would never come.
 * request_id shares the strictly increasing host request space with spawn,
 * subscribe and workspace refresh/mutation. path is UTF-8 without NUL, at
 * most 4096 bytes: empty or "~" for the serving user's home, "~/rest", or an
 * absolute path; the server normalizes it lexically and follows no symlinks.
 *
 * The client retains exactly one listing. A new request replaces it, and a
 * reply answering any request but the latest is dropped silently, so a
 * picker that was cancelled or moved on never sees a late answer. A
 * correlated ERROR settles the listing as REFUSED/OTHER. Disconnecting while
 * PENDING yields UNKNOWN_OUTCOME. The frame itself is read by the embedder's
 * ordinary phux_client_feed_frame loop; poll info after feeding. */
typedef enum PhuxDirectoryStatus {
    PHUX_DIRECTORY_NONE = 0,            /* nothing requested yet */
    PHUX_DIRECTORY_PENDING = 1,         /* queued or on the wire */
    PHUX_DIRECTORY_LISTED = 2,          /* entries are valid */
    PHUX_DIRECTORY_REFUSED = 3,         /* error_code and message say why */
    PHUX_DIRECTORY_UNKNOWN_OUTCOME = 4  /* connection ended first */
} PhuxDirectoryStatus;

/* Wire DirectoryErrorCode; an unallocated wire value reads as OTHER. */
typedef enum PhuxDirectoryError {
    PHUX_DIRECTORY_NOT_FOUND = 0,
    PHUX_DIRECTORY_PERMISSION_DENIED = 1,
    PHUX_DIRECTORY_NOT_A_DIRECTORY = 2,
    PHUX_DIRECTORY_OTHER = 3
} PhuxDirectoryError;

/* Entry flag bit 0: a symbolic link resolving to a directory. Other bits are
 * reserved and must be ignored. */
#define PHUX_DIRECTORY_ENTRY_SYMLINK 0x1u

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * supported reports the negotiated feature bit. For LISTED, path is the
 * resolved absolute path, parent its lexical parent (has_parent false at the
 * root), entry_count at most 1024, sorted by name in ascending byte order,
 * and truncated set when the server cut the listing short. For REFUSED, path
 * is the path the server attempted and message is diagnostic text that must
 * not be parsed. For PENDING, path is the requested path. Spans are borrowed
 * until the next mutable client call. */
typedef struct PhuxDirectoryListingInfo {
    size_t size;
    uint32_t version;
    bool supported;
    bool truncated;
    bool has_parent;
    uint32_t request_id;
    uint32_t status;
    uint32_t error_code;
    uint32_t entry_count;
    PhuxBytes path;
    PhuxBytes parent;
    PhuxBytes message;
} PhuxDirectoryListingInfo;

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * name is one path component, borrowed until the next mutable client call. */
typedef struct PhuxDirectoryEntry {
    size_t size;
    uint32_t version;
    uint32_t flags;
    PhuxBytes name;
} PhuxDirectoryEntry;

PhuxClientResult phux_client_list_directory(PhuxClient *client, uint32_t request_id, PhuxBytes path);

/* A satellite's directories (docs/spec/L3.md section 4.1). Additive to ABI
 * version 2. Attached to a federation hub, a satellite pane's directories
 * live on the satellite; LIST_DIRECTORY.host names it and the hub relays the
 * request. An older hub skips the field and lists ITSELF, so a nonempty host
 * requires HELLO_OK to have advertised LIST_DIRECTORY_HOST (0x00080000) as
 * well as LIST_DIRECTORY. Without it: PHUX_CLIENT_INVALID_STATE, nothing
 * queued, no request ID consumed, and the retained listing unchanged. An
 * embedder that still wants a listing then asks for the serving host's own
 * with an empty host, and says whose directories it shows.
 *
 * Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * request_id and path are exactly as for phux_client_list_directory. host is
 * the satellite name as a satellite-tagged PhuxResourceId carries it: UTF-8
 * without NUL, at most 255 bytes; empty is the serving host, and the frame is
 * then byte-identical to phux_client_list_directory's. A relayed refusal
 * (unknown or unreachable satellite, a satellite without LIST_DIRECTORY, the
 * relay deadline) arrives as an ordinary REFUSED/OTHER listing whose message
 * names the host. */
typedef struct PhuxDirectoryRequest {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    PhuxBytes path;
    PhuxBytes host;
} PhuxDirectoryRequest;

PhuxClientResult phux_client_list_directory_on(PhuxClient *client, const PhuxDirectoryRequest *request);
/** *out_supported: HELLO_OK advertised both LIST_DIRECTORY and
 * LIST_DIRECTORY_HOST, so a nonempty PhuxDirectoryRequest.host is accepted. */
PhuxClientResult phux_client_directory_host_supported(const PhuxClient *client, bool *out_supported);
PhuxClientResult phux_client_directory_info(const PhuxClient *client, PhuxDirectoryListingInfo *out_info);
/** PHUX_CLIENT_INVALID_ARGUMENT for an index at or past entry_count. */
PhuxClientResult phux_client_directory_entry_get(const PhuxClient *client, size_t index, PhuxDirectoryEntry *out_entry);

/* ------------------------------------------ sessions without attaching
 *
 * GET_STATE after HELLO_OK, for a client that only needs a server's session
 * list, such as the standby coordinator of a multi-host selector. An
 * attached client is a subscriber: it contributes its viewport to every
 * pane's window-size policy and streams every pane. A client that only
 * queries does neither. Additive to ABI version 2.
 *
 * Requires a negotiated client that has neither attached nor queued ATTACH,
 * with no query already pending. request_id shares the strictly increasing
 * host request space. The reply replaces the list phux_client_session_count
 * and phux_client_session_get read, validated and bounded as ATTACHED's
 * catalog is; query again to refresh it. A refusal (correlated ERROR or an
 * error result) settles the query as REFUSED and keeps the previous list.
 * Disconnecting while PENDING yields UNKNOWN_OUTCOME. */
typedef enum PhuxSessionQueryStatus {
    PHUX_SESSION_QUERY_NONE = 0,
    PHUX_SESSION_QUERY_PENDING = 1,
    PHUX_SESSION_QUERY_OK = 2,
    PHUX_SESSION_QUERY_REFUSED = 3,
    PHUX_SESSION_QUERY_UNKNOWN_OUTCOME = 4
} PhuxSessionQueryStatus;

PhuxClientResult phux_client_query_sessions(PhuxClient *client, uint32_t request_id);
/** Writes the latest query's request ID (0 before any) and status. */
PhuxClientResult phux_client_session_query_status(const PhuxClient *client, uint32_t *out_request_id, uint32_t *out_status);

/* ---------------------------------------------------------- remote hosts
 *
 * Reach a remote phux server the way `phux attach --remote HOST` does
 * (ADR-0007, ADR-0031, ADR-0093 rung 1) without implementing QUIC or TLS.
 * Additive to ABI version 2; no existing declaration changes.
 *
 * PhuxClient stays sans-IO. The embedder creates a connected SOCK_STREAM
 * Unix-domain socket pair, keeps one end for exactly the framed I/O it
 * already does against a local server's socket, and hands the other end to
 * a tunnel. The tunnel dials the host and relays SPEC section 5 frames
 * through the pair unchanged; nothing is decoded.
 *
 * Hosts come from the CLI's own [[remote]] registry in the phux config.toml
 * (`phux host add|enroll`, `phux --remote`); there is no second registry.
 * A target is a registry name or [USER@]HOST[:PORT], matched as the CLI
 * matches it: the exact name, then the bare host, then any entry whose
 * endpoint addresses that host. An explicit :PORT overrides the endpoint for
 * this dial only. quic:// and wss:// (ws:// on loopback) are dialed; ssh://
 * is refused because it needs a terminal. Pairing is never attempted: an
 * unregistered host fails with a message naming the CLI command that pairs
 * it. The bearer token is read from the entry's token file inside the tunnel
 * and never crosses this ABI. Trust matches the CLI: a routable host needs
 * its certificate pin, and a routable WebSocket also needs wss:// and a
 * token. Name resolution plus establishment is bounded to 15 seconds.
 *
 * Linking: on macOS the static archive now also references CoreFoundation
 * (the registry is read through phux-config, whose clock dependency asks
 * CoreFoundation for the system time zone). Link -framework CoreFoundation;
 * an AppKit application already does.
 */
typedef struct PhuxRemoteTunnel PhuxRemoteTunnel;

typedef enum PhuxRemoteTunnelState {
    PHUX_REMOTE_TUNNEL_RESOLVED = 0,   /* registry entry found; not started */
    PHUX_REMOTE_TUNNEL_CONNECTING = 1, /* dialing */
    PHUX_REMOTE_TUNNEL_CONNECTED = 2,  /* relaying frames */
    PHUX_REMOTE_TUNNEL_FAILED = 3,     /* terminal; message says why */
    PHUX_REMOTE_TUNNEL_CLOSED = 4      /* terminal; embedder end closed or freed */
} PhuxRemoteTunnelState;

typedef enum PhuxRemoteTransport {
    PHUX_REMOTE_TRANSPORT_NONE = 0,
    PHUX_REMOTE_TRANSPORT_QUIC = 1,
    PHUX_REMOTE_TRANSPORT_WS = 2
} PhuxRemoteTransport;

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * target: UTF-8 without NUL, at most 1024 bytes. config_path: empty for the
 * CLI's resolution ($XDG_CONFIG_HOME/phux/config.toml, else
 * ~/.config/phux/config.toml), otherwise an absolute path. */
typedef struct PhuxRemoteTarget {
    size_t size;
    uint32_t version;
    PhuxBytes target;
    PhuxBytes config_path;
} PhuxRemoteTarget;

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * Spans are borrowed from the tunnel until phux_remote_tunnel_free. name is
 * the registry entry (the typed target when resolution failed); endpoint is
 * the effective URI after any :PORT override; session is the entry's pinned
 * session or empty; message is empty unless state is FAILED. */
typedef struct PhuxRemoteTunnelInfo {
    size_t size;
    uint32_t version;
    uint32_t state;
    uint32_t transport;
    PhuxBytes name;
    PhuxBytes endpoint;
    PhuxBytes session;
    PhuxBytes message;
} PhuxRemoteTunnelInfo;

/** Resolve without touching the network: reads config.toml only. The token
 * file is read by the tunnel thread just before it dials, and the owned
 * copy is dropped as soon as the dial completes. Returns PHUX_CLIENT_OK with a tunnel even for an unregistered
 * host; that tunnel is FAILED with a message. Only malformed arguments fail
 * the call. */
PhuxClientResult phux_remote_tunnel_resolve(const PhuxRemoteTarget *target, PhuxRemoteTunnel **out_tunnel);
/** Thread-safe with respect to the tunnel's own thread and to a concurrent
 * phux_remote_tunnel_start; may be called from any thread until free. */
PhuxClientResult phux_remote_tunnel_info(const PhuxRemoteTunnel *tunnel, PhuxRemoteTunnelInfo *out_info);
/** Requires RESOLVED; otherwise PHUX_CLIENT_INVALID_STATE. Ownership of
 * transport_fd transfers on EVERY return path, including failure. On macOS
 * set SO_NOSIGPIPE on both ends first: the tunnel writes from a library
 * thread and cannot change the process's SIGPIPE disposition. On failure the
 * tunnel publishes FAILED and its message BEFORE closing transport_fd, so
 * the embedder reads EOF only after the reason is readable. */
PhuxClientResult phux_remote_tunnel_start(PhuxRemoteTunnel *tunnel, int transport_fd);
/** Cancels any dial, closes the connection, and joins the tunnel thread;
 * bounded by one scheduler poll, never by the network. Must not race info. */
void phux_remote_tunnel_free(PhuxRemoteTunnel *tunnel);

#ifdef __cplusplus
}
#endif
#endif
