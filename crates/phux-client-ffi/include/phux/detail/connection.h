#ifndef PHUX_CLIENT_DETAIL_CONNECTION_H
#define PHUX_CLIENT_DETAIL_CONNECTION_H

/* Internal section of phux/client.h; include the umbrella header instead. */

/* ------------------------------------------------------- connected mode
 *
 * phux_client_new builds the EMBEDDED lane: the embedder owns the socket,
 * dials, reconnects, and pumps frames with feed_frame and outgoing_*.
 *
 * phux_client_connect builds the CONNECTED lane instead (ADR-0133). The
 * runtime resolves the target under the CLI's trust rules, dials, walks the
 * reconnect ladder, and reads and writes the socket on its own thread. An
 * embedder that takes this lane carries no dialer, no backoff constant and
 * no framing of its own.
 *
 * The DECODE POINT DOES NOT MOVE. This ABI's per-frame behavior --
 * retired-close suppression, bootstrap-profile validation, arrangement
 * reconciliation -- reads binding-owned catalog state that only the owning
 * thread may touch. So the driver retains inbound frames and
 * phux_client_poll feeds them from the owning thread, through the same path
 * feed_frame walks. The client keeps its owning-thread contract unchanged;
 * driver wakes run off-thread, while registration and explicit runtime
 * wakes may invoke the callback inline on the calling thread.
 *
 * Differences from the embedded lane, all of them enforced:
 *   - The runtime queues HELLO on every connection. Do NOT call
 *     phux_client_queue_hello; ATTACH stays explicit.
 *   - feed_frame returns PHUX_CLIENT_INVALID_STATE.
 *   - outgoing_count is always 0 and outgoing_get/clear have nothing to
 *     drain: the driver writes queued frames itself.
 *   - A reconnect discards any frame the consumer never polled, because it
 *     was built against the connection that ended.
 *   - The inbound queue is bounded at 128 frames and 32 MiB. A consumer
 *     that stops polling fails its connection rather than growing without
 *     limit, exactly as a socket-owning embedder had to bound itself.
 *
 * Additive to ABI version 2; no existing declaration changes.
 *
 * Linking is the remote tunnel's: on macOS the static archive references
 * CoreFoundation, and an AppKit application already links it.
 */

/** Notification that consumer-visible state changed. Driver wakes run on
 * the runtime's thread; listener registration and explicit runtime wakes
 * can invoke it synchronously on the calling thread, including BEFORE
 * phux_client_connect or phux_client_connect_captured returns a handle.
 * One outstanding wake covers any number of frames; the next
 * phux_client_poll re-arms it. No runtime lock is held during the callback.
 *
 * Fully initialize a thread-safe context BEFORE connecting. The callback
 * must asynchronously POST work to the owning event loop, never dispatch
 * it inline or re-enter this ABI (even on the owning thread). Poll only
 * after the connect call has returned a valid handle. Do not wait on an
 * embedder mutex held across connect: an inline wake would deadlock. Keep
 * callback/context valid until phux_client_free returns, after retiring the
 * listener and waiting out callbacks in flight. */
typedef void (*PhuxClientWakeCallback)(void *context);

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION, and
 * base the same way phux_client_new's options are initialized.
 * Exactly one of target and socket_path is non-empty.
 * target: a registry name or [USER@]HOST[:PORT], resolved as
 *   `phux attach --remote` resolves it, with the same trust rules -- a
 *   routable host needs its certificate pin, a routable WebSocket also
 *   needs wss:// and a token. ssh:// is refused. An unregistered host
 *   fails the call with a message naming the CLI command that pairs it.
 * socket_path: the local server's Unix-domain socket.
 * config_path: empty for the CLI's resolution, otherwise an absolute path.
 * client_name: the HELLO client name, at most 256 bytes.
 * wake: may be NULL, in which case the embedder polls on its own schedule.
 * wake_context: handed back to wake untouched; never dereferenced here.
 * Initialize it before this call; see PhuxClientWakeCallback for threading
 * and lifetime requirements. */
typedef struct PhuxConnectOptions {
    size_t size;
    uint32_t version;
    PhuxClientOptions base;
    PhuxBytes target;
    PhuxBytes socket_path;
    PhuxBytes config_path;
    PhuxBytes client_name;
    PhuxClientWakeCallback wake;
    void *wake_context;
} PhuxConnectOptions;

/** Start a session whose socket, reconnect ladder and framing belong to the
 * runtime. The dial begins at once on a runtime-owned thread; observe
 * phux_client_state and phux_client_last_error. Malformed arguments and an
 * unresolvable target fail the call; a reachable-but-down host does not,
 * because the ladder owns that.
 *
 * A wake may run inline before this call returns; see PhuxClientWakeCallback.
 * Free with phux_client_free, which retires the listener, waits out callbacks
 * in flight, stops the driver and JOINS its thread. Release the callback
 * context only AFTER free returns. The join is bounded by the consumer, not
 * the network: an in-flight dial is abandoned on close rather than run to
 * its timeout. */
PhuxClientResult phux_client_connect(const PhuxConnectOptions *options, PhuxClient **out_client);

/** Feed everything the driver has read since the last poll, then publish the
 * resulting effects, exactly as feed_frame does per frame. Call it from
 * owning-loop work posted by the wake, or on an owning-thread timer, only
 * after connect returns a valid handle. Never call inline from the wake.
 * PHUX_CLIENT_INVALID_STATE on an embedded client.
 * A contiguous run of terminal output is applied as one batch (at most 256
 * frames or 1 MiB), so a flood publishes each grid once per run, not once
 * per frame; effects and their order are those of per-frame feeding.
 * A protocol error in any frame stops that batch and returns; frames after
 * it are discarded with the connection. */
PhuxClientResult phux_client_poll(PhuxClient *client);

/** Cut a reconnect backoff short, or probe a socket that may have gone
 * stale after the app came to the foreground or the network path changed.
 * A no-op on the embedded lane. */
PhuxClientResult phux_client_nudge(PhuxClient *client);

/** Drop the socket and redial for fresh snapshots. A no-op on the embedded
 * lane, and never a reopen once the session is closed or failed. */
PhuxClientResult phux_client_resync(PhuxClient *client);

/** Why the session is failing or failed, as the runtime reports it.
 *
 * NOT phux_client_last_error, which is the bridge's own last refusal of an
 * ABI call. On the connected lane the dial, the trust rules and the
 * reconnect ladder live in the runtime, so their reasons are the only
 * account of why a host is unreachable -- a consumer that shows a failed
 * host has nothing else to show. Empty when there is nothing to report, and
 * always empty on the embedded lane, where the embedder's own transport
 * owns the reason. Borrowed until the next mutable call on this client. */
PhuxClientResult phux_client_connection_error(PhuxClient *client, PhuxBytes *out_error);

/** Whether phux_client_poll would find anything: a frame the driver has
 * read, or an event not yet drained. A consumer that polls its clients in
 * turn rather than acting on each wake uses this to skip an empty turn.
 * Always false on the embedded lane, which has nothing to poll. */
bool phux_client_poll_pending(const PhuxClient *client);

/** How many connections this client has opened; 0 before the first dial.
 *
 * An embedded client fences per-connection state by building a fresh
 * PhuxClient per connection. A connected client cannot: the runtime
 * reconnects underneath a handle that outlives every socket. Fence on this
 * instead -- a change retires the terminals, correlations and replicas the
 * previous connection built, exactly as replacing the client did. */
uint64_t phux_client_connection_epoch(const PhuxClient *client);

/** Whether this client's socket belongs to the runtime. False for a NULL
 * client and for every phux_client_new client. */
bool phux_client_is_connected(const PhuxClient *client);


#endif
