#ifndef PHUX_CLIENT_DETAIL_TYPES_H
#define PHUX_CLIENT_DETAIL_TYPES_H

/* Internal section of phux/client.h; include the umbrella header instead. */

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

/** Borrowed server-owned session summary from the latest ATTACHED snapshot.
 * Frozen at its version 2 layout: window_count is the legacy count (one per
 * session with a terminal, else zero), which the client derives from the
 * snapshot's per-resource session facets when the server reports session
 * lifetimes; read PhuxSessionRecord at the same index for keys,
 * lifetime, revision, members, and state counts. */
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
 * Borrowed resource summary from the latest accepted ATTACHED or GET_STATE
 * snapshot. Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION before
 * phux_client_resource_get. Every resource the snapshot listed appears, across
 * sessions and kinds, minus resources the server has since reported closed.
 * Only PHUX_RESOURCE_TERMINAL resources in the focused session take part in the
 * attach and have grids; other kinds never publish a replica and refuse
 * terminal-facet calls with PHUX_CLIENT_INVALID_STATE. parent is NULL when the
 * resource has no parent, otherwise it borrows bridge storage until the next
 * mutable call. provider/native_id/state are UTF-8 spans, empty for kinds
 * without an agent facet. session_id is the live session it belongs to (0
 * when the snapshot names none); cols/rows its grid (0 for kinds without
 * one); key its persistent ResourceKey from the organization replica, all
 * zero when unknown.
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
    uint32_t session_id;
    uint16_t cols;
    uint16_t rows;
    uint8_t key[16];
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
 * spawn/attach/detach-terminal and arrangement refresh/mutation calls for this
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
 * has_retain_secs / retain_secs and idempotency_key are trailing-additive at
 * ABI version 2 (ADR-0124, ADR-0126). A host that still passes the geometry-only
 * size is accepted and treated as both absent. has_retain_secs false means
 * absent (today's close-at-exit); true sends retain_secs, including 0 for the
 * server default. Requires RETAIN_ON_EXIT or PHUX_CLIENT_INVALID_STATE, nothing
 * queued. idempotency_key is 16 bytes; all-zero means absent. A nonzero key
 * requires SPAWN_IDEMPOTENCY or PHUX_CLIENT_INVALID_STATE, nothing queued.
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
    bool has_retain_secs;
    uint32_t retain_secs;
    uint8_t idempotency_key[16];
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
    PHUX_OPERATION_DETACH_RESOURCE = 3,
    /* phux_client_queue_kill_if (see "conditional kill" below); additive. */
    PHUX_OPERATION_KILL_IF = 4,
    PHUX_OPERATION_CLOSE_RESOURCE = 5,
    PHUX_OPERATION_CLOSE_RESOURCES = 6,
    /* phux_client_queue_acquire_input / _release_input (see "input lease"
     * below); additive. */
    PHUX_OPERATION_ACQUIRE_INPUT = 7,
    PHUX_OPERATION_RELEASE_INPUT = 8
} PhuxOperationKind;

typedef enum PhuxOperationStatus {
    PHUX_OPERATION_SUCCESS = 1,
    PHUX_OPERATION_REFUSED = 2,
    PHUX_OPERATION_UNKNOWN_OUTCOME = 3
} PhuxOperationStatus;

typedef enum PhuxOperationErrorDomain {
    PHUX_OPERATION_ERROR_NONE = 0,
    PHUX_OPERATION_ERROR_SPAWN = 1,
    PHUX_OPERATION_ERROR_PROTOCOL = 2,
    /* error_code is a PhuxOrgRefusal: an organization command (a close sent
     * as RESOURCE_TERMINATE) was refused. */
    PHUX_OPERATION_ERROR_ORGANIZATION = 3
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


#endif
