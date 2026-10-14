/* Native ABI smoke for the organization section (ABI 3): record layouts,
 * vocabulary values, and the refusals a client without ORGANIZATION gives.
 * Compile and link against the same checkout's FFI; 64-bit native layout. */
#include "phux/client.h"
#include <assert.h>
#include <stddef.h>
#include <string.h>

_Static_assert(sizeof(PhuxSessionInfo) == 40, "PhuxSessionInfo is frozen");
_Static_assert(offsetof(PhuxSessionInfo, window_count) == 32, "frozen window_count offset");
_Static_assert(sizeof(PhuxResourceInfo) == 128, "resource info size");
_Static_assert(offsetof(PhuxResourceInfo, session_id) == 104, "resource session offset");
_Static_assert(offsetof(PhuxResourceInfo, key) == 112, "resource key offset");
_Static_assert(sizeof(PhuxSessionRecord) == 128, "session record size");
_Static_assert(offsetof(PhuxSessionRecord, key) == 16, "session record key offset");
_Static_assert(offsetof(PhuxSessionRecord, revision) == 48, "session record revision offset");
_Static_assert(offsetof(PhuxSessionRecord, lifetime) == 64, "session record lifetime offset");
_Static_assert(offsetof(PhuxSessionRecord, name) == 96, "session record name offset");
_Static_assert(offsetof(PhuxSessionRecord, key_text) == 112, "session record key text offset");
_Static_assert(sizeof(PhuxResourceRecord) == 152, "resource record size");
_Static_assert(offsetof(PhuxResourceRecord, activation_id) == 56, "activation offset");
_Static_assert(offsetof(PhuxResourceRecord, live_id) == 88, "live id offset");
_Static_assert(offsetof(PhuxResourceRecord, key_text) == 136, "record key text offset");
_Static_assert(sizeof(PhuxOrganizationInfo) == 88, "organization info size");
_Static_assert(offsetof(PhuxOrganizationInfo, revision) == 56, "organization revision offset");
_Static_assert(sizeof(PhuxOrgResult) == 72, "org result size");
_Static_assert(offsetof(PhuxOrgResult, revision) == 32, "org result revision offset");
_Static_assert(sizeof(PhuxOrgTarget) == 56, "org target size");
_Static_assert(offsetof(PhuxOrgTarget, status) == 32, "org target status offset");
_Static_assert(PHUX_SESSION_LIFETIME_PERSISTENT == 1 && PHUX_SESSION_LIFETIME_EPHEMERAL == 2,
               "lifetimes are stable");
_Static_assert(PHUX_EXEC_RUNNING == 2 && PHUX_EXEC_LOST == 6, "exec states are stable");
_Static_assert(PHUX_ORG_REFUSAL_CONFLICT == 3 && PHUX_ORG_REFUSAL_NOT_EMPTY == 5,
               "refusals are the wire tag plus one");
_Static_assert(PHUX_OPERATION_ERROR_ORGANIZATION == 3, "organization refusal domain");

int main(void) {
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024, .max_history_page_bytes = 1024,
        .max_history_page_rows = 128, .max_history_cache_bytes = 4096,
        .max_history_materialized_rows = 1024, .history_prefetch_rows = 64,
    };
    PhuxClient *client = NULL;
    assert(phux_client_new(&options, &client) == PHUX_CLIENT_OK);

    bool supported = true;
    assert(phux_client_organization_supported(client, &supported) == PHUX_CLIENT_OK);
    assert(!supported);
    PhuxOrganizationInfo info = {.size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_organization_info(client, &info) == PHUX_CLIENT_OK);
    assert(!info.supported && info.state == 0 && info.record_count == 0);
    PhuxOrganizationInfo stale = {.size = sizeof(stale), .version = 2};
    assert(phux_client_organization_info(client, &stale) == PHUX_CLIENT_INVALID_ARGUMENT);
    assert(phux_client_organization_refresh(client) == PHUX_CLIENT_INVALID_STATE);

    assert(phux_client_record_count(client) == 0);
    assert(phux_client_record_count(NULL) == 0);
    PhuxResourceRecord record = {.size = sizeof(record), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_record_get(client, 0, &record) == PHUX_CLIENT_INVALID_ARGUMENT);
    PhuxSessionRecord session = {.size = sizeof(session), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_session_record_get(client, 0, &session) == PHUX_CLIENT_INVALID_ARGUMENT);

    uint8_t key[16];
    memset(key, 7, sizeof(key));
    assert(phux_client_org_move(client, 1, key, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_move_many(client, 1, key, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_terminate(client, 1, key, false) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_remove(client, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_relaunch(client, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_delete_session(client, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_set_lifetime(client, 1, key, PHUX_SESSION_LIFETIME_PERSISTENT) ==
           PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_inbox_resolve(client, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_intake_resolve(client, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(PHUX_ORG_INTAKE_RESOLVE == PHUX_ORG_INBOX_RESOLVE);
    assert(PHUX_SESSION_RECORD_INTAKE == PHUX_SESSION_RECORD_INBOX);
    assert(phux_client_kill_session(client, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_org_undo(client, 2, 1) == PHUX_CLIENT_INVALID_ARGUMENT);
    assert(phux_client_org_move(client, 1, NULL, key) == PHUX_CLIENT_INVALID_ARGUMENT);
    uint64_t next = 0;
    assert(phux_client_expire_operations(client, &next) == PHUX_CLIENT_OK);
    assert(next == UINT64_MAX);
    assert(phux_client_org_result_count(client) == 0);
    PhuxOrgResult result = {.size = sizeof(result), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_org_result_get(client, 0, &result) == PHUX_CLIENT_INVALID_ARGUMENT);
    assert(phux_client_org_result_release(client, 1) == PHUX_CLIENT_INVALID_ARGUMENT);

    assert(phux_client_create_session(client, 1, (PhuxBytes){(const uint8_t *)"x", 1},
                                      PHUX_SESSION_LIFETIME_PERSISTENT) == PHUX_CLIENT_INVALID_STATE);
    phux_client_free(client);
    return 0;
}
