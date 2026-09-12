#include "support.h"

static size_t execution_count(const char *path) {
    FILE *file = fopen(path, "r");
    if (file == NULL) return 0;
    size_t count = 0;
    char line[256];
    while (fgets(line, sizeof(line), file) != NULL) ++count;
    fclose(file);
    return count;
}

static void send_once(PhuxResourceId id, const char *value, const char *record, size_t before) {
    assert(execution_count(record) == before);
    checked(phux_client_send_paste(client, &id, (const uint8_t *)value, strlen(value), true));
    while (execution_count(record) == before) assert(pump(5000));
    while (pump(100)) {}
    assert(execution_count(record) == before + 1);
}

static void server_identity(char *out, size_t capacity) {
    PhuxBytes identity;
    checked(phux_client_server_id(client, &identity));
    assert(identity.len > 0 && identity.len < capacity);
    memcpy(out, identity.data, identity.len);
    out[identity.len] = 0;
}

static void drain_history(PhuxResourceId id) {
    PhuxTerminalGridView view = grid(id);
    while (view.history_loading) {
        assert(pump(5000));
        view = grid(id);
    }
    printf("history: pages=%llu rows=%llu bytes=%llu\n",
        (unsigned long long)view.history_pages_loaded,
        (unsigned long long)view.history_total_rows,
        (unsigned long long)view.history_bytes_loaded);
    assert(view.history_pages_loaded > 1);
    assert(view.history_total_rows > view.rows);
}

static size_t search_count(PhuxResourceId id, const char *query) {
    const PhuxSearchResult *results;
    size_t count;
    checked(phux_client_search(client, &id, text(query), true, &results, &count));
    checked(phux_client_search_results_release(client));
    return count;
}

static void oldest_history(PhuxResourceId id) {
    PhuxTerminalGridView view = grid(id);
    while (view.history_has_more) {
        checked(phux_client_scroll_viewport(client, &id, 0, 0));
        do {
            assert(pump(5000));
            view = grid(id);
        } while (view.history_loading);
    }
    assert(search_count(id, "HISTORY-0000") == 1);
    printf("PASS: oldest generated history became searchable (%llu rows)\n",
        (unsigned long long)view.history_total_rows);
}

static void row_text(PhuxResourceId id, char *out, size_t capacity) {
    PhuxTerminalGridView view = grid(id);
    size_t written = 0;
    for (size_t n = 0; n < view.cols; ++n) {
        PhuxTerminalCell cell = view.cells[n];
        assert(written + cell.utf8_len < capacity);
        memcpy(out + written, view.utf8.data + cell.utf8_offset, cell.utf8_len);
        written += cell.utf8_len;
    }
    out[written] = 0;
}

static void pinned_history_input(PhuxResourceId id) {
    assert(search_count(id, "HISTORY-0000") == 0);
    assert(grid(id).history_loading);
    assert(execution_count("executed") == 0);
    checked(phux_client_send_paste(client, &id, (const uint8_t *)"first-input\n", 12, true));
    while (grid(id).history_pages_loaded == 0) assert(pump(5000));
    checked(phux_client_scroll_viewport(client, &id, 2, -1));
    PhuxDocumentAnchor anchor;
    PhuxDocumentPoint top = {.space = PHUX_DOCUMENT_VIEWPORT, .row = 0, .column = 0};
    checked(phux_client_anchor_create(client, &id, top, &anchor));
    checked(phux_client_history_viewport_pin(client, &id, anchor));
    char before[1024], after[1024];
    row_text(id, before, sizeof(before));
    drain_history(id);
    row_text(id, after, sizeof(after));
    assert(strcmp(before, after) == 0);
    checked(phux_client_anchor_release(client, &id, anchor));
    assert(execution_count("executed") == 1);
    oldest_history(id);
    checked(phux_client_history_follow_live(client, &id));
}

static PhuxResourceId split_work(PhuxResourceId first) {
    PhuxBytes argv[] = {text("/bin/sh"), text("-c"),
        text("stty -echo; printf 'SECOND-READY\\n'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> second-executed; printf 'SECOND:%s\\n' \"$line\"; done")};
    PhuxSpawnOptions spawn = {.size = sizeof(spawn), .version = PHUX_CLIENT_ABI_VERSION,
        .request_id = 1, .owner_terminal = &first, .argv = argv, .argc = 3, .cols = 80, .rows = 24};
    checked(phux_client_queue_spawn(client, &spawn));
    PhuxResourceId second = completion(1, PHUX_OPERATION_SPAWN);
    (void)grid(second);
    checked(phux_client_workspace_refresh(client, 2));
    PhuxWorkspaceInfo info = workspace();
    PhuxWorkspaceWindow current = window();
    PhuxWorkspaceMutation split = {.size = sizeof(split), .version = PHUX_CLIENT_ABI_VERSION,
        .request_id = 3, .session_id = info.session_id, .expected_revision = info.revision,
        .kind = 2, .terminal_id = first, .new_terminal_id = second, .direction = 2, .ratio = 0.5};
    memcpy(split.window_id, current.window_id, 16);
    checked(phux_client_workspace_mutate(client, &split));
    info = workspace();
    assert(info.status == 2 && info.node_count == 3);
    split.request_id = 4;
    split.expected_revision = info.revision;
    split.kind = 6;
    split.name = text("Transport proof");
    checked(phux_client_workspace_mutate(client, &split));
    assert(workspace().status == 2);
    send_once(second, "second-terminal-only\n", "second-executed", 0);
    assert(execution_count("executed") == 1);
    return second;
}

static void assert_restored_layout(void) {
    PhuxWorkspaceInfo info = workspace();
    assert(info.state == 2 && info.node_count == 3);
    PhuxWorkspaceWindow current = window();
    assert(current.name.len == strlen("Transport proof"));
    assert(memcmp(current.name.data, "Transport proof", current.name.len) == 0);
}

static void lifecycle(const char *config, const char *target) {
    attach(config, target, "everyday");
    PhuxResourceId id = first_terminal();
    PhuxTerminalGridView initial = grid(id);
    assert(initial.history_loading);
    char identity[256], restored[256];
    server_identity(identity, sizeof(identity));
    FILE *saved_identity = fopen("server-identity", "w");
    assert(saved_identity != NULL);
    assert(fputs(identity, saved_identity) >= 0);
    assert(fclose(saved_identity) == 0);
    pinned_history_input(id);
    PhuxResourceId second = split_work(id);
    disconnect_remote();
    assert(phux_client_send_paste(client, &id, (const uint8_t *)"must-not-run\n", 13, true)
        == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_outgoing_count(client) == 0);
    destroy_client();

    attach(config, target, "everyday");
    server_identity(restored, sizeof(restored));
    assert(strcmp(identity, restored) == 0);
    assert(first_terminal().id == id.id);
    (void)grid(id);
    (void)grid(second);
    assert_restored_layout();
    assert(execution_count("executed") == 1);
    send_once(id, "after-reconnect\n", "executed", 1);
    assert(execution_count("second-executed") == 1);
    printf("PASS %s: pinned authenticated attach, pinned history/input, split/rename, disconnected input refused, same server/terminals/layout reconnect, exact target executions\n", target);
    disconnect_remote();
    destroy_client();
}

static void expired_lease(const char *config, const char *target) {
    attach(config, target, "everyday");
    PhuxResourceId id = first_terminal();
    PhuxTerminalGridView initial = grid(id);
    assert(initial.history_loading);
    assert(phux_client_outgoing_count(client) > 0);
    /* Production NATIVE_HISTORY_TTL is 30s. Hold the next queued request
     * beyond the actual lease, rather than fabricating a stale wire reply. */
    sleep(31);
    while (expired_history == 0) assert(pump(5000));
    assert(!grid(id).history_loading);
    send_once(id, "after-expired-history\n", "executed", 3);
    printf("PASS %s: real 30s history lease expired; loading settled; terminal input still executes\n", target);
    disconnect_remote();
    destroy_client();
}

static void mark(const char *path) {
    FILE *file = fopen(path, "w");
    assert(file != NULL);
    assert(fclose(file) == 0);
}

static void stalled_peer(const char *config, const char *target) {
    attach(config, target, "everyday");
    PhuxResourceId id = first_terminal();
    (void)grid(id);
    while (pump(100)) {}
    char identity[256], restored[256];
    server_identity(identity, sizeof(identity));
    mark("stall-ready");
    /* Python suspends only this fixture's server. Real QUIC idle/WSS ping
     * deadlines detect the silent peer; no fabricated disconnect frame. */
    while (tunnel_info().state == PHUX_REMOTE_TUNNEL_CONNECTED) sleep(1);
    PhuxRemoteTunnelInfo info = tunnel_info();
    assert(info.state == PHUX_REMOTE_TUNNEL_FAILED && info.message.len > 0);
    printf("%s stalled-peer detection: %.*s\n", target, (int)info.message.len, info.message.data);
    disconnect_remote();
    assert(phux_client_send_paste(client, &id, (const uint8_t *)"must-not-run\n", 13, true)
        == PHUX_CLIENT_INVALID_STATE);
    assert(phux_client_outgoing_count(client) == 0);
    destroy_client();
    mark("loss-detected");
    while (access("server-resumed", F_OK) != 0) sleep(1);
    attach(config, target, "everyday");
    server_identity(restored, sizeof(restored));
    assert(strcmp(identity, restored) == 0);
    (void)grid(id);
    assert_restored_layout();
    send_once(id, "after-link-stall\n", "executed", 2);
    printf("PASS %s: production liveness detected silent server; post-detection input refused; same terminal resumed\n", target);
    disconnect_remote();
    destroy_client();
}

static void refused(const char *config, const char *target) {
    connect_remote(config, target);
    struct pollfd fd = {connection, POLLIN, 0};
    assert(poll(&fd, 1, 20000) == 1);
    char byte;
    assert(read(connection, &byte, 1) == 0);
    PhuxRemoteTunnelInfo info = tunnel_info();
    assert(info.state == PHUX_REMOTE_TUNNEL_FAILED);
    assert(info.message.len > 0);
    printf("PASS %s: refused before protocol admission: %.*s\n", target,
        (int)info.message.len, info.message.data);
    close(connection);
    phux_remote_tunnel_free(tunnel);
}

static void cold_restart(const char *config, const char *target) {
    char before[256] = {0}, after[256];
    FILE *saved = fopen("server-identity", "r");
    assert(saved != NULL);
    assert(fread(before, 1, sizeof(before) - 1, saved) > 0);
    assert(fclose(saved) == 0);
    negotiate(config, target);
    server_identity(after, sizeof(after));
    assert(strcmp(before, after) != 0);
    printf("PASS %s: cold server carries a different HELLO_OK identity\n", target);
    disconnect_remote();
    destroy_client();
}

int main(int argc, char **argv) {
    assert(argc == 4);
    signal(SIGPIPE, SIG_IGN);
    alarm(90);
    if (strcmp(argv[3], "lifecycle") == 0) lifecycle(argv[1], argv[2]);
    else if (strcmp(argv[3], "lease") == 0) expired_lease(argv[1], argv[2]);
    else if (strcmp(argv[3], "refused") == 0) refused(argv[1], argv[2]);
    else if (strcmp(argv[3], "stall") == 0) stalled_peer(argv[1], argv[2]);
    else if (strcmp(argv[3], "cold") == 0) cold_restart(argv[1], argv[2]);
    else assert(!"unknown probe mode");
    return 0;
}
