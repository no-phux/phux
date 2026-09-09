/* Isolated-server regression: pass a private socket and existing session name.
 * Link against this checkout's static FFI. Never points at a default socket.
 * Every spawned workload keeps producing output while detached. */
#include "phux/client.h"
#include <arpa/inet.h>
#include <assert.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static PhuxClient *client;
static int connection;

static PhuxBytes text(const char *value) {
    return (PhuxBytes){(const uint8_t *)value, strlen(value)};
}

static void checked(PhuxClientResult result) {
    if (result == PHUX_CLIENT_OK) return;
    PhuxBytes message = {0};
    phux_client_last_error(client, &message);
    fprintf(stderr, "FFI result %d: %.*s\n", result, (int)message.len, message.data);
    exit(1);
}

static void transfer(void *data, size_t size, int writing) {
    uint8_t *bytes = data;
    while (size != 0) {
        ssize_t count = writing ? write(connection, bytes, size) : read(connection, bytes, size);
        assert(count > 0);
        bytes += count;
        size -= (size_t)count;
    }
}

static void flush(void) {
    for (size_t n = 0; n < phux_client_outgoing_count(client); ++n) {
        PhuxBytes frame;
        checked(phux_client_outgoing_get(client, n, &frame));
        transfer((void *)frame.data, frame.len, 1);
    }
    checked(phux_client_outgoing_clear(client));
}

static int pump(int timeout_ms) {
    flush();
    struct pollfd fd = {connection, POLLIN, 0};
    int ready = poll(&fd, 1, timeout_ms);
    assert(ready >= 0);
    if (ready == 0) return 0;
    uint32_t prefix;
    transfer(&prefix, sizeof(prefix), 0);
    size_t len = ntohl(prefix);
    assert(len <= 16 * 1024 * 1024);
    uint8_t *frame = malloc(len + 4);
    assert(frame != NULL);
    memcpy(frame, &prefix, 4);
    transfer(frame + 4, len, 0);
    checked(phux_client_feed_frame(client, frame, len + 4));
    free(frame);
    checked(phux_client_effect_clear(client));
    return 1;
}

static PhuxTerminalId completion(uint32_t request, uint32_t kind) {
    while (phux_client_operation_count(client) == 0) assert(pump(5000));
    PhuxOperationResult result = {.size = sizeof(result), .version = PHUX_CLIENT_ABI_VERSION};
    checked(phux_client_operation_get(client, 0, &result));
    assert(result.request_id == request && result.kind == kind);
    assert(result.status == PHUX_OPERATION_SUCCESS);
    assert(result.terminal_id.kind == PHUX_TERMINAL_LOCAL);
    PhuxTerminalId id = result.terminal_id;
    checked(phux_client_operation_clear(client));
    return id;
}

static void wait_grid(PhuxTerminalId id) {
    PhuxTerminalGridView view;
    PhuxClientResult result;
    while ((result = phux_client_terminal_grid(client, &id, &view)) != PHUX_CLIENT_OK) {
        assert(result == PHUX_CLIENT_NO_VALUE || result == PHUX_CLIENT_INVALID_STATE);
        assert(pump(5000));
    }
    checked(phux_client_terminal_grid(client, &id, &view));
    assert(view.cell_count > 0);
}

int main(int argc, char **argv) {
    assert(argc == 3);
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    assert(strlen(argv[1]) < sizeof(address.sun_path));
    strcpy(address.sun_path, argv[1]);
    connection = socket(AF_UNIX, SOCK_STREAM, 0);
    assert(connection >= 0);
    assert(connect(connection, (struct sockaddr *)&address, sizeof(address)) == 0);
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024 * 1024, .max_history_page_bytes = 1024 * 1024,
        .max_history_page_rows = 512, .max_history_cache_bytes = 4 * 1024 * 1024,
        .max_history_materialized_rows = 4096, .history_prefetch_rows = 64,
    };
    checked(phux_client_new(&options, &client));
    checked(phux_client_queue_hello(client, text("detach-live-probe")));
    while (phux_client_state(client) != PHUX_CLIENT_STATE_NEGOTIATED) assert(pump(5000));
    PhuxAttachOptions attach = {
        .size = sizeof(attach), .version = PHUX_CLIENT_ABI_VERSION,
        .attach_id = 1, .target_kind = PHUX_ATTACH_BY_NAME,
        .name = text(argv[2]), .cols = 80, .rows = 24,
    };
    checked(phux_client_queue_attach(client, &attach));
    while (phux_client_state(client) != PHUX_CLIENT_STATE_ATTACHED) assert(pump(5000));
    PhuxTerminalId first = {0};
    for (uint32_t n = 0; n < 20; ++n) {
        PhuxBytes command[] = {text("/bin/sh"), text("-c"),
            text("i=0; while :; do i=$((i+1)); printf '\\rDETACHED-WORK-ALIVE-%s' \"$i\"; sleep 0.05; done")};
        PhuxSpawnOptions spawn = {
            .size = sizeof(spawn), .version = PHUX_CLIENT_ABI_VERSION,
            .request_id = 2 * n + 1, .argv = command, .argc = 3, .cols = 80, .rows = 24,
        };
        checked(phux_client_queue_spawn(client, &spawn));
        PhuxTerminalId id = completion(spawn.request_id, PHUX_OPERATION_SPAWN);
        if (n == 0) first = id;
        wait_grid(id);
        PhuxDetachTerminalOptions detach = {
            .size = sizeof(detach), .version = PHUX_CLIENT_ABI_VERSION,
            .request_id = 2 * n + 2, .terminal_id = id,
        };
        checked(phux_client_queue_detach_terminal(client, &detach));
        assert(completion(detach.request_id, PHUX_OPERATION_DETACH_TERMINAL).id == id.id);
        PhuxTerminalGridView removed;
        assert(phux_client_terminal_grid(client, &id, &removed) == PHUX_CLIENT_INVALID_STATE);
        /* Observe the next output tick: leaked pumps fail unsolicited admission. */
        while (pump(100)) {}
    }
    PhuxAttachTerminalOptions reattach = {
        .size = sizeof(reattach), .version = PHUX_CLIENT_ABI_VERSION,
        .request_id = 41, .terminal_id = first,
    };
    checked(phux_client_queue_attach_terminal(client, &reattach));
    assert(completion(41, PHUX_OPERATION_ATTACH_TERMINAL).id == first.id);
    wait_grid(first);
    PhuxTerminalGridView before;
    checked(phux_client_terminal_grid(client, &first, &before));
    uint64_t sequence = before.last_seq;
    do { assert(pump(5000)); checked(phux_client_terminal_grid(client, &first, &before)); }
    while (before.last_seq == sequence);
    printf("PASS: 20 durable spawn/detach cycles; original terminal %u reattached and still producing output\n", first.id);
    checked(phux_client_disconnect(client));
    phux_client_free(client);
    close(connection);
    return 0;
}
