/* Intentional close against an explicitly isolated socket. A second FFI client
 * observes RESOURCE_CLOSED and surviving output; OS pid probes establish actual
 * process termination rather than merely disappearing client state. */
#include "phux/client.h"
#include <arpa/inet.h>
#include <assert.h>
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

typedef struct { PhuxClient *ffi; int fd; } Client;

static PhuxBytes text(const char *value) {
    return (PhuxBytes){(const uint8_t *)value, strlen(value)};
}

static void checked(Client *client, PhuxClientResult result) {
    if (result == PHUX_CLIENT_OK) return;
    PhuxBytes message = {0};
    phux_client_last_error(client->ffi, &message);
    fprintf(stderr, "FFI result %d: %.*s\n", result, (int)message.len, message.data);
    exit(1);
}

static void transfer(int fd, void *data, size_t size, int writing) {
    uint8_t *bytes = data;
    while (size != 0) {
        ssize_t count = writing ? write(fd, bytes, size) : read(fd, bytes, size);
        assert(count > 0);
        bytes += count;
        size -= (size_t)count;
    }
}

static void flush(Client *client) {
    for (size_t n = 0; n < phux_client_outgoing_count(client->ffi); ++n) {
        PhuxBytes frame;
        checked(client, phux_client_outgoing_get(client->ffi, n, &frame));
        transfer(client->fd, (void *)frame.data, frame.len, 1);
    }
    checked(client, phux_client_outgoing_clear(client->ffi));
}

static int pump(Client *client, int timeout_ms) {
    flush(client);
    struct pollfd fd = {client->fd, POLLIN, 0};
    int ready = poll(&fd, 1, timeout_ms);
    assert(ready >= 0);
    if (ready == 0) return 0;
    uint32_t prefix;
    transfer(client->fd, &prefix, sizeof(prefix), 0);
    size_t len = ntohl(prefix);
    assert(len <= 16 * 1024 * 1024);
    uint8_t *frame = malloc(len + 4);
    assert(frame != NULL);
    memcpy(frame, &prefix, 4);
    transfer(client->fd, frame + 4, len, 0);
    checked(client, phux_client_feed_frame(client->ffi, frame, len + 4));
    free(frame);
    checked(client, phux_client_effect_clear(client->ffi));
    return 1;
}

static Client connect_client(const char *socket_path, const char *session) {
    Client client = {0};
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    assert(strlen(socket_path) < sizeof(address.sun_path));
    strcpy(address.sun_path, socket_path);
    client.fd = socket(AF_UNIX, SOCK_STREAM, 0);
    assert(client.fd >= 0);
    assert(connect(client.fd, (struct sockaddr *)&address, sizeof(address)) == 0);
    PhuxClientOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 1024 * 1024, .max_history_page_bytes = 1024 * 1024,
        .max_history_page_rows = 512, .max_history_cache_bytes = 4 * 1024 * 1024,
        .max_history_materialized_rows = 4096, .history_prefetch_rows = 64,
    };
    checked(&client, phux_client_new(&options, &client.ffi));
    checked(&client, phux_client_queue_hello(client.ffi, text("close-resource-live")));
    while (phux_client_state(client.ffi) != PHUX_CLIENT_STATE_NEGOTIATED) assert(pump(&client, 5000));
    PhuxAttachOptions attach = {
        .size = sizeof(attach), .version = PHUX_CLIENT_ABI_VERSION,
        .attach_id = 1, .target_kind = PHUX_ATTACH_BY_NAME,
        .name = text(session), .cols = 80, .rows = 24,
    };
    checked(&client, phux_client_queue_attach(client.ffi, &attach));
    while (phux_client_state(client.ffi) != PHUX_CLIENT_STATE_ATTACHED) assert(pump(&client, 5000));
    return client;
}

static PhuxResourceId completion(Client *client, uint32_t request, uint32_t kind) {
    while (phux_client_operation_count(client->ffi) == 0) assert(pump(client, 5000));
    PhuxOperationResult result = {.size = sizeof(result), .version = PHUX_CLIENT_ABI_VERSION};
    checked(client, phux_client_operation_get(client->ffi, 0, &result));
    assert(result.request_id == request && result.kind == kind);
    assert(result.status == PHUX_OPERATION_SUCCESS);
    assert(result.terminal_id.kind == PHUX_RESOURCE_ID_LOCAL);
    PhuxResourceId id = result.terminal_id;
    checked(client, phux_client_operation_clear(client->ffi));
    return id;
}

static PhuxTerminalGridView wait_grid(Client *client, PhuxResourceId id) {
    PhuxTerminalGridView view;
    PhuxClientResult result;
    while ((result = phux_client_terminal_grid(client->ffi, &id, &view)) != PHUX_CLIENT_OK) {
        assert(result == PHUX_CLIENT_NO_VALUE || result == PHUX_CLIENT_INVALID_STATE);
        assert(pump(client, 5000));
    }
    assert(view.cell_count > 0);
    return view;
}

static PhuxResourceId spawn(Client *client, uint32_t request, const char *script) {
    PhuxBytes command[] = {text("/bin/sh"), text("-c"), text(script)};
    PhuxSpawnOptions options = {
        .size = sizeof(options), .version = PHUX_CLIENT_ABI_VERSION,
        .request_id = request, .argv = command, .argc = 3, .cols = 80, .rows = 24,
    };
    checked(client, phux_client_queue_spawn_bound(client->ffi, &options));
    PhuxResourceId id = completion(client, request, PHUX_OPERATION_SPAWN);
    wait_grid(client, id);
    return id;
}

static pid_t read_pid(Client *client, const char *path) {
    for (int attempt = 0; attempt < 50; ++attempt) {
        FILE *file = fopen(path, "r");
        if (file != NULL) {
            int pid = 0;
            int count = fscanf(file, "%d", &pid);
            fclose(file);
            if (count == 1 && pid > 0) {
                assert(kill(pid, 0) == 0);
                return (pid_t)pid;
            }
        }
        pump(client, 100);
    }
    assert(!"workload did not publish its pid");
    return 0;
}

static void disconnect_client(Client *client) {
    checked(client, phux_client_disconnect(client->ffi));
    phux_client_free(client->ffi);
    close(client->fd);
}

static void verify_closed(Client *observer, PhuxResourceId id, pid_t pid) {
    PhuxTerminalGridView view;
    while (phux_client_terminal_grid(observer->ffi, &id, &view) == PHUX_CLIENT_OK) assert(pump(observer, 5000));
    for (int attempts = 0; kill(pid, 0) == 0; ++attempts) {
        assert(attempts < 100);
        pump(observer, 100);
    }
    assert(errno == ESRCH);
}

static void batch_close(Client *owner, Client *observer, PhuxResourceId ids[2]) {
    pid_t first = read_pid(owner, "batch1.pid"), second = read_pid(owner, "batch2.pid");
    checked(owner, phux_client_queue_close_resources(owner->ffi, 6, ids, 2));
    assert(completion(owner, 6, PHUX_OPERATION_CLOSE_RESOURCES).id == 0);
    verify_closed(observer, ids[0], first);
    verify_closed(observer, ids[1], second);
    printf("PASS: one batch close terminated resources %u/%u (pids %d/%d), independently observed\n",
        ids[0].id, ids[1].id, (int)first, (int)second);
}

int main(int argc, char **argv) {
    assert(argc == 3);
    Client owner = connect_client(argv[1], argv[2]);
    PhuxResourceId victim = spawn(&owner, 1, "echo $$ > victim.pid; exec sleep 3600");
    PhuxResourceId survivor = spawn(&owner, 2,
        "echo $$ > survivor.pid; i=0; while :; do i=$((i+1)); printf '\\rSURVIVOR-%s' \"$i\"; sleep 0.05; done");
    PhuxResourceId batch[] = {
        spawn(&owner, 3, "echo $$ > batch1.pid; exec sleep 3600"),
        spawn(&owner, 4, "echo $$ > batch2.pid; exec sleep 3600"),
    };
    Client observer = connect_client(argv[1], argv[2]);
    wait_grid(&observer, victim);
    wait_grid(&observer, batch[0]);
    wait_grid(&observer, batch[1]);
    pid_t victim_pid = read_pid(&owner, "victim.pid"), survivor_pid = read_pid(&owner, "survivor.pid");
    uint64_t sequence = wait_grid(&observer, survivor).last_seq;
    checked(&owner, phux_client_queue_close_resource(owner.ffi, 5, &victim));
    assert(completion(&owner, 5, PHUX_OPERATION_CLOSE_RESOURCE).id == victim.id);
    verify_closed(&observer, victim, victim_pid);
    assert(kill(survivor_pid, 0) == 0);
    while (wait_grid(&observer, survivor).last_seq == sequence) assert(pump(&observer, 5000));
    printf("PASS: attached close %u terminated pid %d; second client observed closure; resource %u pid %d still produces output\n",
        victim.id, (int)victim_pid, survivor.id, (int)survivor_pid);
    batch_close(&owner, &observer, batch);
    assert(kill(survivor_pid, 0) == 0);
    checked(&owner, phux_client_queue_close_resource(owner.ffi, 7, &survivor));
    assert(completion(&owner, 7, PHUX_OPERATION_CLOSE_RESOURCE).id == survivor.id);
    disconnect_client(&observer);
    disconnect_client(&owner);
    return 0;
}
