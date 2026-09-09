/* Native ABI smoke: the additive resource catalog and agent-records effect
 * stay usable from C at PHUX_CLIENT_ABI_VERSION 1. */
#include "phux/client.h"
#include <assert.h>
#include <stddef.h>

_Static_assert(offsetof(PhuxResourceInfo, size) == 0, "resource info is size-versioned");
_Static_assert(PHUX_CLIENT_EFFECT_AGENT_RECORDS == 4, "agent records is the fourth effect kind");
_Static_assert(PHUX_RESOURCE_TERMINAL == 0 && PHUX_RESOURCE_AGENT_SESSION == 1,
               "resource kinds mirror the wire tags");
_Static_assert(PHUX_CLIENT_AGENT_RECORDS_RETAINED == 1 && PHUX_CLIENT_AGENT_RECORDS_LIVE == 2 &&
               PHUX_CLIENT_AGENT_RECORDS_CLOSED == 3,
               "agent records details are stable");

int main(void) {
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024, .max_history_page_bytes = 1024,
        .max_history_page_rows = 128, .max_history_cache_bytes = 4096,
        .max_history_materialized_rows = 1024, .history_prefetch_rows = 64,
    };
    PhuxClient *client = NULL;
    assert(phux_client_new(&options, &client) == PHUX_CLIENT_OK);
    assert(phux_client_resource_count(client) == 0);
    assert(phux_client_resource_count(NULL) == 0);

    PhuxResourceInfo resource = {
        .size = sizeof(resource), .version = PHUX_CLIENT_ABI_VERSION,
        .kind = PHUX_RESOURCE_AGENT_SESSION, .parent = &resource.terminal_id,
    };
    assert(phux_client_resource_get(client, 0, &resource) == PHUX_CLIENT_NO_VALUE);
    assert(resource.size == sizeof(resource));
    assert(resource.version == PHUX_CLIENT_ABI_VERSION);
    assert(resource.kind == PHUX_RESOURCE_TERMINAL);
    assert(resource.parent == NULL);
    assert(resource.provider.len == 0 && resource.native_id.len == 0 && resource.state.len == 0);
    assert(phux_client_resource_get(client, 0, NULL) == PHUX_CLIENT_INVALID_ARGUMENT);
    assert(phux_client_resource_get(NULL, 0, &resource) == PHUX_CLIENT_INVALID_ARGUMENT);

    PhuxResourceInfo stale = {.size = sizeof(stale), .version = PHUX_CLIENT_ABI_VERSION + 1};
    assert(phux_client_resource_get(client, 0, &stale) == PHUX_CLIENT_INVALID_ARGUMENT);
    PhuxResourceInfo small = {.size = sizeof(size_t), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_resource_get(client, 0, &small) == PHUX_CLIENT_INVALID_ARGUMENT);

    PhuxClientEffect effect;
    assert(phux_client_effect_count(client) == 0);
    assert(phux_client_effect_get(client, 0, &effect) == PHUX_CLIENT_NO_VALUE);
    assert(effect.kind != PHUX_CLIENT_EFFECT_AGENT_RECORDS);

    assert(phux_client_disconnect(client) == PHUX_CLIENT_OK);
    assert(phux_client_resource_count(client) == 0);
    phux_client_free(client);
    return 0;
}
