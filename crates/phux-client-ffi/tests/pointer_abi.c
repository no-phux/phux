/* Compile-only consumer: the additive gesture API must remain usable from C. */
#include "phux/client.h"
#include <stddef.h>

_Static_assert(sizeof(((PhuxSelectionGestureEvent *)0)->size) == sizeof(size_t),
               "gesture sizing follows the native ABI");
_Static_assert(offsetof(PhuxSelectionGestureResult, start) == sizeof(uint64_t),
               "gesture results begin with an opaque stream handle");

PhuxClientResult pointer_abi(PhuxClient *client, const PhuxResourceId *id) {
    PhuxSelectionGestureEvent event = {
        .size = sizeof(event), .version = PHUX_CLIENT_ABI_VERSION, .phase = 0, .clicks = 2,
        .columns = 80, .cell_width = 10, .screen_height = 480,
    };
    PhuxSelectionGestureResult result = {0};
    PhuxClientResult status = phux_client_selection_gesture(client, id, &event, &result);
    if (status != PHUX_CLIENT_OK) return status;
    if (result.start.opaque_id) phux_client_anchor_release(client, id, result.start);
    if (result.end.opaque_id) phux_client_anchor_release(client, id, result.end);
    event.phase = 2;
    event.handle = result.handle;
    return phux_client_selection_gesture(client, id, &event, &result);
}
