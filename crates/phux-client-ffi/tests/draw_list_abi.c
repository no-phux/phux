/* Draw-list layouts, pinned against the Rust records in src/c/draw_list.rs. */
#include "phux/client.h"
#include <assert.h>
#include <stddef.h>

_Static_assert(sizeof(PhuxDrawOptions) == 24, "draw options size");
_Static_assert(offsetof(PhuxDrawOptions, focused) == 18, "focused offset");
_Static_assert(sizeof(PhuxDrawBackground) == 10, "background size");
_Static_assert(sizeof(PhuxDrawText) == 20, "text size");
_Static_assert(offsetof(PhuxDrawText, utf8_offset) == 12, "utf8_offset offset");
_Static_assert(sizeof(PhuxDrawBlock) == 18, "block size");
_Static_assert(offsetof(PhuxDrawBlock, regions) == 9, "regions offset");
_Static_assert(sizeof(PhuxDrawDecoration) == 12, "decoration size");
_Static_assert(sizeof(PhuxDrawCursor) == 16, "cursor size");
_Static_assert(offsetof(PhuxDrawCursor, style) == 8, "cursor style offset");
_Static_assert(sizeof(PhuxDrawList) == 120, "draw list size");
_Static_assert(offsetof(PhuxDrawList, backgrounds) == 24, "backgrounds offset");
_Static_assert(offsetof(PhuxDrawList, cursor) == 88, "cursor offset");
_Static_assert(offsetof(PhuxDrawList, utf8) == 104, "utf8 offset");

int main(void) {
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024, .max_history_page_bytes = 1024,
        .max_history_page_rows = 128, .max_history_cache_bytes = 4096,
        .max_history_materialized_rows = 1024, .history_prefetch_rows = 64,
    };
    PhuxClient *client = NULL;
    assert(phux_client_new(&options, &client) == PHUX_CLIENT_OK);
    PhuxResourceId terminal = {0};
    terminal.id = 1;
    PhuxDrawList list = {.size = sizeof(list), .version = PHUX_CLIENT_ABI_VERSION};
    /* An unconnected client has no grid to draw. */
    assert(phux_client_terminal_draw_list(client, &terminal, NULL, &list) != PHUX_CLIENT_OK);
    PhuxDrawList short_list = {.size = sizeof(size_t), .version = PHUX_CLIENT_ABI_VERSION};
    assert(phux_client_terminal_draw_list(client, &terminal, NULL, &short_list) ==
           PHUX_CLIENT_INVALID_ARGUMENT);
    phux_client_free(client);
    return 0;
}
