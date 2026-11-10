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
// Answers a frame's `paste` request with the system pasteboard's text.
void phux_rune_ios_paste(void *host, const char *text);
// The network path changed: rebind the connection and probe it now.
void phux_rune_ios_path_changed(void *host);
// One step of terminal text size, from a pinch.
void phux_rune_ios_zoom(void *host, bool larger);
void phux_rune_ios_active(void *host, bool active);
void phux_rune_ios_traits(void *host, bool dark, bool reduced_motion);
char *phux_rune_ios_report(void *host, const char *marker);
char *phux_rune_ios_tree(void *host);
// Read an error synchronously on the SAME OS THREAD as the failing call,
// before another bridge call or Swift actor/task hop. The slot is thread-local.
char *phux_rune_ios_error(void);
void phux_rune_ios_free(char *text);
