// Stock 1.0 decoder contract probe; see docs/libghostty.md.
#include <ghostty/vt.h>
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { const uint8_t *data; size_t len, offset, eof_reads; } Reader;

static bool read_bytes(void *context, uint8_t *data, size_t capacity, size_t *written) {
    Reader *reader = context;
    size_t count = reader->len - reader->offset;
    if (count > capacity) count = capacity;
    if (count > 31) count = 31; // force real short reads
    memcpy(data, reader->data + reader->offset, count);
    reader->offset += count;
    *written = count;
    if (count == 0) reader->eof_reads++;
    return true;
}

static uint16_t tag(const uint8_t *record) {
    return (uint16_t)(record[0] | (uint16_t)record[1] << 8);
}

static size_t record_len(const uint8_t *record) {
    return 10 + ((size_t)record[2] | (size_t)record[3] << 8 |
        (size_t)record[4] << 16 | (size_t)record[5] << 24);
}

static void check_live_terminal(GhosttyTerminal terminal) {
    uint16_t x = 0;
    assert(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_CURSOR_X, &x) == GHOSTTY_SUCCESS);
    assert(x == 1); // unfinished ESC [31 + live mX
}

static void decode_with_lookahead(const uint8_t *bytes, size_t len, size_t ready) {
    Reader reader = {.data = bytes, .len = ready};
    GhosttyReader input = {.read = read_bytes, .userdata = &reader};
    GhosttySnapshotDecoder decoder = NULL;
    GhosttyTerminal terminal = NULL;
    assert(ghostty_snapshot_decoder_new(NULL, &decoder, input) == GHOSTTY_SUCCESS);
    assert(ghostty_snapshot_decoder_ready(decoder, &terminal) == GHOSTTY_SUCCESS);
    assert(reader.offset == ready && reader.eof_reads == 0);
    ghostty_terminal_vt_write(terminal, (const uint8_t *)"mX", 2);
    check_live_terminal(terminal);
    size_t end = ready, pages = 0;
    while (end < len) {
        // Stage a complete operation and the next real header. Stock PAGE
        // decoding reads ahead to distinguish an optional placement record.
        while (tag(bytes + end) != 3 && tag(bytes + end) != 6) end += record_len(bytes + end);
        uint16_t current = tag(bytes + end);
        end += record_len(bytes + end);
        reader.len = end < len ? end + 10 : end;
        GhosttyResult result = ghostty_snapshot_decoder_next(decoder);
        if (current == 6) {
            assert(result == GHOSTTY_NO_VALUE);
            break;
        }
        assert(result == GHOSTTY_SUCCESS);
        pages++;
        check_live_terminal(terminal);
        // No waiting, worker thread, or temporary EOF was needed. The callback
        // context stays at the same address while available bytes grow.
        assert(reader.eof_reads == 0);
    }
    assert(pages > 0 && reader.offset == len);
    assert(ghostty_snapshot_decoder_next(decoder) == GHOSTTY_NO_VALUE);
    ghostty_snapshot_decoder_free(decoder);
    ghostty_terminal_free(terminal);
    printf("PASS: stock READY/live continuation/%zu deferred pages/FINISH, no EOF reads\n", pages);
}

static void show_bare_page_failure(const uint8_t *bytes, size_t len, size_t ready) {
    Reader reader = {.data = bytes, .len = ready};
    GhosttyReader input = {.read = read_bytes, .userdata = &reader};
    GhosttySnapshotDecoder decoder = NULL;
    GhosttyTerminal terminal = NULL;
    assert(ghostty_snapshot_decoder_new(NULL, &decoder, input) == GHOSTTY_SUCCESS);
    assert(ghostty_snapshot_decoder_ready(decoder, &terminal) == GHOSTTY_SUCCESS);
    size_t end = ready;
    while (tag(bytes + end) != 3) end += record_len(bytes + end);
    reader.len = end + record_len(bytes + end);
    assert(ghostty_snapshot_decoder_next(decoder) == GHOSTTY_SUCCESS);
    assert(reader.eof_reads > 0);
    reader.len = len;
    assert(ghostty_snapshot_decoder_next(decoder) != GHOSTTY_SUCCESS);
    ghostty_snapshot_decoder_free(decoder);
    ghostty_terminal_free(terminal);
    puts("PASS: a bare PAGE causes sticky lookahead failure; adding bytes cannot recover it");
}

int main(void) {
    GhosttyTerminal source = NULL;
    assert(ghostty_terminal_new(NULL, &source, 80, 24) == GHOSTTY_SUCCESS);
    const size_t history_bytes = 10 * 1024 * 1024, continuation = 1024;
    assert(ghostty_terminal_set(source, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &history_bytes) == GHOSTTY_SUCCESS);
    assert(ghostty_terminal_set(source, GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES, &continuation) == GHOSTTY_SUCCESS);
    for (size_t line = 0; line < 6000; line++) {
        const uint8_t text[] = "history row\r\n";
        ghostty_terminal_vt_write(source, text, sizeof(text) - 1);
    }
    ghostty_terminal_vt_write(source, (const uint8_t *)"\033[31", 4);
    uint8_t *bytes = NULL;
    size_t len = 0;
    assert(ghostty_snapshot_encode_alloc(source, NULL, &bytes, &len) == GHOSTTY_SUCCESS);
    size_t ready = 10;
    while (tag(bytes + ready) != 5) ready += record_len(bytes + ready);
    ready += record_len(bytes + ready);
    assert(ready < len);
    decode_with_lookahead(bytes, len, ready);
    show_bare_page_failure(bytes, len, ready);
    ghostty_free(NULL, bytes, len);
    ghostty_terminal_free(source);
    return 0;
}
