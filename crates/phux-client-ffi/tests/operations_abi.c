/* Native ABI smoke: compile/link against this checkout's phux-client-ffi. */
#include "phux/client.h"
#include <assert.h>

int main(void) {
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024, .max_history_page_bytes = 1024,
        .max_history_page_rows = 128, .max_history_cache_bytes = 4096,
        .max_history_materialized_rows = 1024, .history_prefetch_rows = 64,
    };
    PhuxClient *client = NULL;
    assert(phux_client_new(&options, &client) == PHUX_CLIENT_OK);
    assert(client != NULL);
    PhuxSpawnOptions spawn = {
        .size = sizeof(spawn), .version = PHUX_CLIENT_ABI_VERSION,
        .request_id = 1, .cols = 80, .rows = 24,
    };
    PhuxAttachTerminalOptions attach = {
        .size = sizeof(attach), .version = PHUX_CLIENT_ABI_VERSION,
        .request_id = 2, .terminal_id = {.kind = 0, .id = 2},
    };
    assert(phux_client_queue_spawn(client, &spawn) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_queue_attach_terminal(client, &attach) == PHUX_CLIENT_INVALID_STATE);
    PhuxOperationResult result = {
        .size = sizeof(result), .version = PHUX_CLIENT_ABI_VERSION,
        .request_id = 99, .kind = PHUX_OPERATION_SPAWN,
    };
    assert(phux_client_operation_count(client) == 0);
    assert(phux_client_operation_get(client, 0, &result) == PHUX_CLIENT_NO_VALUE);
    assert(result.size == sizeof(result));
    assert(result.version == PHUX_CLIENT_ABI_VERSION);
    assert(result.request_id == 0 && result.kind == 0);
    assert(result.terminal_id.id == 0 && result.message.len == 0);
    PhuxBytes identity = {0};
    assert(phux_client_server_id(client, &identity) == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_operation_clear(client) == PHUX_CLIENT_OK);
    assert(phux_client_disconnect(client) == PHUX_CLIENT_OK);
    assert(phux_client_state(client) == PHUX_CLIENT_STATE_DETACHED);
    assert(phux_client_disconnect(client) == PHUX_CLIENT_OK);
    phux_client_free(client);
    return 0;
}
