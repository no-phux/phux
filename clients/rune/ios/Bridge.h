#pragma once
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>

// All host calls are on the UIKit main thread. The CAMetalLayer must outlive
// the host. Strings are UTF-8, NUL-terminated and borrowed for the call only.
// Result strings are owned and freed exactly once with phux_rune_ios_free.
// create takes ownership of context even on failure: release is called when
// the last runtime waker retires. wake/release may run on a worker thread.
typedef void (*PhuxRuneCallback)(void *context);
// Warm the shared GPU device and renderer off the main thread; idempotent.
void phux_rune_ios_prewarm(void);
// Simulator qualification only. Creates a new private, payload-free JSONL trace;
// refuses an existing subscriber/file. Ordinary/device launches never call it.
bool phux_rune_ios_capture_connection_diagnostics(const char *path);
// Final qualification background callback only. Seals capture and exclusively
// publishes its completion/loss receipt before the collector terminates the app.
bool phux_rune_ios_finish_connection_diagnostics(void);
char *phux_rune_ios_validate(const char *configuration);
// Parse a complete pairing link. Secrets remain transient; the result is
// staged for explicit peer confirmation, not saved or dialed by this call.
char *phux_rune_ios_pairing_import(const char *link);
void *phux_rune_ios_create(void *layer, double width, double height, double scale,
                          bool dark, const char *configuration, void *context,
                          PhuxRuneCallback wake, PhuxRuneCallback release, void *identity);
// Identity calls need no UIKit/Metal and may run on a caller-owned worker.
// create takes context even on failure. It retains the immutable signer until
// its last Arc retires (which can be after identity_destroy or host_destroy).
// sign/release must be thread-safe. sign returns DER signature length, or a
// negative result on failure; never write beyond capacity (currently 80).
typedef intptr_t (*PhuxRuneSign)(void *context, const uint8_t *message, size_t length,
                                uint8_t *signature, size_t capacity);
void *phux_rune_ios_identity_create(const uint8_t *point, size_t length, void *context,
                                   PhuxRuneSign sign, PhuxRuneCallback release);
// Callers must not destroy a wrapper during a call borrowing it. Host creation
// clones its signer; the wrapper may then be destroyed independently.
void phux_rune_ios_identity_destroy(void *identity);
char *phux_rune_ios_identity_validate(const char *configuration, void *identity);
char *phux_rune_ios_enrollment_validate(const char *configuration);
// BLOCKING: call off the main thread. Returns owned JSON {"chain_pem":...}.
// A one-use ticket is borrowed for this call only and never stored or logged.
char *phux_rune_ios_enroll(const char *configuration, const char *ticket, void *identity);
void phux_rune_ios_destroy(void *host);
void phux_rune_ios_resize(void *host, double width, double height, double scale);
char *phux_rune_ios_frame(void *host);
char *phux_rune_ios_input(void *host, const char *operation);
void phux_rune_ios_touch(void *host, uint64_t identity, uint32_t phase,
                         double x, double y, double seconds);
// A tap on the frame's native control `id` (its `controls`): runs that
// control's press, as Rune's accessibility press would. False when none ran.
bool phux_rune_ios_press(void *host, const char *id);
// A tapped card, widget or Dynamic Island: `phux://inbox` or
// `phux://pane/@N`. False when the link is neither.
bool phux_rune_ios_open_link(void *host, const char *url);
// Where the phone's Live Activity gateway takes agent counts, and its grant
// there: written to every computer it attaches to.
void phux_rune_ios_set_live(void *host, const char *device, const char *gateway, const char *grant);
// Answers a frame's `paste` request with the system pasteboard's text.
void phux_rune_ios_paste(void *host, const char *text);
// One picked file for a frame's `attaching` request, borrowed for this call.
// Null data: it could not be read, which is said rather than dropped.
void phux_rune_ios_attach(void *host, uint64_t id, const char *extension,
                          const uint8_t *data, size_t length);
// The network path changed: rebind the connection and probe it now.
void phux_rune_ios_path_changed(void *host);
// One step of terminal text size, from a pinch.
void phux_rune_ios_zoom(void *host, bool larger);
void phux_rune_ios_active(void *host, bool active);
// The system's appearance and accessibility settings. content_size is a
// UIContentSizeCategory raw value; an unknown one reads as the default.
void phux_rune_ios_traits(void *host, bool dark, bool reduced_motion, bool increased_contrast,
                          bool reduce_transparency, const char *content_size);
// Qualification log: the probe observed input_ready and is about to type.
void phux_rune_ios_trace_input_ready(void *host);
char *phux_rune_ios_report(void *host, const char *marker);
char *phux_rune_ios_tree(void *host);
// Read an error synchronously on the SAME OS THREAD as the failing call,
// before another bridge call or Swift actor/task hop. The slot is thread-local.
char *phux_rune_ios_error(void);
void phux_rune_ios_free(char *text);
