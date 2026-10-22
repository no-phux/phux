#ifndef PHUX_CLIENT_DETAIL_EFFECTS_H
#define PHUX_CLIENT_DETAIL_EFFECTS_H

/* Internal section of phux/client.h; include the umbrella header instead. */

/**
 * PHUX_CLIENT_EFFECT_AGENT_RECORDS carries AgentEventsJsonlV1 records from an
 * AgentSession resource (PhuxResourceInfo.kind == PHUX_RESOURCE_AGENT_SESSION).
 * terminal_id is the resource id, stream_id/bootstrap_id its committed
 * generation, detail a PhuxClientAgentRecordsKind, and bytes zero
 * or more complete records, one JSON object per line
 * ({"seq","ts_ms","type","data"}; key order is not significant). The kernel
 * validated every record before it reached the bridge; a stream whose payload
 * fails validation retires its generation with a STATUS RESYNC_REQUIRED effect
 * instead. RETAINED replaces the whole retained backlog, even when empty, at
 * BOOTSTRAP_READY; LIVE appends an incremental batch, from a live output frame
 * or a retained resume suffix at BOOTSTRAP_READY. An empty successful resume
 * need not emit an effect. seq is the newest record's server-assigned sequence,
 * or zero for an empty replacement. CLOSED has empty bytes and retires the
 * resource from the catalog. Hosts must tolerate effect kinds they
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

/**
 * PHUX_CLIENT_STATUS_CWD/_COMMAND_STARTED/_COMMAND_FINISHED/_EXITED carry the
 * subscribed terminal-process facts of PHA-406/PHA-284: the bridge
 * subscribes with SUBSCRIBE_EVENTS after every ATTACH_READY (a repeat
 * subscribe is a documented wire no-op), and folds the resulting cwd_changed
 * / command_started / command_finished / terminal_control{Exited} events,
 * plus a plain RESOURCE_CLOSED teardown, into these effects. A host that
 * called phux_client_subscribe_events with after_seq before ATTACH_READY
 * sends that journal cursor on the automatic subscribe (ADR-0123). They are
 * additive PhuxClientStatusKind members: a host that does not recognise one
 * must skip it like any other effect kind it does not handle, and adding
 * them does not bump PHUX_CLIENT_ABI_VERSION (see its definition above).
 * Unlike every other effect kind that uses them, stream_id and bootstrap_id
 * on CWD/COMMAND_STARTED/COMMAND_FINISHED/EXITED are not a replica
 * generation: CWD and COMMAND_STARTED leave both at their zero default, and
 * COMMAND_FINISHED/EXITED repurpose them as a presence flag and payload bit
 * pattern (below). Do not key a per-generation cache (render pool, history
 * cache, ...) on them for these four kinds the way a caller keys one on
 * DAMAGE/JOB/AGENT_RECORDS' stream_id/bootstrap_id elsewhere in this API.
 *
 * CWD: bytes is the new working directory (absolute, lossy UTF-8).
 *
 * COMMAND_STARTED: no payload.
 *
 * COMMAND_FINISHED and EXITED share one Option<int32_t> encoding for their
 * numeric payload, because PhuxClientEffect has no dedicated "has a value"
 * field: stream_id is 1 when the value is present and 0 when it is absent
 * (both fields' shared default), and bootstrap_id then holds the value's
 * uint32_t bit pattern, recovered as (int32_t)(uint32_t)bootstrap_id.
 * COMMAND_FINISHED's value is the shell's OSC-133 D exit code.
 *
 * EXITED's value is the process exit code; status_code is a
 * PhuxClientCloseReason wire value (below); first_row carries the
 * terminating signal number when one is known and nonzero, or 0 when the
 * process exited without one or the cause is unknown.
 *
 * INPUT_HOLDER (ADR-0033) is the server's terminal_control broadcast: who
 * holds the terminal's exclusive input lease, restated on every lease or
 * lifecycle change. Only the holder's keys reach the PTY; everyone else's
 * are acked and dropped, so a host renders this rather than letting a
 * terminal look dead. stream_id is 1 when a holder exists and 0 while the
 * wheel is free; bootstrap_id is then the holder's server-side client id;
 * first_row is 1 when the holder is this connection; status_code is the
 * ControlAction wire value (0 ACQUIRED, 1 SEIZED, 2 RELEASED, 9 EXPIRED;
 * lifecycle actions restate the holder unchanged). Taking over is a
 * deliberate phux_client_queue_acquire_input; focus, a second attach, or a
 * reconnect never implies it (docs/protocol.md#attach-and-output). Additive like every
 * member here.
 */
typedef enum PhuxClientStatusKind {
    PHUX_CLIENT_STATUS_BELL = 1,
    PHUX_CLIENT_STATUS_TITLE = 2,
    PHUX_CLIENT_STATUS_RESYNC_REQUIRED = 3,
    PHUX_CLIENT_STATUS_SERVER_ERROR = 4,
    PHUX_CLIENT_STATUS_DETACHED = 5,
    PHUX_CLIENT_STATUS_HISTORY = 6,
    PHUX_CLIENT_STATUS_HISTORY_UNAVAILABLE = 7,
    PHUX_CLIENT_STATUS_CWD = 8,
    PHUX_CLIENT_STATUS_COMMAND_STARTED = 9,
    PHUX_CLIENT_STATUS_COMMAND_FINISHED = 10,
    PHUX_CLIENT_STATUS_EXITED = 11,
    PHUX_CLIENT_STATUS_INPUT_HOLDER = 12
} PhuxClientStatusKind;
/**
 * status_code on a PHUX_CLIENT_STATUS_EXITED effect: the resource's
 * CloseReason wire value (`docs/protocol.md`, RESOURCE_CLOSED), named to
 * match `phux_protocol::wire::frame::CloseReason` exactly. UNKNOWN is also
 * what an absent reason field decodes as — an older peer, or a cause this
 * build does not recognise — so do not treat it as EXITED: a resource can
 * close for a real, stated reason this build has no name for yet.
 * Additive like every enum here: it does not bump PHUX_CLIENT_ABI_VERSION.
 */
typedef enum PhuxClientCloseReason {
    PHUX_CLIENT_CLOSE_EXITED = 0,
    PHUX_CLIENT_CLOSE_KILLED = 1,
    PHUX_CLIENT_CLOSE_PARENT_CLOSED = 2,
    PHUX_CLIENT_CLOSE_SERVER_SHUTDOWN = 3,
    PHUX_CLIENT_CLOSE_UNKNOWN = 255
} PhuxClientCloseReason;
/**
 * status_code on a PHUX_CLIENT_STATUS_DETACHED effect: the DETACHED frame's
 * DetachReason wire value (docs/protocol.md), or PHUX_CLIENT_DETACH_REASON_UNSTATED
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
    PHUX_CLIENT_DETACH_AUTHENTICATION_FAILED = 5,
    PHUX_CLIENT_DETACH_AUTHORIZATION_REVOKED = 6,
    PHUX_CLIENT_DETACH_AUTHORIZATION_EXPIRED = 7,
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
 * HISTORY_UNAVAILABLE, PhuxClientDetachReason for DETACHED,
 * PhuxClientCloseReason for EXITED, the ControlAction wire value for
 * INPUT_HOLDER, and zero otherwise (including CWD, COMMAND_STARTED, and
 * COMMAND_FINISHED, whose own payload is documented on PhuxClientStatusKind
 * above).
 */

/** Borrowed effect. bytes contains title/error detail when defined by kind. The replica never answers terminal queries (the server's canonical terminal is the sole answerer), so no PTY reply appears here or in the outgoing queue. */
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


#endif
