#define _POSIX_C_SOURCE 200809L
#include <ghostty/vt.h>
#include <assert.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

// Deliberately discard output: this measures engine work, not networking or
// snapshot-buffer allocation. See docs/libghostty.md for reproduction.
// PHUX_CAPTURE is only for comparison with the historical detached-capture pin.
typedef struct {
    size_t total, envelope, header_len, remaining, largest_write;
    uint8_t header[10];
    uint16_t tag;
    uint64_t started, ready;
} Sink;

static uint64_t now_ns(void) {
    struct timespec value;
    assert(clock_gettime(CLOCK_MONOTONIC, &value) == 0);
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec;
}

static void record_complete(Sink *sink) {
    if (sink->tag == 5 && sink->ready == 0) sink->ready = now_ns();
    sink->header_len = 0;
}

static size_t read_header(Sink *sink, const uint8_t *data, size_t len) {
    size_t count = 10 - sink->header_len;
    if (count > len) count = len;
    memcpy(sink->header + sink->header_len, data, count);
    sink->header_len += count;
    if (sink->header_len != 10) return count;
    sink->tag = (uint16_t)(sink->header[0] | (uint16_t)sink->header[1] << 8);
    sink->remaining = (size_t)sink->header[2] | (size_t)sink->header[3] << 8 |
        (size_t)sink->header[4] << 16 | (size_t)sink->header[5] << 24;
    if (sink->remaining == 0) record_complete(sink);
    return count;
}

static bool discard_write(void *context, const uint8_t *data, size_t len) {
    Sink *sink = context;
    sink->total += len;
    if (len > sink->largest_write) sink->largest_write = len;
    while (len > 0) {
        size_t count;
        if (sink->envelope < 10) {
            count = 10 - sink->envelope;
            if (count > len) count = len;
            sink->envelope += count;
        } else if (sink->header_len < 10) {
            count = read_header(sink, data, len);
        } else {
            count = sink->remaining;
            if (count > len) count = len;
            sink->remaining -= count;
            if (sink->remaining == 0) record_complete(sink);
        }
        data += count;
        len -= count;
    }
    return true;
}

static void fill_history(GhosttyTerminal terminal, size_t lines, bool dense) {
    uint8_t batch[122 * 128];
    const size_t width = dense ? 120 : 24;
    for (size_t line = 0; line < 128; line++) {
        for (size_t col = 0; col < width; col++) {
            batch[line * (width + 2) + col] = (uint8_t)(33 + (line * 37 + col * 17) % 90);
        }
        batch[line * (width + 2) + width] = '\r';
        batch[line * (width + 2) + width + 1] = '\n';
    }
    while (lines > 0) {
        size_t count = lines < 128 ? lines : 128;
        ghostty_terminal_vt_write(terminal, batch, count * (width + 2));
        lines -= count;
    }
}

static void fill_images(GhosttyTerminal terminal, size_t count) {
    // Each 128x128 RGB image occupies 48 KiB, below a 256 KiB unit budget.
    uint8_t command[4200];
    for (size_t image = 1; image <= count; image++) {
        for (size_t chunk = 0; chunk < 16; chunk++) {
            int prefix = chunk == 0
                ? snprintf((char *)command, sizeof(command),
                    "\033_Ga=T,f=24,s=128,v=128,i=%zu,q=2,m=1;", image)
                : snprintf((char *)command, sizeof(command),
                    "\033_Gm=%d;", chunk == 15 ? 0 : 1);
            assert(prefix > 0 && (size_t)prefix + 4098 < sizeof(command));
            memset(command + prefix, 'A', 4096);
            command[prefix + 4096] = 27;
            command[prefix + 4097] = '\\';
            ghostty_terminal_vt_write(terminal, command, (size_t)prefix + 4098);
        }
    }
}

static void print_sample(const char *mode, Sink *sink, uint64_t released,
        uint64_t finished, GhosttyTerminal terminal, size_t sample) {
    uint64_t rows = 0;
    GhosttyTerminalMemoryUsage memory = {.size = sizeof(memory)};
    assert(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS, &rows) == GHOSTTY_SUCCESS);
    assert(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_MEMORY_USAGE, &memory) == GHOSTTY_SUCCESS);
    printf("{\"mode\":\"%s\",\"sample\":%zu,\"history_rows\":%" PRIu64
        ",\"resident_bytes\":%" PRIu64 ",\"image_bytes\":%" PRIu64
        ",\"ready_us\":%.3f,\"source_release_us\":%.3f,\"complete_us\":%.3f"
        ",\"encoded_bytes\":%zu,\"largest_write\":%zu}\n",
        mode, sample, rows, memory.primary_resident_bytes, memory.primary_image_bytes,
        (double)(sink->ready - sink->started) / 1000.0,
        (double)(released - sink->started) / 1000.0,
        (double)(finished - sink->started) / 1000.0,
        sink->total, sink->largest_write);
}

static void measure_stock(GhosttyTerminal terminal, size_t sample) {
    Sink sink = {.started = now_ns()};
    GhosttyWriter writer = {.write = discard_write, .userdata = &sink};
    assert(ghostty_snapshot_encode(terminal, writer) == GHOSTTY_SUCCESS);
    uint64_t end = now_ns();
    assert(sink.ready != 0);
    print_sample("stock-full", &sink, end, end, terminal, sample);
}

#ifdef PHUX_CAPTURE
static void measure_capture(GhosttyTerminal terminal, size_t sample) {
    Sink sink = {.started = now_ns()};
    GhosttyWriter writer = {.write = discard_write, .userdata = &sink};
    GhosttySnapshotCaptureOptions options = {
        .size = sizeof(options), .max_record_bytes = 256 * 1024, .max_pages = 100000,
    };
    GhosttySnapshotCapture capture = NULL;
    assert(ghostty_snapshot_capture_new(NULL, terminal, writer, &options, &capture) == GHOSTTY_SUCCESS);
    GhosttySnapshotCaptureEvent event = {.size = sizeof(event)};
    do {
        assert(ghostty_snapshot_capture_next(capture, &event) == GHOSTTY_SUCCESS);
    } while (event.kind != GHOSTTY_SNAPSHOT_CAPTURE_EVENT_READY);
    assert(ghostty_snapshot_capture_detach(capture) == GHOSTTY_SUCCESS);
    uint64_t released = now_ns();
    do {
        assert(ghostty_snapshot_capture_next_history(capture, terminal, &event) == GHOSTTY_SUCCESS);
        assert(event.kind != GHOSTTY_SNAPSHOT_CAPTURE_EVENT_INVALIDATED);
    } while (event.kind != GHOSTTY_SNAPSHOT_CAPTURE_EVENT_FINISH);
    uint64_t finished = now_ns();
    ghostty_snapshot_capture_free(capture);
    assert(sink.ready != 0);
    print_sample("fork-detached", &sink, released, finished, terminal, sample);
}
#endif

int main(int argc, char **argv) {
    if (argc != 6) {
        fprintf(stderr, "usage: %s LINES HISTORY_MIB DENSE IMAGES SAMPLES\n", argv[0]);
        return 2;
    }
    const size_t lines = strtoull(argv[1], NULL, 10);
    const size_t history_bytes = strtoull(argv[2], NULL, 10) * 1024 * 1024;
    const bool dense = strtoull(argv[3], NULL, 10) != 0;
    const size_t images = strtoull(argv[4], NULL, 10);
    const size_t samples = strtoull(argv[5], NULL, 10);
    GhosttyTerminal terminal = NULL;
    assert(ghostty_terminal_new(NULL, &terminal, 120, 40) == GHOSTTY_SUCCESS);
    assert(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &history_bytes) == GHOSTTY_SUCCESS);
    const size_t max_lines = lines + 100;
    assert(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &max_lines) == GHOSTTY_SUCCESS);
    const uint64_t image_limit = 64 * 1024 * 1024;
    assert(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &image_limit) == GHOSTTY_SUCCESS);
    fill_history(terminal, lines, dense);
    fill_images(terminal, images);
    for (size_t sample = 0; sample < samples; sample++) {
        measure_stock(terminal, sample);
#ifdef PHUX_CAPTURE
        measure_capture(terminal, sample);
#endif
    }
    ghostty_terminal_free(terminal);
    return 0;
}
