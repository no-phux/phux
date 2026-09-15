/* Native ABI smoke: compile/link against this checkout's phux-client-ffi. */
#include "phux/client.h"
#include <assert.h>
#include <stddef.h>

_Static_assert(PHUX_CLIENT_ABI_VERSION == 2u, "named projection is additive on ABI 2");
_Static_assert(offsetof(PhuxProjectionInfo, request_id) == sizeof(size_t) + sizeof(uint32_t),
    "request_id follows size/version");
_Static_assert(offsetof(PhuxSpawnOptions, cols) < offsetof(PhuxSpawnOptions, has_retain_secs),
    "retain/idempotency stay trailing");

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
    assert(phux_client_projection_supported(client, &supported) == PHUX_CLIENT_OK);
    assert(supported == false);
    PhuxBytes key = {.data = (const uint8_t *)"myapp.layout/v1/1", .len = 17};
    PhuxBytes value = {.data = (const uint8_t *)"x", .len = 1};
    assert(phux_client_projection_get(client, 1, key) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_projection_set(client, 2, key, value) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_projection_delete(client, 3, key) == PHUX_CLIENT_INVALID_STATE);
    PhuxProjectionInfo info = {
        .size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION,
    };
    assert(phux_client_projection_info(client, &info) == PHUX_CLIENT_OK);
    assert(info.status == PHUX_PROJECTION_NONE);
    assert(info.op == PHUX_PROJECTION_OP_NONE);
    phux_client_free(client);
    return 0;
}
