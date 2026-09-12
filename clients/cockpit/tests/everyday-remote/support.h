/* Real exported C ABI, using the framing pattern from detach_live.c.
 * The transport is always the production remote tunnel, never a mock relay. */
#include "phux/client.h"
#include <arpa/inet.h>
#include <assert.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static PhuxClient *client;
static PhuxRemoteTunnel *tunnel;
static int connection = -1;
static unsigned expired_history;

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

static PhuxRemoteTunnelInfo tunnel_info(void) {
    PhuxRemoteTunnelInfo info = {.size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION};
    checked(phux_remote_tunnel_info(tunnel, &info));
    return info;
}

static void transfer(void *data, size_t size, int writing) {
    uint8_t *bytes = data;
    while (size != 0) {
        struct pollfd fd = {connection, writing ? POLLOUT : POLLIN, 0};
        assert(poll(&fd, 1, 10000) == 1);
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

static void effects(void) {
    for (size_t n = 0; n < phux_client_effect_count(client); ++n) {
        PhuxClientEffect effect;
        checked(phux_client_effect_get(client, n, &effect));
        if (effect.kind != PHUX_CLIENT_EFFECT_STATUS) continue;
        if (effect.detail == PHUX_CLIENT_STATUS_HISTORY_UNAVAILABLE) {
            printf("history unavailable code=%u\n", effect.status_code);
            expired_history += effect.status_code == PHUX_CLIENT_HISTORY_UNAVAILABLE_EXPIRED;
        }
    }
    checked(phux_client_effect_clear(client));
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
    effects();
    return 1;
}

static void connect_remote(const char *config, const char *target) {
    PhuxRemoteTarget request = {.size = sizeof(request), .version = PHUX_CLIENT_ABI_VERSION,
        .target = text(target), .config_path = text(config)};
    checked(phux_remote_tunnel_resolve(&request, &tunnel));
    assert(tunnel_info().state == PHUX_REMOTE_TUNNEL_RESOLVED);
    int sockets[2];
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) == 0);
    connection = sockets[0];
    checked(phux_remote_tunnel_start(tunnel, sockets[1]));
}

static void negotiate(const char *config, const char *target) {
    connect_remote(config, target);
    PhuxClientOptions options = {.size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024 * 1024, .max_history_page_bytes = 1024 * 1024,
        .max_history_page_rows = 32, .max_history_cache_bytes = 4 * 1024 * 1024,
        .max_history_materialized_rows = 4096, .history_prefetch_rows = 64};
    checked(phux_client_new(&options, &client));
    checked(phux_client_queue_hello(client, text("everyday-remote-live")));
    while (phux_client_state(client) != PHUX_CLIENT_STATE_NEGOTIATED) assert(pump(5000));
}

static void attach(const char *config, const char *target, const char *session) {
    negotiate(config, target);
    PhuxAttachOptions request = {.size = sizeof(request), .version = PHUX_CLIENT_ABI_VERSION,
        .attach_id = 1, .target_kind = PHUX_ATTACH_BY_NAME, .name = text(session),
        .cols = 80, .rows = 24, .request_scrollback = true, .scrollback_limit_lines = 2000};
    checked(phux_client_queue_attach(client, &request));
    while (phux_client_state(client) != PHUX_CLIENT_STATE_ATTACHED) assert(pump(5000));
}

static PhuxTerminalGridView grid(PhuxResourceId id) {
    PhuxTerminalGridView view;
    PhuxClientResult result;
    while ((result = phux_client_terminal_grid(client, &id, &view)) != PHUX_CLIENT_OK) {
        assert(result == PHUX_CLIENT_NO_VALUE || result == PHUX_CLIENT_INVALID_STATE);
        assert(pump(5000));
    }
    assert(view.cell_count > 0);
    return view;
}

static PhuxResourceId first_terminal(void) {
    for (size_t n = 0; n < phux_client_resource_count(client); ++n) {
        PhuxResourceInfo info = {.size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION};
        checked(phux_client_resource_get(client, n, &info));
        if (info.kind == PHUX_RESOURCE_TERMINAL) return info.terminal_id;
    }
    assert(!"no terminal in fixture");
    return (PhuxResourceId){0};
}

static void disconnect_remote(void) {
    checked(phux_client_disconnect(client));
    close(connection);
    connection = -1;
    phux_remote_tunnel_free(tunnel);
    tunnel = NULL;
}

static void destroy_client(void) {
    phux_client_free(client);
    client = NULL;
}

static PhuxResourceId completion(uint32_t request, uint32_t kind) {
    while (phux_client_operation_count(client) == 0) assert(pump(5000));
    PhuxOperationResult result = {.size = sizeof(result), .version = PHUX_CLIENT_ABI_VERSION};
    checked(phux_client_operation_get(client, 0, &result));
    assert(result.request_id == request && result.kind == kind);
    assert(result.status == PHUX_OPERATION_SUCCESS);
    assert(result.terminal_id.kind == PHUX_RESOURCE_ID_LOCAL);
    PhuxResourceId id = result.terminal_id;
    checked(phux_client_operation_clear(client));
    return id;
}

static PhuxWorkspaceInfo workspace(void) {
    PhuxWorkspaceInfo info = {.size = sizeof(info), .version = PHUX_CLIENT_ABI_VERSION};
    checked(phux_client_workspace_info(client, &info));
    while (info.state == 0 || info.status == 1) {
        assert(pump(5000));
        checked(phux_client_workspace_info(client, &info));
    }
    assert(info.state == 1 || info.state == 2);
    return info;
}

static PhuxWorkspaceWindow window(void) {
    (void)workspace();
    PhuxWorkspaceWindow result = {.size = sizeof(result), .version = PHUX_CLIENT_ABI_VERSION};
    checked(phux_client_workspace_window_get(client, 0, &result));
    return result;
}
