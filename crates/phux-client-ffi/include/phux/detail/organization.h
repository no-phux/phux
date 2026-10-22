#ifndef PHUX_CLIENT_DETAIL_ORGANIZATION_H
#define PHUX_CLIENT_DETAIL_ORGANIZATION_H

/* Internal section of phux/client.h; include the umbrella header instead. */

/* ---------------------------------------------------------- organization
 *
 * Sessions, resources, and Intake as the server's organization records
 * them (docs/organization.md), when HELLO_OK advertised
 * ORGANIZATION (features_ext 0x10). New in ABI version 3.
 *
 * On every connection that advertises it, the client reads the records
 * (paged GET_ORGANIZATION) and then follows them through the event journal
 * it subscribes to after ATTACH_READY; PhuxOrganizationInfo.state reports
 * where that is, and revision moves whenever the records changed under this
 * client. A standby client that never attaches sees the records as of its
 * last phux_client_organization_refresh.
 *
 * Keys are 16 raw bytes; all zero means absent (no key is all zero). Their
 * text form (key_text) is lowercase base32, accepted in full and shown as
 * its shortest unique prefix of at least 6 characters.
 *
 * Every operation below takes a host request_id from the strictly
 * increasing host request space and refuses with INVALID_STATE, nothing
 * queued and no request ID consumed, unless ORGANIZATION was advertised and
 * the records are LIVE, or when the records do not hold a named record. It
 * carries a fresh operation id; after a reconnect inside the server's retry
 * horizon the runtime resends it with the same id (connected lane), and past
 * it the result settles UNKNOWN: reconcile from the records before acting
 * again. Results are retained per request until released, together with
 * pending ones bounded by PHUX_CLIENT_MAX_OPERATIONS. Each result names its
 * targets: the resources it addressed, or for a session operation the
 * session; a kill names the session first, then every member in membership
 * order, so a partial outcome is never reported as one atomic result. */
typedef enum PhuxSessionLifetime {
    PHUX_SESSION_LIFETIME_UNKNOWN = 0,   /* a server that reports none */
    PHUX_SESSION_LIFETIME_PERSISTENT = 1,
    PHUX_SESSION_LIFETIME_EPHEMERAL = 2
} PhuxSessionLifetime;

/* An activation's execution state; NONE when the record never ran. */
typedef enum PhuxExecState {
    PHUX_EXEC_NONE = 0,
    PHUX_EXEC_STARTING = 1,
    PHUX_EXEC_RUNNING = 2,
    PHUX_EXEC_STOPPING = 3,
    PHUX_EXEC_COMPLETED = 4,
    PHUX_EXEC_FAILED = 5,
    PHUX_EXEC_LOST = 6,      /* may still run; remove and relaunch refuse */
    PHUX_EXEC_ABANDONED = 7
} PhuxExecState;

/* How an activation ended: EXITED carries the exit status and SIGNALED the
 * signal in outcome_code; UNOBSERVED ended while no server ran. */
typedef enum PhuxOutcomeKind {
    PHUX_OUTCOME_NONE = 0,
    PHUX_OUTCOME_EXITED = 1,
    PHUX_OUTCOME_SIGNALED = 2,
    PHUX_OUTCOME_UNOBSERVED = 3,
    PHUX_OUTCOME_OTHER = 4
} PhuxOutcomeKind;

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * state: 0 empty or unsupported, 1 reading, 2 LIVE, 3 stale (a journal gap;
 * a re-read is in flight). authority and inbox are the authority key and its
 * designated Intake session's key (legacy field name). message is the last reason the records
 * could not be read, empty otherwise; borrowed until the next mutable call. */
typedef struct PhuxOrganizationInfo {
    size_t size;
    uint32_t version;
    bool supported;
    uint32_t state;
    uint8_t authority[16];
    uint8_t inbox[16];
    uint64_t revision;
    uint32_t record_count;
    PhuxBytes message;
} PhuxOrganizationInfo;

PhuxClientResult phux_client_organization_supported(const PhuxClient *client, bool *out_supported);
PhuxClientResult phux_client_organization_info(const PhuxClient *client, PhuxOrganizationInfo *out_info);
/** Read the records again from the first page. INVALID_STATE without
 * ORGANIZATION or before HELLO_OK. */
PhuxClientResult phux_client_organization_refresh(PhuxClient *client);

/* PhuxSessionRecord.flags */
#define PHUX_SESSION_RECORD_FOCUSED 0x1u
#define PHUX_SESSION_RECORD_AWAITING_ORGANIZATION 0x2u /* imported work in Intake */
#define PHUX_SESSION_RECORD_INTAKE 0x4u                /* the designated Intake session */
#define PHUX_SESSION_RECORD_INBOX PHUX_SESSION_RECORD_INTAKE /* legacy ABI spelling */
#define PHUX_SESSION_RECORD_ORGANIZED 0x8u             /* the records hold this session */
#define PHUX_SESSION_RECORD_EMPTY 0x10u

/** The full record of the session phux_client_session_get reads at the same
 * index. Initialize size/version. key and lifetime come from the snapshot or
 * the records; revision, arrangement_key, and the state counts only with
 * PHUX_SESSION_RECORD_ORGANIZED (zero otherwise). member_count counts every
 * member of every kind (from the snapshot without ORGANIZED). running_count
 * covers STARTING/RUNNING/STOPPING, completed_count COMPLETED/FAILED/
 * ABANDONED, lost_count LOST. Intake a host shows is the INTAKE session
 * plus every AWAITING_ORGANIZATION session, with their members. Spans are
 * borrowed until the next mutable call. */
typedef struct PhuxSessionRecord {
    size_t size;
    uint32_t version;
    uint32_t session_id;
    uint8_t key[16];
    uint8_t arrangement_key[16];
    uint64_t revision;
    int64_t created_at_unix_secs;
    uint32_t lifetime;          /* PhuxSessionLifetime */
    uint32_t flags;             /* PHUX_SESSION_RECORD_* */
    uint32_t member_count;
    uint32_t running_count;
    uint32_t completed_count;
    uint32_t lost_count;
    uint16_t attached_client_count;
    PhuxBytes name;
    PhuxBytes key_text;
} PhuxSessionRecord;

PhuxClientResult phux_client_session_record_get(const PhuxClient *client, size_t index, PhuxSessionRecord *out_record);

/** One resource record, live or not: a completed resource whose exit policy
 * retains it, or a LOST activation, has no live handle (live false) but is
 * still a member of its session. kind is the wire ResourceKind tag. on_exit
 * is 0 remove, 1 retain. session_id is the live handle of its session, 0
 * when that session has none here. Initialize size/version. Spans and
 * live_id.host are borrowed until the next mutable call. */
typedef struct PhuxResourceRecord {
    size_t size;
    uint32_t version;
    uint32_t kind;
    uint8_t key[16];
    uint8_t session_key[16];
    uint64_t revision;
    uint64_t activation_id;     /* 0: never activated */
    uint32_t exec_state;        /* PhuxExecState */
    uint32_t outcome_kind;      /* PhuxOutcomeKind */
    int32_t outcome_code;
    uint32_t on_exit;
    bool live;
    PhuxResourceId live_id;
    uint32_t session_id;
    PhuxBytes name;
    PhuxBytes key_text;
} PhuxResourceRecord;

size_t phux_client_record_count(const PhuxClient *client);
PhuxClientResult phux_client_record_get(const PhuxClient *client, size_t index, PhuxResourceRecord *out_record);

typedef enum PhuxOrgKind {
    PHUX_ORG_MOVE = 1,
    PHUX_ORG_MOVE_MANY = 2,
    PHUX_ORG_TERMINATE = 3,
    PHUX_ORG_REMOVE = 4,
    PHUX_ORG_RELAUNCH = 5,
    PHUX_ORG_DELETE_SESSION = 6,
    PHUX_ORG_SET_LIFETIME = 7,
    PHUX_ORG_INTAKE_RESOLVE = 8,
    PHUX_ORG_INBOX_RESOLVE = PHUX_ORG_INTAKE_RESOLVE, /* legacy ABI spelling */
    PHUX_ORG_UNDO = 9,
    PHUX_ORG_KILL_SESSION = 10
} PhuxOrgKind;

/* COMMITTED: the edit is recorded. ACCEPTED: a terminate was accepted; its
 * end arrives as a record change (exec_state). PARTIAL: some targets took
 * effect and some did not. UNKNOWN: it may have reached the server;
 * reconcile before acting again. NOT_SENT: it never reached a socket. */
typedef enum PhuxOrgStatus {
    PHUX_ORG_PENDING = 1,
    PHUX_ORG_COMMITTED = 2,
    PHUX_ORG_ACCEPTED = 3,
    PHUX_ORG_REFUSED = 4,
    PHUX_ORG_ERROR = 5,
    PHUX_ORG_UNKNOWN = 6,
    PHUX_ORG_NOT_SENT = 7,
    PHUX_ORG_PARTIAL = 8
} PhuxOrgStatus;

/* The wire refusal, plus one. CONFLICT means a record changed meanwhile
 * (conflict_revision names its revision now); NOT_EMPTY that work arrived in
 * a session being deleted; EXECUTION_UNCERTAIN that a LOST activation blocks
 * a remove or relaunch. */
typedef enum PhuxOrgRefusal {
    PHUX_ORG_REFUSAL_NONE = 0,
    PHUX_ORG_REFUSAL_UNAUTHORIZED = 1,
    PHUX_ORG_REFUSAL_NOT_FOUND = 2,
    PHUX_ORG_REFUSAL_CONFLICT = 3,
    PHUX_ORG_REFUSAL_STALE = 4,
    PHUX_ORG_REFUSAL_NOT_EMPTY = 5,
    PHUX_ORG_REFUSAL_UNSUPPORTED = 6,
    PHUX_ORG_REFUSAL_INVALID_ARGUMENT = 7,
    PHUX_ORG_REFUSAL_NAME_TAKEN = 8,
    PHUX_ORG_REFUSAL_EXECUTION_UNCERTAIN = 9,
    PHUX_ORG_REFUSAL_LEASE_HELD = 10,
    PHUX_ORG_REFUSAL_DUPLICATE_OPERATION = 11,
    PHUX_ORG_REFUSAL_STORE_FAILURE = 12,
    PHUX_ORG_REFUSAL_OTHER = 255
} PhuxOrgRefusal;

/* ALREADY_GONE, TIMED_OUT, NO_LIVE_TARGET, and KEPT belonged to the
 * client-side kill workflow and are no longer produced. */
typedef enum PhuxOrgTargetStatus {
    PHUX_ORG_TARGET_PENDING = 1,
    PHUX_ORG_TARGET_DONE = 2,
    PHUX_ORG_TARGET_ALREADY_GONE = 3,
    PHUX_ORG_TARGET_REFUSED = 4,
    PHUX_ORG_TARGET_UNKNOWN = 5,
    PHUX_ORG_TARGET_TIMED_OUT = 6,
    PHUX_ORG_TARGET_NO_LIVE_TARGET = 7,
    PHUX_ORG_TARGET_KEPT = 8,
    PHUX_ORG_TARGET_NOT_SENT = 9
} PhuxOrgTargetStatus;

/** Initialize size/version. kind PhuxOrgKind, status PhuxOrgStatus, refusal
 * the first failing target's PhuxOrgRefusal. revision is the primary
 * record's resulting revision for a single committed edit. undoable: a
 * committed membership or intake edit (MOVE, MOVE_MANY, INTAKE_RESOLVE) whose
 * receipts this client holds. message is a sentence, never parsed; borrowed
 * until the next mutable call. */
typedef struct PhuxOrgResult {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    uint32_t kind;
    uint32_t status;
    uint32_t refusal;
    bool undoable;
    uint64_t revision;
    uint64_t conflict_revision;
    uint32_t target_count;
    PhuxBytes message;
} PhuxOrgResult;

/** Initialize size/version. key is a session key when is_session, else a
 * resource key. status PhuxOrgTargetStatus. */
typedef struct PhuxOrgTarget {
    size_t size;
    uint32_t version;
    uint8_t key[16];
    bool is_session;
    uint32_t status;
    uint32_t refusal;
    PhuxBytes message;
} PhuxOrgTarget;

/* Every key argument points at 16 readable bytes. */
/** Move one resource into another session: membership only, it keeps running. */
PhuxClientResult phux_client_org_move(PhuxClient *client, uint32_t request_id, const uint8_t *resource, const uint8_t *to_session);
/** Move 1..1024 distinct resources (count * 16 contiguous key bytes) into one
 * session: one atomic batch per server-bounded chunk, each chunk's outcome
 * on its own targets. */
PhuxClientResult phux_client_org_move_many(PhuxClient *client, uint32_t request_id, const uint8_t *resources, size_t count, const uint8_t *to_session);
/** End a resource's current activation here (graceful, or forced). ACCEPTED;
 * the end arrives as the record's exec_state. */
PhuxClientResult phux_client_org_terminate(PhuxClient *client, uint32_t request_id, const uint8_t *resource, bool force);
/** Remove an inactive resource record. */
PhuxClientResult phux_client_org_remove(PhuxClient *client, uint32_t request_id, const uint8_t *resource);
/** Start a new activation of an inactive resource from its launch. */
PhuxClientResult phux_client_org_relaunch(PhuxClient *client, uint32_t request_id, const uint8_t *resource);
/** Delete an empty session; REFUSED NOT_EMPTY while it has members. */
PhuxClientResult phux_client_org_delete_session(PhuxClient *client, uint32_t request_id, const uint8_t *session);
/** Make a session PERSISTENT or EPHEMERAL; EPHEMERAL on an empty session deletes it. */
PhuxClientResult phux_client_org_set_lifetime(PhuxClient *client, uint32_t request_id, const uint8_t *session, uint32_t lifetime);
/** Clear an imported session's Intake mark; its members and arrangement stay. */
PhuxClientResult phux_client_org_intake_resolve(PhuxClient *client, uint32_t request_id, const uint8_t *session);
/** Legacy ABI alias of phux_client_org_intake_resolve. */
PhuxClientResult phux_client_org_inbox_resolve(PhuxClient *client, uint32_t request_id, const uint8_t *session);
/** Undo the undoable result of original_request_id by its receipts (one undo
 * per committed chunk). Refused by the server once the receipts expire or
 * the records moved on; never reverses execution. Undo before releasing the
 * original result. */
PhuxClientResult phux_client_org_undo(PhuxClient *client, uint32_t request_id, uint32_t original_request_id);
/** Kill a whole session with one SESSION_KILL: the server ends every member,
 * work that arrived meanwhile included, and deletes the session. COMMITTED
 * when the session went away; one target, the session. INVALID_STATE when
 * the server does not advertise SESSION_KILL. */
PhuxClientResult phux_client_kill_session(PhuxClient *client, uint32_t request_id, const uint8_t *session);
/** Resolve operations past their retry horizon.
 * The connected lane does this itself; an embedded host calls it while a
 * result is PENDING, at the latest after *out_next_ms milliseconds
 * (UINT64_MAX when nothing is due; out_next_ms may be NULL). */
PhuxClientResult phux_client_expire_operations(PhuxClient *client, uint64_t *out_next_ms);

/** Results in request-id order, pending ones included. */
size_t phux_client_org_result_count(const PhuxClient *client);
PhuxClientResult phux_client_org_result_get(const PhuxClient *client, size_t index, PhuxOrgResult *out_result);
PhuxClientResult phux_client_org_target_get(const PhuxClient *client, size_t result_index, size_t target_index, PhuxOrgTarget *out_target);
/** Release a settled result; INVALID_STATE while it is PENDING. */
PhuxClientResult phux_client_org_result_release(PhuxClient *client, uint32_t request_id);


#endif
