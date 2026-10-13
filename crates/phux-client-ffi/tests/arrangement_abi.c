/* Compile and link against the same checkout's FFI; 64-bit native layout. */
#include "phux/client.h"
#include <assert.h>
#include <stddef.h>

_Static_assert(PHUX_CLIENT_ABI_VERSION == 3u, "the arrangement records are ABI 3");
_Static_assert(sizeof(PhuxArrangementInfo) == 128, "arrangement info size");
_Static_assert(offsetof(PhuxArrangementInfo, revision) == 16, "revision offset");
_Static_assert(offsetof(PhuxArrangementInfo, message) == 56, "message offset");
_Static_assert(offsetof(PhuxArrangementInfo, backend) == 72, "backend offset");
_Static_assert(offsetof(PhuxArrangementInfo, server_revision) == 80, "server revision offset");
_Static_assert(offsetof(PhuxArrangementInfo, session_key) == 88, "session key offset");
_Static_assert(offsetof(PhuxArrangementInfo, arrangement_key) == 104, "arrangement key offset");
_Static_assert(offsetof(PhuxArrangementInfo, unarranged_count) == 120, "unarranged offset");
_Static_assert(offsetof(PhuxArrangementInfo, unplaced_count) == 124, "unplaced offset");
_Static_assert(sizeof(PhuxArrangementGroup) == 56, "group size");
_Static_assert(offsetof(PhuxArrangementGroup, group_id) == 12, "group id offset");
_Static_assert(sizeof(PhuxArrangementNode) == 56, "node size");
_Static_assert(offsetof(PhuxArrangementNode, terminal_id) == 16, "node id offset");
_Static_assert(sizeof(PhuxCatalogTerminal) == 80, "catalog terminal size");
_Static_assert(sizeof(PhuxArrangementMutation) == 136, "mutation size");
_Static_assert(offsetof(PhuxArrangementMutation, group_id) == 32, "mutation group offset");
_Static_assert(offsetof(PhuxArrangementMutation, path_bits) == 128, "path offset");

int main(void) {
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024, .max_history_page_bytes = 1024,
        .max_history_page_rows = 128, .max_history_cache_bytes = 4096,
        .max_history_materialized_rows = 1024, .history_prefetch_rows = 64,
    };
    PhuxClient *client = NULL;
    assert(phux_client_new(&options, &client) == PHUX_CLIENT_OK);
    PhuxArrangementInfo info = {.size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_arrangement_info(client, &info) == PHUX_CLIENT_OK);
    assert(info.state == PHUX_ARRANGEMENT_STATE_UNAVAILABLE && info.status == 0 && info.revision == 0);
    assert(info.backend == PHUX_ARRANGEMENT_BACKEND_NONE && info.server_revision == 0);
    assert(phux_client_arrangement_info(client, NULL) == PHUX_CLIENT_INVALID_ARGUMENT);
    info.size--;
    assert(phux_client_arrangement_info(client, &info) == PHUX_CLIENT_INVALID_ARGUMENT);
    PhuxArrangementGroup group = {.size = sizeof(group), .version = PHUX_CLIENT_ABI_VERSION};
    PhuxArrangementNode node = {.size = sizeof(node), .version = PHUX_CLIENT_ABI_VERSION};
    PhuxCatalogTerminal terminal = {.size = sizeof(terminal), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_arrangement_group_get(client, 0, &group) == PHUX_CLIENT_NO_VALUE);
    assert(phux_client_arrangement_node_get(client, 0, &node) == PHUX_CLIENT_NO_VALUE);
    assert(phux_client_catalog_terminal_get(client, 0, &terminal) == PHUX_CLIENT_NO_VALUE);
    assert(phux_client_arrangement_refresh(client, 1) == PHUX_CLIENT_INVALID_STATE);
    PhuxArrangementMutation mutation = {.size = sizeof(mutation), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_arrangement_mutate(client, &mutation) == PHUX_CLIENT_INVALID_STATE);
    phux_client_free(client);
    return 0;
}
