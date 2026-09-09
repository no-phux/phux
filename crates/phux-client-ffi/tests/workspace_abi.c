/* Compile and link against the same checkout's FFI; 64-bit native layout. */
#include "phux/client.h"
#include <assert.h>
#include <stddef.h>

_Static_assert(sizeof(PhuxWorkspaceInfo) == 72, "workspace info size");
_Static_assert(offsetof(PhuxWorkspaceInfo, revision) == 16, "revision offset");
_Static_assert(offsetof(PhuxWorkspaceInfo, message) == 56, "message offset");
_Static_assert(sizeof(PhuxWorkspaceWindow) == 56, "window size");
_Static_assert(offsetof(PhuxWorkspaceWindow, window_id) == 12, "window id offset");
_Static_assert(sizeof(PhuxWorkspaceNode) == 56, "node size");
_Static_assert(offsetof(PhuxWorkspaceNode, terminal_id) == 16, "node id offset");
_Static_assert(sizeof(PhuxCatalogTerminal) == 80, "catalog terminal size");
_Static_assert(sizeof(PhuxWorkspaceMutation) == 136, "mutation size");
_Static_assert(offsetof(PhuxWorkspaceMutation, path_bits) == 128, "path offset");

int main(void) {
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024, .max_history_page_bytes = 1024,
        .max_history_page_rows = 128, .max_history_cache_bytes = 4096,
        .max_history_materialized_rows = 1024, .history_prefetch_rows = 64,
    };
    PhuxClient *client = NULL;
    assert(phux_client_new(&options, &client) == PHUX_CLIENT_OK);
    PhuxWorkspaceInfo info = {.size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_workspace_info(client, &info) == PHUX_CLIENT_OK);
    assert(info.state == 0 && info.status == 0 && info.revision == 0);
    assert(phux_client_workspace_info(client, NULL) == PHUX_CLIENT_INVALID_ARGUMENT);
    info.size--;
    assert(phux_client_workspace_info(client, &info) == PHUX_CLIENT_INVALID_ARGUMENT);
    PhuxWorkspaceWindow window = {.size = sizeof(window), .version = PHUX_CLIENT_ABI_VERSION};
    PhuxWorkspaceNode node = {.size = sizeof(node), .version = PHUX_CLIENT_ABI_VERSION};
    PhuxCatalogTerminal terminal = {.size = sizeof(terminal), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_workspace_window_get(client, 0, &window) == PHUX_CLIENT_NO_VALUE);
    assert(phux_client_workspace_node_get(client, 0, &node) == PHUX_CLIENT_NO_VALUE);
    assert(phux_client_catalog_terminal_get(client, 0, &terminal) == PHUX_CLIENT_NO_VALUE);
    assert(phux_client_workspace_refresh(client, 1) == PHUX_CLIENT_INVALID_STATE);
    PhuxWorkspaceMutation mutation = {.size = sizeof(mutation), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_workspace_mutate(client, &mutation) == PHUX_CLIENT_INVALID_STATE);
    phux_client_free(client);
    return 0;
}
