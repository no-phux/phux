/* Real owning-connection create, duplicate refusal, and reconnect persistence.
 * Run with run_detach_live.py: explicit private socket and seeded session only.
 * Link against this checkout's FFI, never an installed system library. */
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

static PhuxBytes text(const char *s) {
    return (PhuxBytes){(const uint8_t *)s, strlen(s)};
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
    while (size) {
        ssize_t count = writing ? write(connection, bytes, size) : read(connection, bytes, size);
        assert(count > 0);
        bytes += count;
        size -= (size_t)count;
    }
}

static void pump(void) {
    for (size_t n = 0; n < phux_client_outgoing_count(client); ++n) {
        PhuxBytes frame;
        checked(phux_client_outgoing_get(client, n, &frame));
        transfer((void *)frame.data, frame.len, 1);
    }
    checked(phux_client_outgoing_clear(client));
    struct pollfd fd = {connection, POLLIN, 0};
    assert(poll(&fd, 1, 5000) == 1);
    uint32_t prefix;
    transfer(&prefix, sizeof(prefix), 0);
    size_t size = ntohl(prefix);
    assert(size <= 16 * 1024 * 1024);
    uint8_t *frame = malloc(size + 4);
    assert(frame);
    memcpy(frame, &prefix, 4);
    transfer(frame + 4, size, 0);
    checked(phux_client_feed_frame(client, frame, size + 4));
    free(frame);
}

static void open_client(const char *path) {
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    assert(strlen(path) < sizeof(address.sun_path));
    strcpy(address.sun_path, path);
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
    checked(phux_client_queue_hello(client, text("session-create-live")));
    while (phux_client_state(client) != PHUX_CLIENT_STATE_NEGOTIATED) pump();
}

static void close_client(void) {
    checked(phux_client_disconnect(client));
    phux_client_free(client);
    close(connection);
}

static PhuxSessionCreateInfo result(uint32_t id) {
    PhuxSessionCreateInfo info = {.size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION};
    do {
        checked(phux_client_session_create_info(client, id, &info));
        if (info.status == 1) pump();
    } while (info.status == 1);
    return info;
}

static void verify_empty_sessions(void) {
    size_t empty = 0;
    for (size_t n = 0; n < phux_client_session_count(client); ++n) {
        uint32_t flags = 0;
        checked(phux_client_session_flags(client, n, &flags));
        if (flags == (PHUX_SESSION_FLAG_KEEP_EMPTY | PHUX_SESSION_FLAG_EMPTY)) ++empty;
    }
    assert(empty == 2);
    assert(phux_client_state(client) == PHUX_CLIENT_STATE_NEGOTIATED);
}

int main(int argc, char **argv) {
    assert(argc == 3); /* runner also passes its seed session name */
    open_client(argv[1]);
    checked(phux_client_create_session(client, 1, text("ffi-empty-one"), true));
    checked(phux_client_create_session(client, 2, text("ffi-empty-two"), true));
    PhuxSessionCreateInfo second = result(2);
    PhuxSessionCreateInfo first = result(1);
    assert(first.status == 2 && second.status == 2);
    assert(first.session_id && second.session_id && first.session_id != second.session_id);
    checked(phux_client_session_create_release(client, 1));
    checked(phux_client_session_create_release(client, 2));
    checked(phux_client_create_session(client, 3, text("ffi-empty-one"), true));
    assert(result(3).status == 3);
    verify_empty_sessions();
    close_client();
    open_client(argv[1]);
    checked(phux_client_query_sessions(client, 1));
    uint32_t id = 0, status = 0;
    do {
        pump();
        checked(phux_client_session_query_status(client, &id, &status));
    } while (status == PHUX_SESSION_QUERY_PENDING);
    assert(status == PHUX_SESSION_QUERY_OK);
    verify_empty_sessions();
    close_client();
    puts("PASS: real concurrent keep-empty creates, exact IDs, duplicate refusal, reconnect persistence; never attached");
    return 0;
}
