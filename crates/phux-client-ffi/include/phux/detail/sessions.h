#ifndef PHUX_CLIENT_DETAIL_SESSIONS_H
#define PHUX_CLIENT_DETAIL_SESSIONS_H

/* Internal section of phux/client.h; include the umbrella header instead. */

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

/* ---------------------------------------------------- conditional kill
 *
 * ADR-0109, docs/spec/L1.md sections 3.1 and 5.2.1. Additive to ABI version
 * 2: bind_instance stays a separate function, and PhuxOperationResult is
 * unchanged. All of it
 * requires HELLO_OK to have advertised CONDITIONAL_KILL (0x00200000);
 * otherwise PHUX_CLIENT_INVALID_STATE, nothing queued, no request ID
 * consumed.
 *
 * phux_client_queue_spawn_bound is phux_client_queue_spawn with
 * SPAWN_RESOURCE.bind_instance set: the server binds the new terminal to its
 * instance token and its RESOURCE_SPAWNED carries the 16-byte token.
 * phux_client_operation_instance reads it for the completion at index:
 * *out_bound is false (and out_instance untouched) for every completion that
 * carried none, including every unbound spawn.
 *
 * phux_client_queue_kill_if sends KILL_RESOURCE_IF for terminal_id with the
 * precondition a late kill of one's own spawn needs: the server's instance
 * token still equals `instance`, and no connection but the spawning one has
 * attached or used the terminal (UNATTACHED_SINCE_SPAWN). The server checks
 * and kills atomically; a failed condition answers PRECONDITION_FAILED and
 * kills nothing. It needs only a negotiated client, attached or not, and is
 * correlated like the other operations: a completion of kind
 * PHUX_OPERATION_KILL_IF, success or refused (error_domain protocol, code
 * the wire ErrorCode). There is deliberately no unconditional kill here. */
PhuxClientResult phux_client_queue_spawn_bound(PhuxClient *client, const PhuxSpawnOptions *options);
PhuxClientResult phux_client_operation_instance(const PhuxClient *client, size_t index, uint8_t out_instance[16], bool *out_bound);
PhuxClientResult phux_client_queue_kill_if(PhuxClient *client, uint32_t request_id, const PhuxResourceId *terminal_id, const uint8_t instance[16]);

/* Intentional termination of a current, live terminal owned by this ATTACH.
 * Against a server that advertised ORGANIZATION whose records this client
 * holds (PhuxOrganizationInfo.state LIVE), a local terminal is terminated by
 * RESOURCE_TERMINATE of its current activation: the reply is ACCEPTED, not Ok,
 * and a refusal reports error_domain PHUX_OPERATION_ERROR_ORGANIZATION with a
 * PhuxOrgRefusal code (STALE when the activation is no longer current).
 * Otherwise it is KILL_RESOURCE, or instance-only KILL_RESOURCE_IF when a bound
 * spawn supplied evidence. Satellite IDs always take the kill path and REQUIRE
 * bound-spawn instance evidence: otherwise
 * INVALID_STATE with last_error is returned before enqueue/ID consumption.
 * A hub connection alone cannot fence a satellite restart. The satellite must
 * evaluate the unchanged instance precondition or the relay returns refusal.
 * This does not weaken abandoned-spawn cleanup above. Result kind 5
 * reports correlated success/refusal/unknown outcome. SUCCESS is published only
 * after this exact Client has processed command Ok (ACCEPTED for a terminate)
 * AND RESOURCE_CLOSED for the captured terminal, in either order. Ok alone
 * only acknowledges cancellation;
 * it never produces a success receipt or fabricates replica retirement.
 * Disconnect while either proof is missing reports UNKNOWN_OUTCOME, including
 * after Ok. Clearing completed results leaves pending close evidence intact.
 * The exact client is the connection fence. It cannot reconnect; disconnect
 * discards queued output. Captured actions and outgoing bytes MUST NOT be
 * replayed on a replacement connection or retargeted by numeric terminal ID. */
PhuxClientResult phux_client_queue_close_resource(PhuxClient *client, uint32_t request_id, const PhuxResourceId *terminal_id);

/* Atomic LOCAL batch on the owning coordinator via KILL_RESOURCES. All 1..256 IDs
 * must be distinct current live owners of this attachment; ANY satellite ID
 * (even bound) returns INVALID_STATE/last_error for the entire batch, because
 * satellite batch forwarding provides no atomic correlated teardown. Never
 * fall back to closing only the local subset. Validation failure
 * queues nothing and consumes no request ID. Same connection fence/no replay
 * contract as close_resource. Result kind 6 has no single returned terminal;
 * SUCCESS requires Ok AND individual RESOURCE_CLOSED for EVERY captured ID,
 * regardless of arrival order. Partial closure leaves the operation pending;
 * disconnect then produces UNKNOWN_OUTCOME, not success. The wire
 * batch has no instance precondition; never move it to another connection. */
PhuxClientResult phux_client_queue_close_resources(PhuxClient *client, uint32_t request_id, const PhuxResourceId *terminal_ids, size_t count);

/* Same validations, fence, and kind-6 completion as queue_close_resources.
 * Against an ORGANIZATION server whose records this client holds for every
 * ID, each terminal is terminated on its own (RESOURCE_TERMINATE: the first
 * on request_id, the rest on bridge-internal ids); SUCCESS needs every
 * ACCEPTED and every RESOURCE_CLOSED, and the first refusal settles the
 * request REFUSED (domain ORGANIZATION). It is not atomic; a session's
 * lifetime, not this call, decides whether an emptied session stays.
 * Otherwise the wire command is CLOSE_TAB_RESOURCES (L1 §5.2.2): a
 * full-coverage batch leaves a persistent session empty instead of reaping
 * it. Ending a whole session is phux_client_kill_session (organization
 * section) against a SESSION_KILL server, else queue_close_resources. */
PhuxClientResult phux_client_queue_close_tab_resources(PhuxClient *client, uint32_t request_id, const PhuxResourceId *terminal_ids, size_t count);
/** *out_supported: HELLO_OK advertised CLOSE_TAB_RESOURCES. */
PhuxClientResult phux_client_close_tab_resources_supported(const PhuxClient *client, bool *out_supported);
/** *out_supported: HELLO_OK advertised CONDITIONAL_KILL. */
PhuxClientResult phux_client_conditional_kill_supported(const PhuxClient *client, bool *out_supported);

/* ------------------------------------------------ persistent sessions
 *
 * A session's lifetime decides whether it survives its last member:
 * PERSISTENT does, EPHEMERAL is deleted with it (the legacy keep-empty mark
 * is lifetime PERSISTENT). PhuxSessionRecord.lifetime carries it; these
 * flags remain for hosts that read only PhuxSessionInfo.
 *
 * phux_client_session_flags reads the flags of the session
 * phux_client_session_get reads at the same index: KEEP_EMPTY for a
 * persistent session, and EMPTY as well when it has no members (from a
 * server that reports lifetimes) or no windows (from an older one that
 * advertised KEEP_EMPTY_SESSIONS, 0x00020000). From a server that reports
 * neither, every session reads 0, so a window_count of 0 there is not a
 * claim that a session is empty. INVALID_ARGUMENT for an index at or past
 * phux_client_session_count or a null output. */
#define PHUX_SESSION_FLAG_KEEP_EMPTY 0x1u
#define PHUX_SESSION_FLAG_EMPTY 0x2u

PhuxClientResult phux_client_session_flags(const PhuxClient *client, size_t index, uint32_t *out_flags);
/** *out_supported: HELLO_OK advertised KEEP_EMPTY_SESSIONS or ORGANIZATION. */
PhuxClientResult phux_client_keep_empty_supported(const PhuxClient *client, bool *out_supported);

/* ------------------------------------------------------- session rename
 *
 * Rename a session on the connected server (phux.session.name/v1,
 * docs/spec/L3.md section 3.1). Additive to ABI version 2; no existing
 * declaration changes. Works on a negotiated client whether or not it has
 * attached: a rename writes metadata and never attaches or sizes anything.
 *
 * The call judges the request against the latest session list first. An
 * unknown current name, or a new name another session already holds, settles
 * REFUSED with a reason and queues nothing; a rename to the same name
 * settles RENAMED and queues nothing. Otherwise it queues, in order, a
 * SUBSCRIBE_METADATA of the key (once per client), the SET_METADATA
 * (current\0new, correlated by request_id), and a bridge-internal GET_STATE
 * as an ordering barrier, and settles PENDING. The server's METADATA_CHANGED
 * settles it RENAMED; the barrier's answer settles it either way even without
 * one; a correlated ERROR settles it REFUSED with the server's message.
 * Disconnecting while PENDING yields UNKNOWN_OUTCOME.
 *
 * Every METADATA_CHANGED of the key renames the session list in place,
 * whoever asked, and bumps sessions_revision: reread phux_client_session_get
 * when it moves. request_id shares the strictly increasing host request
 * space and is consumed by every accepted call, a refused one included. Both
 * names are UTF-8, non-blank, at most 256 bytes, without control characters
 * (INVALID_ARGUMENT otherwise). A new name the session selector grammar
 * cannot address (leading @ # % ^, a ':', '/@', or a bare '.' or '=')
 * settles REFUSED with the reason.
 * One rename may be PENDING (INVALID_STATE otherwise).
 *
 * Against a server that advertised ORGANIZATION, a rename that must be sent
 * is instead one typed SESSION_RENAME (the last writer wins) through the
 * runtime's operation journal, which holds it until the records' seed names
 * the server's authority (request_id is consumed but not put on the wire):
 * the commit settles it RENAMED, a refusal REFUSED with its sentence, an
 * unresolvable outcome UNKNOWN_OUTCOME. A rename from another client arrives
 * through the organization records and moves sessions_revision the same way.
 * Such a server is never sent the metadata key. Only when no seed can land
 * (it was refused and no authority is known) is such a rename INVALID_STATE
 * without consuming request_id; call phux_client_organization_refresh. A
 * session whose listing carries no session key, or a submission the journal
 * refuses, returns INVALID_STATE and settles REFUSED with the reason. */
typedef enum PhuxSessionRenameStatus {
    PHUX_SESSION_RENAME_NONE = 0,
    PHUX_SESSION_RENAME_PENDING = 1,
    PHUX_SESSION_RENAME_RENAMED = 2,
    PHUX_SESSION_RENAME_REFUSED = 3,
    PHUX_SESSION_RENAME_UNKNOWN_OUTCOME = 4
} PhuxSessionRenameStatus;

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * session_id is the renamed session's (0 when refused as unknown); message
 * says why a rename was refused or unknown and must not be parsed. Spans are
 * borrowed until the next mutable client call. */
typedef struct PhuxSessionRenameInfo {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    uint32_t status;
    uint32_t session_id;
    uint64_t sessions_revision;
    PhuxBytes message;
} PhuxSessionRenameInfo;

PhuxClientResult phux_client_rename_session(PhuxClient *client, uint32_t request_id, PhuxBytes current, PhuxBytes new_name);
PhuxClientResult phux_client_session_rename_info(const PhuxClient *client, PhuxSessionRenameInfo *out_info);

/* Create a session on this exact negotiated connection. lifetime is a
 * PhuxSessionLifetime (PERSISTENT or EPHEMERAL; anything else is
 * INVALID_ARGUMENT). No ATTACH is sent and the current session remains
 * selected. request_id shares the monotonic operation namespace. Names are
 * UTF-8, non-blank, at most 256 bytes, without control characters, and
 * addressable by a session selector (no leading @ # % ^, no ':' or '/@', not
 * a bare '.' or '='); INVALID_ARGUMENT otherwise.
 *
 * Against a server that advertised ORGANIZATION whose records this client
 * holds: one typed SESSION_CREATE through the runtime's operation journal. A
 * PERSISTENT session without a directory is created empty; an EPHEMERAL one
 * always launches its first shell (it is never empty). On commit an ordered
 * GET_STATE binds the created key to its live session_id. The journal holds
 * the command until the records' seed names the server's authority; only when
 * no seed can land (it was refused and no authority is known) is the call
 * INVALID_STATE without consuming request_id, until
 * phux_client_organization_refresh. Such a server is never sent the metadata
 * operation below.
 *
 * Otherwise: the CLI's nonce-correlated keep-empty metadata operation. Only
 * PERSISTENT is possible there, and a server without KEEP_EMPTY_SESSIONS is
 * refused before any write (INVALID_STATE). */
PhuxClientResult phux_client_create_session(PhuxClient *client, uint32_t request_id, PhuxBytes name, uint32_t lifetime);

/* Same as phux_client_create_session. A non-empty directory is the session's
 * directory and launches its first shell there (`cwd` in the legacy create
 * document). An empty directory is the same session as
 * phux_client_create_session. */
PhuxClientResult phux_client_create_session_in(PhuxClient *client, uint32_t request_id, PhuxBytes name, PhuxBytes directory, uint32_t lifetime);

typedef struct PhuxSessionCreateInfo {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    uint32_t status; /* 1 pending, 2 created, 3 refused, 4 unknown outcome */
    uint32_t session_id; /* nonzero only after an authoritative state reply */
    PhuxBytes message; /* borrowed until the next mutable client call */
} PhuxSessionCreateInfo;

/* Each request retains its own result under concurrency. Initialize size and
 * version (PHUX_CLIENT_ABI_VERSION) before querying. Copy the message before
 * releasing a result. Release abandons interest; pending writes still execute
 * and their reply correlations are retained until drained or disconnected.
 * Release only on the original Client incarnation: reconnects may reuse IDs. */
PhuxClientResult phux_client_session_create_info(const PhuxClient *client, uint32_t request_id, PhuxSessionCreateInfo *out_info);
PhuxClientResult phux_client_session_create_release(PhuxClient *client, uint32_t request_id);

/* Follow renames any client makes, from the start of the connection.
 * Additive to ABI version 2. Queues the SUBSCRIBE_METADATA of
 * phux.session.name/v1 now if HELLO_OK has been applied, else as the first
 * frame queued once it is; a later rename does not subscribe again. The
 * subscription is read-only: it never attaches or sizes anything, and the
 * server delivers METADATA_CHANGED to a client that never attached, so a
 * listing client may follow. Each broadcast renames the session list in
 * place and bumps sessions_revision, as above. Against a server that
 * advertised ORGANIZATION the key is never subscribed: renames arrive as
 * SessionRenamed on the server-wide event stream, which an attach subscribes;
 * a client that has not attached queues SUBSCRIBE_EVENTS (from the records'
 * journal cut) once its organization records are LIVE. Each one renames the
 * session list and moves sessions_revision the same way. Idempotent;
 * INVALID_STATE once DETACHED. */
PhuxClientResult phux_client_follow_session_names(PhuxClient *client);


#endif
