#ifndef PHUX_CLIENT_DETAIL_CLIENT_H
#define PHUX_CLIENT_DETAIL_CLIENT_H

/* Internal section of phux/client.h; include the umbrella header instead. */

/**
 * Process-wide diagnostics. Installs the bridge's tracing subscriber once per
 * process; every later call with a parsable filter is a no-op that returns
 * PHUX_CLIENT_OK, and a process that already has a Rust tracing subscriber
 * keeps it. Log lines are written to the process's standard error, so an
 * embedder that wants a file redirects descriptor 2 before calling. `filter`
 * is a RUST_LOG-style directive list (for example "phux=debug,quinn=info"),
 * UTF-8 without NUL, at most 4096 bytes; an empty span means the RUST_LOG
 * environment variable, else the bridge default
 * "phux=info,phux_client_ffi=info,phux_dial=info,quinn=warn,rustls=warn,warn",
 * which records the remote tunnel's lifecycle at info and everything else at
 * warn. An unparsable filter returns PHUX_CLIENT_INVALID_ARGUMENT and installs
 * nothing, on every call. Safe from any thread, but call it before
 * phux_client_new so the first bridge line is the log's first line.
 */
PhuxClientResult phux_client_log_init(PhuxBytes filter);

/**
 * Production artifacts that guarantee panic containment MUST be built with
 * `cargo build --profile ffi-release -p phux-client-ffi`; the workspace's
 * ordinary release profile aborts and is not a supported host-library build.
 * Every API then contains Rust panics and returns PHUX_CLIENT_PANIC. A client
 * and every pointer obtained from it are owning-thread-only. feed_frame borrows
 * input only for the call. Returned frame/effect/grid/search/selection buffers
 * are owned by the bridge and remain valid until the next mutable PhuxClient
 * call. Opaque document anchors remain valid until explicitly released or
 * their terminal generation is replaced or its presentation is cleared.
 * count/get/state/last_error and
 * terminal_mouse_tracking are read-only and do not invalidate borrowed
 * pointers; clear calls are mutable. Outbound caller-provided byte fields must
 * not exceed PHUX_CLIENT_MAX_OUTBOUND_BYTES.
 */
PhuxClientResult phux_client_new(const PhuxClientOptions *options, PhuxClient **out_client);
void phux_client_free(PhuxClient *client);
PhuxClientState phux_client_state(const PhuxClient *client);
PhuxClientResult phux_client_last_error(const PhuxClient *client, PhuxBytes *out_error);
PhuxClientResult phux_client_queue_hello(PhuxClient *client, PhuxBytes client_name);
PhuxClientResult phux_client_queue_attach(PhuxClient *client, const PhuxAttachOptions *options);
PhuxClientResult phux_client_queue_spawn(PhuxClient *client, const PhuxSpawnOptions *options);
/* Arm or send SUBSCRIBE_EVENTS with an optional journal cursor (ADR-0123).
 * Additive to ABI version 2. terminal NULL is the connection-wide scope the
 * automatic post-ATTACH_READY subscribe uses. after_seq NULL is live-only; a
 * non-NULL pointer is the cursor and is stored for that automatic subscribe
 * as well. A cursor requires EVENT_JOURNAL (0x01000000) or
 * PHUX_CLIENT_INVALID_STATE, nothing queued. Before ATTACH the call only
 * stores the cursor; after ATTACH it queues the frame immediately. Needs a
 * negotiated client that is not DETACHED. */
PhuxClientResult phux_client_subscribe_events(PhuxClient *client, const PhuxResourceId *terminal, const uint64_t *after_seq);
PhuxClientResult phux_client_queue_attach_resource(PhuxClient *client, const PhuxAttachResourceOptions *options);
/* Declare the role every later ATTACH and ATTACH_RESOURCE carries (ADR-0127).
 * Additive to ABI version 2. role_policy is the L1.md 8.1 byte: 0 the default
 * (sends nothing), 1 VIEWER (observe-only: the subscription's input is
 * refused), 2 PRIMARY with DELIBERATE takeover (attach and seize the input
 * lease). Any other value is PHUX_CLIENT_INVALID_ARGUMENT. A non-zero role
 * needs a negotiated client whose server advertised ATTACH_ROLES
 * (0x08000000), or PHUX_CLIENT_INVALID_STATE; nothing is stored on refusal.
 * A takeover is consumed by the next attach it rides; VIEWER stays declared. */
PhuxClientResult phux_client_attach_role(PhuxClient *client, uint8_t role_policy);

/* ---------------------------------------------------------- input lease
 *
 * ADR-0033, docs/spec/L1.md 5.1 and 8.1. Additive to ABI version 2. The
 * input lease is the one arbitration of who types: while a client holds it,
 * only that client's keys reach the PTY and everyone else's are acked and
 * dropped. Both verbs need only a negotiated client and are correlated like
 * the other operations (kinds PHUX_OPERATION_ACQUIRE_INPUT and
 * _RELEASE_INPUT; refusals carry error_domain protocol and the wire
 * ErrorCode). The resulting holder arrives separately, to every attached
 * client, as PHUX_CLIENT_STATUS_INPUT_HOLDER.
 *
 * phux_client_queue_acquire_input with seize false is cooperative: granted
 * while the wheel is free, refused INPUT_LEASE_HELD (204) while another
 * client drives. With seize true it preempts the holder, who stays attached
 * and learns of it from the same effect. A takeover is a deliberate user
 * act: never send it on focus, on a second attach, or on reconnect. A
 * subscription declared VIEWER (phux_client_attach_role) is refused
 * PERMISSION_DENIED.
 *
 * phux_client_queue_release_input returns the wheel to free; a no-op when
 * this connection does not hold it. The lease also ends with the
 * connection, so a host need not release on disconnect. */
PhuxClientResult phux_client_queue_acquire_input(PhuxClient *client, uint32_t request_id, const PhuxResourceId *terminal_id, bool seize);
PhuxClientResult phux_client_queue_release_input(PhuxClient *client, uint32_t request_id, const PhuxResourceId *terminal_id);

/* Withdraw a subscription, never kill durable work. Requires completed session
 * ATTACH and an admitted terminal without a pending attach/detach. Correlated
 * success retires client replica/admission, including initial participation.
 * Refusal retains state; disconnect yields unknown outcome. Never replay
 * automatically. Detach remains available when dynamic admission capacity is full.
 * Shares the monotonically increasing request IDs and bounded result queue.
 * While pending, input/resize are refused and automatic terminal sends are
 * fenced behind withdrawal. At most one unsent history request and latest ACK
 * are retained per detach, resumed only on refusal with a still-live exact
 * generation/cursor. Success/closure/disconnect discard them; input is never
 * retained or replayed. */
PhuxClientResult phux_client_queue_detach_resource(PhuxClient *client, const PhuxDetachResourceOptions *options);
size_t phux_client_operation_count(const PhuxClient *client);
PhuxClientResult phux_client_operation_get(const PhuxClient *client, size_t index, PhuxOperationResult *out_result);
/** Clears completions only, preserving pending correlation and stream admission. */
PhuxClientResult phux_client_operation_clear(PhuxClient *client);
/** Opaque HELLO_OK identity, borrowed until mutation and retained after disconnect. */
PhuxClientResult phux_client_server_id(const PhuxClient *client, PhuxBytes *out_id);
/** Call on transport loss: cancels pending requests as unknown outcome, discards
 * outgoing frames, and permanently detaches this client. Idempotent. */
PhuxClientResult phux_client_disconnect(PhuxClient *client);
PhuxClientResult phux_client_feed_frame(PhuxClient *client, const uint8_t *data, size_t len);
size_t phux_client_session_count(const PhuxClient *client);
PhuxClientResult phux_client_session_get(const PhuxClient *client, size_t index, PhuxSessionInfo *out_session);
/* Read-only resource catalog from the latest accepted ATTACHED or arrangement
 * GET_STATE (see PhuxResourceInfo). Arrangement refresh subscribes AgentSession
 * streams independently of terminal replica admission.
 * Zero before ATTACHED or for an invalid client. Spans borrowed until the next
 * mutable call. */
size_t phux_client_resource_count(const PhuxClient *client);
PhuxClientResult phux_client_resource_get(const PhuxClient *client, size_t index, PhuxResourceInfo *out_resource);

#endif
