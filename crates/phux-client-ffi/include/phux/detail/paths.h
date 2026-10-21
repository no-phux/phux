#ifndef PHUX_CLIENT_DETAIL_PATHS_H
#define PHUX_CLIENT_DETAIL_PATHS_H

/* Internal section of phux/client.h; include the umbrella header instead. */

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
 * because an older server answers the frame with an uncorrelated
 * Unsupported ERROR and no DIRECTORY_LISTING reply would ever come.
 * request_id shares the strictly increasing host request space with spawn,
 * subscribe and arrangement refresh/mutation. path is UTF-8 without NUL, at
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
 * request_id shares the strictly increasing host request space. path is UTF-8
 * without NUL, at most 4096 bytes: empty or "~" for the serving user's home,
 * "~/rest", or an absolute path. host is the satellite name as a
 * satellite-tagged PhuxResourceId carries it: UTF-8 without NUL, at most 255
 * bytes; empty is the serving host (no host field on the frame). A relayed
 * refusal (unknown or unreachable satellite, a satellite without
 * LIST_DIRECTORY, the relay deadline) arrives as an ordinary REFUSED/OTHER
 * listing whose message names the host. */
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

/* ---------------------------------------------------- host path search
 * PATH_QUERY requires HELLO_OK's extended capability word PATH_QUERY bit 0.
 * Zero request_id means disconnected, unsupported, or the bounded queue is
 * full: no frame was sent, so an older server cannot leave a picker waiting.
 * root/query are UTF-8 without NUL, at most 4096 bytes each; empty host
 * selects the serving host, otherwise host names a satellite (max 255 bytes).
 * recursive=false browses immediate children, true searches below root.
 * Results are absolute host paths, NOT escaped shell text. Selection/insertion
 * is a separate input action requiring the target pane's lease; this API
 * neither pastes text nor changes focus. request_id shares the embedder's
 * strictly increasing explicit request space, consumed only when sent.
 * An answer carries status (complete, warming, truncated) or a typed refusal;
 * disconnect cancels pending requests with error UNANSWERED. Cap: 128 pending
 * plus undrained answers; drain with answers_take to make room. The batch
 * replaces the previous batch; borrowed spans live until next mutable call. */
typedef struct PhuxPathQuery {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    PhuxBytes root;
    PhuxBytes query;
    bool recursive;
    PhuxBytes host;
} PhuxPathQuery;

/* Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION. */
PhuxClientResult phux_client_path_query(PhuxClient *client, const PhuxPathQuery *request, uint32_t *out_request_id);
PhuxClientResult phux_client_path_query_supported(const PhuxClient *client, bool *out_supported);
PhuxClientResult phux_client_path_answers_take(PhuxClient *client, size_t *out_count);

typedef enum PhuxPathKind {
    PHUX_PATH_FILE = 0,
    PHUX_PATH_DIRECTORY = 1,
    PHUX_PATH_SYMLINK = 2
} PhuxPathKind;
typedef enum PhuxPathStatus {
    PHUX_PATH_COMPLETE = 0,
    PHUX_PATH_WARMING = 1,
    PHUX_PATH_TRUNCATED = 2
} PhuxPathStatus;
typedef enum PhuxPathError {
    PHUX_PATH_NOT_FOUND = 0,
    PHUX_PATH_PERMISSION_DENIED = 1,
    PHUX_PATH_NOT_A_DIRECTORY = 2,
    PHUX_PATH_OTHER = 3,
    PHUX_PATH_UNANSWERED = 4
} PhuxPathError;

/* Initialize size/version before reading by index. The header's status is
 * valid only without has_error; error is valid only with has_error. parent is
 * valid only with has_parent. message is display-only, never parse it. */
typedef struct PhuxPathAnswer {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    PhuxBytes root;
    PhuxBytes parent;
    bool has_parent;
    size_t row_count;
    uint32_t status;
    bool has_error;
    uint32_t error;
    PhuxBytes message;
} PhuxPathAnswer;
typedef struct PhuxPathRow {
    size_t size;
    uint32_t version;
    PhuxBytes path;
    uint32_t kind;
} PhuxPathRow;

PhuxClientResult phux_client_path_answer_get(const PhuxClient *client, size_t index, PhuxPathAnswer *out_answer);
PhuxClientResult phux_client_path_row_get(const PhuxClient *client, size_t answer_index, size_t row_index, PhuxPathRow *out_row);


#endif
