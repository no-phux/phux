#include <phux/client.h>

#include <stddef.h>

_Static_assert(PHUX_CLIENT_ABI_VERSION == 1u, "unexpected ABI version");
_Static_assert(sizeof(PhuxBytes) == 16, "PhuxBytes size");
_Static_assert(sizeof(PhuxTerminalId) == 24, "PhuxTerminalId size");
_Static_assert(sizeof(PhuxClientOptions) == 48, "PhuxClientOptions size");
_Static_assert(sizeof(PhuxClientCallbacks) == 40, "PhuxClientCallbacks size");
_Static_assert(sizeof(PhuxAttachOptions) == 56, "PhuxAttachOptions size");
_Static_assert(sizeof(PhuxClientEffect) == 88, "PhuxClientEffect size");
_Static_assert(sizeof(PhuxDocumentAnchor) == 8, "PhuxDocumentAnchor size");
_Static_assert(sizeof(PhuxDocumentPoint) == 12, "PhuxDocumentPoint size");
_Static_assert(sizeof(PhuxTerminalCell) == 36, "PhuxTerminalCell size");
_Static_assert(sizeof(PhuxTerminalGridView) == 176, "PhuxTerminalGridView size");
_Static_assert(sizeof(PhuxKeyEvent) == 56, "PhuxKeyEvent size");
_Static_assert(sizeof(PhuxMouseEvent) == 40, "PhuxMouseEvent size");
_Static_assert(sizeof(PhuxSearchResult) == 16, "PhuxSearchResult size");
_Static_assert(offsetof(PhuxAttachOptions, attach_id) == 12, "attach_id offset");
_Static_assert(offsetof(PhuxClientEffect, terminal_id) == 16, "effect terminal offset");
_Static_assert(offsetof(PhuxTerminalGridView, cells) == 64, "grid cells offset");
_Static_assert(offsetof(PhuxTerminalGridView, top_anchor) == 168, "grid anchor offset");
_Static_assert(offsetof(PhuxKeyEvent, text) == 32, "key text offset");

static void check_symbols(PhuxClient *client, const PhuxTerminalId *terminal_id) {
    PhuxBytes bytes = {0};
    PhuxTerminalGridView grid = {0};
    bool mouse = false;
    (void)phux_client_state(client);
    (void)phux_client_last_error(client, &bytes);
    (void)phux_client_maintenance(client);
    (void)phux_client_maintenance_pending(client);
    (void)phux_client_terminal_grid(client, terminal_id, &grid);
    (void)phux_client_terminal_mouse_tracking(client, terminal_id, &mouse);
}

int main(void) {
    check_symbols(NULL, NULL);
    return 0;
}
