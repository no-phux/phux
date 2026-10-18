#ifndef PHUX_CLIENT_DETAIL_ARRANGEMENT_H
#define PHUX_CLIENT_DETAIL_ARRANGEMENT_H

/* Internal section of phux/client.h; include the umbrella header instead. */

/* ------------------------------------------------- shared arrangement
 *
 * The attached session's arrangement: its groups (tabs), each one split tree
 * of terminal leaves. Rust-owned. All output records require initialized
 * size/version. Borrowed names/IDs expire on the next mutable client call.
 * Catalog bounds: 256 sessions, 256 terminals; arrangement: 32 groups, 512
 * nodes, depth 64. Names/title/cwd/host are bounded to 4096 UTF-8 bytes
 * (refusal, no truncation). Group IDs are random 128-bit identities, stable
 * while the group exists.
 *
 * An arrangement is presentation only. It never decides what a session
 * holds: closing a view never ends work, and an empty arrangement means the
 * members are not arranged, not that the session is empty. Nothing is
 * synthesized when a session has no stored arrangement (state UNARRANGED,
 * zero groups); unarranged_count says how many of its terminals no group
 * shows, so a host can offer to arrange them.
 *
 * Backend ORGANIZATION (the server advertised ORGANIZATION, which gives
 * every session a stored arrangement): reads are ARRANGEMENT_GET. A read
 * that starts while the organization replica is still seeding, or before
 * the live replica lists the session, stays pending (no timeout) until the
 * replica names the arrangement or reports itself unavailable; while it is
 * pending every other arrangement transaction is refused as already
 * pending. A read fails at once (status 3, or a refused call) when the
 * replica is neither live nor seeding (call
 * phux_client_organization_refresh) or lists the session without an
 * arrangement. A mutation before the first successful read is refused. A
 * mutation is a compare-and-swap ARRANGEMENT_REPLACE at server_revision,
 * followed by a confirming read. A concurrent writer makes it fail (status
 * REFUSED, the message names the current revision) and the confirming read
 * publishes the winner; redo the edit against it. It is never
 * last-write-wins. Stored entries whose resource has no live handle here
 * are kept on write (unplaced_count). The client follows the stored
 * arrangement through the organization replica and re-reads it on its own
 * (request_id 0) when it moves past server_revision.
 *
 * Backend LEGACY (a server without ORGANIZATION): the
 * phux.tui.layout/v1/<session-id> metadata value, written by whole-value
 * last-write-wins SET and a confirming GET; expected_revision fences only
 * this client's snapshot. Never used against an ORGANIZATION server.
 *
 * Initial attachment exposes catalog only (state UNAVAILABLE) until the
 * document/state replies complete. Present legacy layouts require schema v3
 * and stable IDs; unsupported stored data is refused, not reset. No focus is
 * shared; clients preserve their own selection by stable IDs. */
#define PHUX_ARRANGEMENT_STATE_UNAVAILABLE 0u
#define PHUX_ARRANGEMENT_STATE_UNARRANGED 1u
#define PHUX_ARRANGEMENT_STATE_AUTHORITATIVE 2u
#define PHUX_ARRANGEMENT_STATE_LAST_GOOD_WITH_ERROR 3u
#define PHUX_ARRANGEMENT_BACKEND_NONE 0u
#define PHUX_ARRANGEMENT_BACKEND_LEGACY 1u
#define PHUX_ARRANGEMENT_BACKEND_ORGANIZATION 2u
typedef struct PhuxArrangementInfo {
    size_t size;
    uint32_t version;
    /* This client's publication counter: bumps on any published change. */
    uint64_t revision;
    uint32_t session_id;
    /* PHUX_ARRANGEMENT_STATE_* */
    uint32_t state;
    uint32_t group_count, node_count, terminal_count;
    /* Latest refresh/mutation: 0 idle, 1 pending, 2 confirmed, 3 refused,
     * 4 disconnected/unknown outcome. request_id=0 is an automatic read. */
    uint32_t request_id, status;
    PhuxBytes message;
    /* PHUX_ARRANGEMENT_BACKEND_* the publication came from. */
    uint32_t backend;
    /* The stored arrangement's revision (what a replace expects); 0 for LEGACY. */
    uint64_t server_revision;
    /* The selected session's key and its stored arrangement's key; all zero
     * when unknown or absent. */
    uint8_t session_key[16];
    uint8_t arrangement_key[16];
    /* Terminals of the session no group shows; stored entries without a live
     * handle here. */
    uint32_t unarranged_count;
    uint32_t unplaced_count;
} PhuxArrangementInfo;
typedef struct PhuxArrangementGroup {
    size_t size;
    uint32_t version;
    uint8_t group_id[16];
    PhuxBytes name;
    uint32_t root_node;
} PhuxArrangementGroup;
typedef struct PhuxArrangementNode {
    size_t size;
    uint32_t version;
    /* leaf=1, side-by-side=2, stacked=3; child indices are snapshot-global. */
    uint32_t kind;
    PhuxResourceId terminal_id;
    uint32_t first, second;
    float ratio;
} PhuxArrangementNode;
typedef struct PhuxCatalogTerminal {
    size_t size;
    uint32_t version;
    PhuxResourceId terminal_id;
    /* 0 means unknown ownership (e.g. satellite); never inferred from numeric ID. */
    uint32_t session_id;
    PhuxBytes title, cwd;
} PhuxCatalogTerminal;
typedef struct PhuxArrangementMutation {
    size_t size;
    uint32_t version, request_id;
    uint64_t expected_revision;
    uint32_t session_id;
    /* add group=1, split=2, close view=3, reorder=4, resize=5, rename=6,
     * close group=7. Closing a view or a group never ends work: the
     * terminals keep running and stay members of the session.
     * Kind 3 refuses the last pane, repairs focus, and prunes empty groups. */
    uint32_t kind;
    uint8_t group_id[16];
    /* add: terminal_id seeds a Rust-minted group ID (group_id ignored).
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
} PhuxArrangementMutation;
/* Host request IDs across spawn/subscribe/refresh/mutate/named-projection
 * must strictly increase, 1..0x7fffffff. The bridge reserves the upper half for internal correlation.
 * One refresh OR mutation may be pending. Refresh never changes the actual
 * attached session or allocates emulators. expected_revision is
 * PhuxArrangementInfo.revision: it fences this client's snapshot; the server
 * fences concurrent writers by compare-and-swap on the ORGANIZATION backend
 * only (see above). Adopt only the confirmed current snapshot; never retry
 * unknown spawns. On the LEGACY backend, poll <=1s and before palette display. */
PhuxClientResult phux_client_arrangement_refresh(PhuxClient *client, uint32_t request_id);
PhuxClientResult phux_client_arrangement_mutate(PhuxClient *client, const PhuxArrangementMutation *mutation);
PhuxClientResult phux_client_arrangement_info(const PhuxClient *client, PhuxArrangementInfo *out_info);
PhuxClientResult phux_client_arrangement_group_get(const PhuxClient *client, size_t index, PhuxArrangementGroup *out_group);
PhuxClientResult phux_client_arrangement_node_get(const PhuxClient *client, size_t index, PhuxArrangementNode *out_node);
PhuxClientResult phux_client_catalog_terminal_get(const PhuxClient *client, size_t index, PhuxCatalogTerminal *out_terminal);

/* Named projections (ADR-0129, docs/spec/L3.md section 3.5). Additive to ABI
 * version 2. A projection is not a resource: it is the L3 metadata key
 * <prefix>.layout/v1/<session-id> (canonical decimal session id, nonempty
 * prefix that does not itself contain .layout/v1/). Scope is Group 1. Values
 * are opaque bytes (the section 3.2 CBOR envelope when the writer is a layout
 * consumer). GET/SET/DELETE map to those L3 verbs. SET and DELETE have no
 * success reply, so the bridge queues a confirming GET on a reserved internal
 * id (last-write-wins, not CAS). Requires HELLO_OK layers to include L3;
 * otherwise PHUX_CLIENT_INVALID_STATE, nothing queued, no request ID consumed.
 * Needs a negotiated client, attached or not. One op may be pending. Request
 * IDs share the strictly increasing host space with spawn/subscribe/refresh.
 * A value over 256 KiB is refused before queueing. Disconnecting while
 * PENDING yields UNKNOWN_OUTCOME. Spans from info are borrowed until the next
 * mutable client call. */
typedef enum PhuxProjectionStatus {
    PHUX_PROJECTION_NONE = 0,
    PHUX_PROJECTION_PENDING = 1,
    PHUX_PROJECTION_OK = 2,
    PHUX_PROJECTION_REFUSED = 3,
    PHUX_PROJECTION_UNKNOWN_OUTCOME = 4
} PhuxProjectionStatus;
typedef enum PhuxProjectionOp {
    PHUX_PROJECTION_OP_NONE = 0,
    PHUX_PROJECTION_OP_GET = 1,
    PHUX_PROJECTION_OP_SET = 2,
    PHUX_PROJECTION_OP_DELETE = 3
} PhuxProjectionOp;
/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * present is true when a value is held: GET of an absent key and a confirmed
 * DELETE report OK with present false. REFUSED keeps any confirming-read
 * bytes for diagnosis. */
typedef struct PhuxProjectionInfo {
    size_t size;
    uint32_t version;
    uint32_t request_id;
    uint32_t status;
    uint32_t op;
    uint32_t session_id;
    bool present;
    PhuxBytes key;
    PhuxBytes value;
    PhuxBytes message;
} PhuxProjectionInfo;
PhuxClientResult phux_client_projection_supported(const PhuxClient *client, bool *out_supported);
PhuxClientResult phux_client_projection_get(PhuxClient *client, uint32_t request_id, PhuxBytes key);
PhuxClientResult phux_client_projection_set(PhuxClient *client, uint32_t request_id, PhuxBytes key, PhuxBytes value);
PhuxClientResult phux_client_projection_delete(PhuxClient *client, uint32_t request_id, PhuxBytes key);
PhuxClientResult phux_client_projection_info(const PhuxClient *client, PhuxProjectionInfo *out_info);


#endif
