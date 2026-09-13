---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-12
---

# Protocol optimization audit

**TL;DR.** The per-Terminal QUIC transition exposes consumer-identity,
compatibility, lifecycle, and backpressure gaps. The native snapshot update
also defeats progressive history. Correct those boundaries first, then
measure batching, copies, compression, and connection reuse. This source
audit ranks the findings and supplies acceptance experiments; it does not
claim measured speedups or a reproduced security exploit.

## Scope and evidence

This is a source audit of the checkout immediately after the per-Terminal
QUIC and official snapshot-codec updates, on 2026-09-12. It covers direct
QUIC, connector/relay, federation, UDS, WebSocket, WebTransport, native client,
browser/FFI transport boundaries, framing, bootstrap, recovery, input, and
observability. External mobile implementations are outside this checkout.

Findings describe code paths, not measured speedups. No WAN load experiment
or live deployment inspection was performed. Arithmetic bounds are labeled
as such; historical measurements are not treated as current baselines.
Task status and implementation sequencing live in Beads, not this document.

### Remediation evidence (2026-09-13)

The [integration validation report](2026-09-13-protocol-integration-validation.md)
records the implemented correctness fixes, actual gates, review findings and
measurement limits. Original source references below remain the audit snapshot.

| Audit area | Implemented acceptance |
|---|---|
| QUIC identity and lifecycle | Explicit bilateral opt-in; bounded ingress and provenance; generation-fenced bind/rebind/FIN; cancellation-safe mux events; bounded writer drain/reset; actual stream-credit isolation |
| Relay and federation | Relay owns tunnel acceptance with explicit single-stream fallback; generation/connection-fenced proxy retirement; retained authoritative close; local input fast path preserves satellite routing |
| Input and control | Ordered synchronous input admission, owned bounded receipts, uncertain failed-frame replay; bounded PUT_FILE/Transcribe FIFO; same-connection real QUIC control/input progress during a held transcriber |
| Native snapshots | Official V1 codec, detached progressive READY/history, aggregate staging budgets, authenticated FINISH with discarded rows, Busy retry limits, resize/reattach/expiry cleanup and real socket-to-C-ABI acceptance |
| Browser and authentication | Bounded framing/queues, absolute recovery deadlines, authenticated WS subprotocol, WT failure fallback to WSS, explicit mTLS fail-closed initialization |
| Measured hot paths | Reused ACK render cache and encoder scratch, in-place ACK pruning, persistent polling, shared congestion tracker and mux-aware draining |
| Observability | Bounded per-stream ingress/write/lifecycle diagnostics retained by GET_PERF and CLI JSON/watch; overflow and suppressed registration accounting |

Compression policy, 0-RTT, broader WebTransport multi-stream support and wire
format replacement remain measurement/design candidates, as scoped below. The
legal-ceiling upload experiment does not change shipping-client chunk policy.
Lowercase duplicate Authorization headers collapsed before application
inspection remain separately tracked as `phux-50wm`; live mTLS registry
revocation remains `phux-pjc5.5`.

## Priority summary

| Priority | Finding | Evidence confidence |
|---|---|---|
| P0 | Relay tunnel streams lack authenticated consumer grouping | Ownership loss follows from code; cross-consumer exposure needs an isolated reproduction |
| P1 | Server activates multi-stream without client opt-in; hub and FFI remain single-stream | Producer/consumer mismatch |
| P1 | Native bootstrap sends full history before READY, then ignores pulled history | Producer/consumer mismatch |
| P1 | Malformed frames, refused binds, and incomplete bootstraps lack aggregate bounds | Specific allocation and error-handling paths |
| P1 | Cross-stream attach/FIN/rebootstrap ordering and teardown are incomplete | Specific lifecycle paths; schedule-dependent manifestations |
| P1 | Replayed input can be called safe to retype; queued pastes can be overtaken | Admission, dedupe, and client queue trace |
| P1 | Explicit mTLS configuration errors fall back to bearer-only | Startup error-handling path |
| P1 | Shared dispatch and saturated hub mailboxes defeat isolation/recovery | Await and removal paths |
| P1/P2 | Browser admission, buffering, and fallback need bounded outcomes | Server and browser caller traces |
| P2 | Mux batching, shared window tracking, copy reduction, ACK work, connection reuse | Redundant-work paths; performance gain unmeasured |

P0/P1 indicate remediation priority, not a claim that a live deployment was
exploited or that every user takes the affected route.

## Route ownership and capability negotiation

### Relay consumer ownership is lost

The relay opens a fresh stream on a shared route tunnel for every consumer
stream ([relay runtime](../crates/phux-relay/src/runtime.rs), lines 497–558).
It carries no consumer-group association. The production connector accepts
and bearer-authenticates one stream, then constructs a `QuicMuxReader` over
the entire tunnel connection
([connector.rs](../crates/phux-server/src/connector.rs), lines 213–278).
That mux and the connector admission loop both call `accept_bi()` on the
same connection ([quic.rs](../crates/phux-server/src/transport/quic.rs),
lines 575–633). Additional consumers create additional competing acceptors.

A Terminal bind can reach bearer admission, a new control stream can reach
an existing consumer's bind parser, and a bind can be evaluated using another
consumer's subscription registry. The latter is a potential authorization
boundary failure: possession of a tunnel stream does not identify the
consumer whose credentials should authorize it. The audit did not execute
an exploit. Correct grouping must precede scheduling or throughput tuning.

**Acceptance experiment:** production relay plus production connector,
two distinguishable consumer credentials, separate and shared subscriptions,
interleaved binds, reconnects, and a consumer without a valid server bearer.
Assert every tunnel stream resolves to exactly its authenticated consumer;
no bootstrap or input crosses that identity boundary. Use disposable
terminals. The existing echo/stub connectors do not exercise this ownership
path. Tracked work: phux-au1s.4.

### Multi-stream activation has no client consent

The server sets `QUIC_STREAMS` for every QUIC transport
([client.rs](../crates/phux-server/src/runtime/client.rs), lines 1962–1969)
and uses that bit to defer attach bootstrap until a bind. No client offer
confirms support. An older same-protocol client can ignore the unknown bit
and wait for content forever while the server waits for `STREAM_BIND`.

Two current production callers already have this shape:

- The hub's QUIC link retains one send/receive pair
  ([link.rs](../crates/phux-server/src/hub/link.rs), lines 1248–1258,
  1330–1349, 1375–1395).
- The FFI remote tunnel copies its embedder socket onto one QUIC stream
  ([pump.rs](../crates/phux-client-ffi/src/remote/pump.rs), lines 145–170).
  Cockpit uses this tunnel; a raw byte-copy adapter cannot represent
  independent streams without an additional stream-aware layer.

There is another federation gap in the opposite direction: a QUIC consumer
binding a satellite resource is explicitly refused as `ResolvedOwned::Remote`
([client.rs](../crates/phux-server/src/runtime/client.rs), lines 2932–2955).

**Acceptance experiment:** previous same-protocol client, current TUI,
FFI/Cockpit, hub-to-QUIC-satellite, and QUIC-consumer-to-hub-to-satellite.
Require actual BEGIN/READY, live output, and input echo. HELLO or an attach
command acknowledgment alone is insufficient. Add bilateral opt-in before
changing shape and define the stream-aware adapters each route needs.

## Native bootstrap and fidelity

### Full history is on the first-paint path

`encode_cut()` encodes the full engine snapshot, locates the engine READY
offset, then assigns the entire encoded blob to `prefix` and its post-READY
tail to `suffix`
([native_state.rs](../crates/phux-server/src/native_state.rs), lines 178–200).
Both share the source allocation, but the full prefix is sent as bootstrap
chunks and the suffix is offered again through history pages.

The real client adapter collects all bootstrap bytes, decodes at protocol
READY, and walks the decoder through the collected history
([ghostty.rs](../crates/phux-client-core/src/engine/ghostty.rs), lines
1190–1259). Its `push_history()` ignores `_input` and always reports retained,
finished history (lines 1262–1275). The kernel still requests history and
rejects `Finished` when a page has a next cursor
([session.rs](../crates/phux-client-core/src/session.rs), lines 2284–2327,
2579–2605). Thus a multi-page history reply triggers a completion mismatch.

The visible history may already be present from the initial blob; the
finding is redundant transfer, full-history startup work, and a broken
progressive-history contract, not that all clients necessarily display an
empty scrollback. A true prefix-only peer also cannot import its later pages
through the adapter's ignored-input implementation.

**Acceptance experiment:** real encoder, real wire, real GhosttyAdapter,
real SessionKernel; fixed viewport with growing history and at least two
history pages. Measure bytes before protocol READY, first publication,
peak memory, imported older rows, and input during paging. Repair this at
the engine-owned incremental API boundary. Treat decoder `InvalidValue`
termination at lines 1228–1237 as a separate corruption-review target;
do not substitute a new handwritten snapshot parser.

### Staging bounds are applied too late or only per chunk

`push_native()` extends an aggregate buffer without a total-byte/chunk limit
(lines 1190–1202). The kernel checks sequence and per-chunk bounds, which do
not bound an endless valid-sized sequence without READY. The successful
native finish path retains the encoded feed alongside the decoded terminal.
On the server, full encoding precedes the prefix-byte limit check
([native_state.rs](../crates/phux-server/src/native_state.rs), lines 774–778).
Chunk stepping after this encode does not make the encode cooperative.

**Acceptance experiment:** enforce per-generation and connection-wide staging
bytes, chunk counts, and an incomplete-generation deadline before allocation.
Test many legal chunks without READY. Measure actual peak allocation and
uninterrupted capture time, then memory after publication. Release buffers
once their decoder no longer requires them.

### Native live bytes still pass through capability rewriting

Both the session output constructor
([attach.rs](../crates/phux-server/src/runtime/attach.rs), lines 558–570)
and resource output path
([commands.rs](../crates/phux-server/src/runtime/commands.rs), lines 2036–2041)
call `downsample_for_caps()` without testing the selected native profile.
Native selection forces raw output mode but preserves other capability fields.
Restricted colors/images/hyperlinks can therefore change the native live
bytes after an exact checkpoint initialized the replica.

**Acceptance experiment:** native negotiation with restrictive presentation
capabilities, then RGB, OSC hyperlinks, images, and keyboard-protocol escapes,
including fragmented escapes. Assert byte-identical output for session attach,
resource attach, and resync replay. Gate pass-through on the selected native
profile. This both preserves fidelity and avoids unnecessary byte scanning.

## QUIC framing, admission, and lifecycle

| Finding | Source evidence | Required acceptance |
|---|---|---|
| Invalid client Terminal-stream framing is treated as incomplete input; the invalid prefix remains while more bytes accumulate | [connection.rs](../crates/phux-client/src/attach/connection.rs), 200–221 | Zero/oversized prefix followed by bounded extra data fails promptly; partial-frame EOF is an error; retained bytes stay bounded |
| Server starts the receive pump before bind admission; refusal resets only the send half | [quic.rs](../crates/phux-server/src/transport/quic.rs), 449–455, 615–665 | Reject/stop both directions; do not read payload before admission; supervised tasks end on refusal |
| Frame headers reserve the full declared body before it arrives | [framing.rs](../crates/phux-protocol/src/wire/framing.rs), 141–157; [quic.rs](../crates/phux-server/src/transport/quic.rs), 706–717 | Connection-wide byte accounting and staged reads bound aggregate incomplete-frame memory |
| Bound-stream provenance is discarded into a shared `BytesMut` queue | [quic.rs](../crates/phux-server/src/transport/quic.rs), 640–664 | Retain origin and validate terminal, generation, and permitted frame kind before dispatch |
| TUI sends ATTACH_RESOURCE then binds immediately on another stream | [loop_state.rs](../crates/phux-tui/src/attach/driver/loop_state.rs), 2617–2666 | Delay control dispatch; binding waits for successful subscription acknowledgment or uses bounded pending admission |
| FIN notifications can overtake their preceding queued input | [quic.rs](../crates/phux-server/src/transport/quic.rs), 647–664; [client.rs](../crates/phux-server/src/runtime/client.rs), 2306–2339 | Distinctive input immediately followed by FIN is handled before detach |
| Closed event receiver returns `None`; biased select immediately continues on that receiver | [client.rs](../crates/phux-server/src/runtime/client.rs), 2315–2339 | Disable the closed arm; handler, accounting, leases, and subscriptions finish after EOF |
| Recovery ATTACH does not replace existing bindings; `bind_terminal()` returns early when already bound | [loop_state.rs](../crates/phux-tui/src/attach/driver/loop_state.rs), 2599–2611; [connection.rs](../crates/phux-client/src/attach/connection.rs), 746–755 | Force engine recovery and require a fresh generation/READY |
| Resource death does not retire the separately owned server stream binding | [client.rs](../crates/phux-server/src/runtime/client.rs), 908–915, 1645–1649, 1747–1754 | Close panes while retaining the session; tasks, bindings, and credit return to baseline |
| Server cap includes control; client allows 128 Terminal bindings and awaits credit without a deadline | [quic.rs](../crates/phux-server/src/transport/quic.rs), 61–68, 185–186; [connection.rs](../crates/phux-client/src/attach/connection.rs), 757–788 | Explicit outcomes at 127/128 panes; stream-credit exhaustion cannot park attach indefinitely |
| Relay cap counts lifetime opens, never decrementing on stream completion | [relay runtime](../crates/phux-relay/src/runtime.rs), 536–558 | More than 128 sequential bind/unbind cycles with one active pane remain connected |
| Relay `open_bi()` is outside its admission timeout and inside a select arm | [relay runtime](../crates/phux-relay/src/runtime.rs), 509–558 | Exhaust tunnel credit, then close control; all splice tasks and permits end within a deadline |

The incomplete server-frame exposure scales with stream count: 127 advertised
16 MiB bodies represent 2,032 MiB before other buffers. This is arithmetic,
not measured RSS. Start its regression test with four refused streams in a
memory-limited disposable server, never with the theoretical maximum.

Additional conformance checks: preserve session ATTACH scrollback settings
when bootstrap moves to STREAM_BIND; the current resource-bootstrap path
uses `scrollback: None` and unbounded session-independent capture budgets
([commands.rs](../crates/phux-server/src/runtime/commands.rs), 1474–1485,
1756–1764). AgentSession binding must use the bound logical StreamId; the
agent-session branch derives one from client identity instead
([resource_commands.rs](../crates/phux-server/src/runtime/resource_commands.rs),
491–514). Test these separately from Terminal-only happy paths.

## Input replay and ordering

**High impact; operation-level delivery cannot be inferred from retry refusal.**
Server `apply_input()` tries to reserve the terminal before dedupe lookup
([input lane](../crates/phux-server/src/runtime/input_lane/mod.rs), 211–251,
649–669). Reconnecting while an earlier attempt remains unresolved can make
a same-ID retry return `RESOURCE_EXHAUSTED`. The client treats every error
except `INPUT_DELIVERY_UNKNOWN` as refused and presents “safe to retype”
([input_replay.rs](../crates/phux-client/src/attach/input_replay.rs), 125–129,
305–348). The first attempt may already have written. A different operation
holding admission can also prevent retrieval of a cached result.

**Acceptance experiment:** controlled PTY writer; disconnect after handoff,
retry before completion, then retry a completed ID while another operation
owns admission. Require at most one write and no safe-to-retype verdict while
an earlier attempt remains uncertain. Consult operation identity before
classifying a retry; preserve bounded admission and dedupe retention.

The journal also has one connection-wide in-flight operation and an unbounded
local queue ([input_replay.rs](../crates/phux-client/src/attach/input_replay.rs),
231–268). TUI pastes enter it but keys send immediately
([dispatch.rs](../crates/phux-tui/src/attach/input_dispatch/dispatch.rs),
1148–1182). With paste A in flight, paste B queued locally, and Enter sent
next, the server cannot order B before Enter: B has not been sent. Per-stream
QUIC ordering cannot recover application submission order across control and
Terminal streams either.

**Acceptance experiment:** delay A's result and assert PTY order A/B/Enter.
Bound journal bytes and events, define overflow disposition, and preserve
per-terminal submission order while allowing independent terminals to progress.

## Explicit mTLS configuration must fail closed

When explicitly enabled, workload CA or registry failures return `None`
([runtime mod](../crates/phux-server/src/runtime/mod.rs), 1816–1844).
The caller converts failure to absent mTLS configuration and still constructs
the QUIC listener (1973–1985), potentially serving bearer-only access.
An unavailable requested authentication mechanism must disable/refuse that
listener, rather than silently choose the weaker mode.

**Acceptance experiment:** isolated server, valid bearer store, explicit mTLS,
malformed/unreadable registry and partial CA material. A client without a
workload certificate must not gain admission. Assert listener status reports
the precise configuration failure.

Scope enforcement and live revocation are separately unfinished, as the
[workload spec](../docs/spec/workload-auth.md), lines 21–26, acknowledges.
Certificate identity includes scopes, but HELLO currently discards granted
capabilities and stream admission relies on subscriptions
([client.rs](../crates/phux-server/src/runtime/client.rs), 2026–2042,
2932–2955). Do not count mTLS key-possession proof as completed authorization.
The existing scope-enforcement work remains the owner of that gap.

## Shared dispatch can defeat stream isolation

**High impact; source-proven serialization.** The production connection loop
awaits `handle_command` before dispatching another received frame
([client.rs](../crates/phux-server/src/runtime/client.rs), lines 2769–2813).
`TRANSCRIBE` awaits an external process through that path
([commands.rs](../crates/phux-server/src/runtime/commands.rs), lines 904–909;
[voice.rs](../crates/phux-server/src/runtime/voice.rs), lines 30–55, 99–105).
Its default deadline is 30 seconds. This suspends that connection's command
and input dispatch, even when input arrived on another QUIC stream. Other
connections and already-running output tasks can continue; it is not a
30-second block of the whole server runtime.

`PUT_FILE` correctly moves disk work to `spawn_blocking`, but the connection
still awaits its completion
([upload.rs](../crates/phux-server/src/runtime/upload.rs), lines 39–86).
Request correlation already exists. The design opportunity is bounded
in-flight work for slow commands, with explicit ordering for operations
that mutate the same resource and cancellation tied to connection lifetime.
Spawning every command concurrently would lose those semantics.

Tracked work: phux-jyt5.

**Acceptance experiment:** configure an isolated server with a transcriber
that waits two seconds. On one connection, issue transcription, then a
control ping and input to a second quiet terminal. Record dispatch and echo
latency before transcription completes. Repeat with a slow upload worker,
disconnect, and resource teardown while commands are pending.

Bind bootstrap and history have the same coupling: the shared connection
loop awaits `handle_stream_event()`, which awaits bootstrap, while history
waits for a permit on the addressed stream's eight-entry mailbox
([client.rs](../crates/phux-server/src/runtime/client.rs), 2315–2332,
2979–2992, 2180–2187). A stopped reader can block unrelated input/control
even though it has its own QUIC writer. Run the same acceptance experiment
with one unread Terminal stream and a history request whose mailbox is full.

The muxes also share 64-frame receive queues across terminals
([server mux](../crates/phux-server/src/transport/quic.rs), 432–443, 557–578;
[client mux](../crates/phux-client/src/attach/connection.rs), 137–174).
Frame-count bounds need aggregate byte limits and fair service; a shared
queue can otherwise propagate one producer's backlog to all of them.

## Federation backpressure can permanently stop a live subscription

The hub removes proxy subscribers on any failed ordinary `try_send`, including
`Full`, without a tombstone or cancellation of the still-live downstream
connection ([relay.rs](../crates/phux-server/src/hub/relay.rs), 2188–2227,
2335–2343). This is permanent subscription removal, not one dropped frame
followed by recovery. The link's single read/write dispatcher also awaits
individual sends, with up to ten seconds per send
([link.rs](../crates/phux-server/src/hub/link.rs), 850–957).

Input/ACK forwarding onto the link's 64-request mailbox is documented
best-effort. It deserves explicit drop accounting, but is distinct from the
silent live-subscriber removal above. Retained bootstrap has useful byte/frame
bounds; without new traffic its retry can wait for ten-second housekeeping
([relay.rs](../crates/phux-server/src/hub/relay.rs), 85–100, 2392–2408;
[link.rs](../crates/phux-server/src/hub/link.rs), 904–919).

**Acceptance experiment:** make one downstream mailbox full for one delivery,
drain it, then continue satellite output. Require convergence or an explicit
resource-scoped failure. Block one outbound link write while receiving output;
measure inbound stalls, control latency, traffic-class drops, and time to
resume retained bootstrap. Separate read/write driving and capacity-triggered
retry are candidates after correctness is pinned. Tracked work: phux-yxxv.

## Browser and WebTransport

### WebTransport admission is serial and unbounded

Server establishment awaits the HTTP/3 request, session accept, and first
bidi stream without an application deadline
([webtransport.rs](../crates/phux-server/src/transport/webtransport.rs),
117–157). Its accept loop waits for each establishment before the next
(266–275). A transport-live session that never opens the phux stream can
block subsequent WebTransport consumers. Other transport listeners have
separate accept futures; this finding is scoped to WebTransport.

QUIC raw admission is deadline-bounded, but it too serializes establishment;
Terminal-stream admission waits up to ten seconds for each bind before
reading the next ([quic.rs](../crates/phux-server/src/transport/quic.rs),
287–338, 603–613). Deadlines bound one wait; bounded concurrency isolates it.

**Acceptance experiment:** A establishes but withholds its stream/bind; B
sends a complete valid handshake/bind. B must progress promptly while A
times out. Keep admission concurrency, resources, and teardown bounded.
Tracked work: phux-woo7 and phux-au1s.14.

### Outbound queues and fallback lack a complete failure contract

Browser sends ignore WebSocket errors and enqueue WebTransport writes with
one asynchronous promise waiter each, without a readiness or byte-budget
policy ([client.rs](../clients/phux-web/src/client.rs), 330–345).
Transport backpressure can therefore become JS queue growth and silent
input-send failure rather than a bounded application outcome.

WebTransport readiness has no application deadline (173–184); fallback
happens only after initial setup returns an error (141–157). WebSocket setup
returns after installing callbacks rather than after HELLO, and lacks a
corresponding close/error reconnect lifecycle (70–128). On the direct
authenticated fallback, `WebSocket::new(url)` cannot supply the bearer header
required by the server
([transport.rs](../crates/phux-server/src/transport.rs), 778–791).
A WebTransport query token alone does not provide authenticated WSS fallback.
An authenticating deployment proxy can change that situation; none was
assumed or inspected in this audit.

**Acceptance experiment:** blackholed WT, unavailable WT, valid credentials
through direct WSS fallback, post-connect loss, suspend/resume, and throttled
upload. Require an observed protocol-ready outcome or explicit failure;
measure queued bytes, outstanding promises, JS/WASM heap, and input delay.
Tracked work: phux-42sg.

**Remediation evidence (2026-09-13):** the integrated browser uses bounded
outbound admission, supervised reader/writer lifetime, protocol-ready
establishment, and authenticated WSS subprotocol fallback. The parent reran
18 Chrome lifecycle/framing tests, two live-server compatibility tests, one
render test, and 12 Node session tests. A separate real `run_with_fallback`
experiment discarded six QUIC datagrams into an owned UDP blackhole, then
reached READY and rendered `PHUX_WEB_OK` over token-authenticated WSS in
4.15 seconds. Wrong/missing tokens were refused; the WSS URL carried no token
and the negotiated subprotocol was only `phux.v1`. The reproducible fixture is
[`scripts/ci/web-browser.py`](../scripts/ci/web-browser.py).

The serial production WebTransport suite passed eight tests, including an
authenticated consumer withholding its first stream while a second consumer
connects. Raw duplicate header detection has a dependency boundary:
`wtransport` collapses identical header names into a map before application
inspection. Application rejection of visible ambiguous carriers does not
prove rejection of every raw duplicate; that remaining dependency work is
tracked as phux-50wm.

### Browser and Cockpit retain shared application scheduling

WebTransport framing repeatedly copies its remaining tail with `Vec::split_off`
([framing.rs](../clients/phux-web/src/framing.rs), 60–62). Browser frame
handling can paint each frame immediately, and JS/WASM conversion copies
payloads ([client.rs](../clients/phux-web/src/client.rs), 109, 255–268,
337, 533–548). Measure equal byte volumes split into many small frames versus
large frames; cursor-based framing and paint coalescing are candidates.

Cockpit already has bounded 128-frame/32 MiB bridge queues and concurrent
FFI transport halves. Its socket worker still flushes up to 16 complete
outbound frames ahead of normal inbound work, with a one-second write budget
per frame ([extension.zig](../clients/cockpit/src/providers/phux/extension.zig),
241–247, 328–338, 368–371). All panes share those budgets. After restoring
FFI QUIC compatibility, measure a large paste on A alongside output and
timestamped keys on B, including queue residence, overflow, and reconnects.

## Recovery deadlines and connection reuse

The raw-output pump constructs a new retry timeout for every received event
([pump.rs](../crates/phux-server/src/runtime/pump.rs), 337–350). Continuous
live events discarded while fenced can keep restarting that timeout, so
the advertised recovery budget becomes an inactivity budget. The attempt
limit is also checked before awaiting a response to the final request
(241–249). Use absolute retry/overall deadlines and a final response window.

**Acceptance experiment:** paused time, continuous fenced output, suppressed
resync response, and separately a response to the last permitted request.
Verify bounded frozen duration and unaffected neighboring consumers.
Tracked work: phux-qobt.

The attach reconnect loop checks its 60-second remote deadline only after
the full dial/HELLO/shutdown attempt
([attach command](../crates/phux/src/commands/attach.rs), 686–722). A peer
that remains transport-live but never answers HELLO can exceed that bound.
Successful remote probes are then shut down and the real attach reconnects,
paying setup again. Carry an absolute deadline through the entire attempt
and return a successful negotiated connection for use by attach.
Tracked work: phux-1vob.

**Remediation evidence (2026-09-13):** remote reconnect now retains the
successful negotiated connection and transfers it into the TUI attach entry.
Both initial attach and reconnect use `connect_for_attach`, preserving the
same HELLO capability contract. The outer absolute deadline covers each probe.
The parent ran all 15 CLI attach tests, including a real WebSocket peer that
compares initial/reconnect HELLO frames and accepts PING on each negotiated
connection without a second HELLO, plus a peer that accepts transport but
withholds protocol negotiation. The local UDS readiness policy remains covered
by the same suite.

Current dialing constructs fresh endpoint/TLS configuration and awaits the
handshake; no explicit application 0-RTT path is implemented
([quic dial](../crates/phux-dial/src/quic.rs), 181–207). Passive NAT rebinding
can still work through quinn; lack of an application rebind call does not
prove otherwise. Test NAT/interface/family changes, suspend/resume, and full
outage separately. First-address-only DNS dialing and deterministic backoff
are further measurement candidates for dual-stack failure and fleet recovery.

## Wire and scheduling optimization opportunities

| Opportunity | Concrete current work | Measurement and constraints |
|---|---|---|
| Restore mux-aware burst draining | `try_recv()` reads control only while `recv()` also reads Terminal queues: [connection.rs](../crates/phux-client/src/attach/connection.rs), 860–907 | Prequeue 32 Terminal frames and drain; measure frames/batch, loop turns, CPU and echo p99. The paint pacer means one loop turn is not necessarily one physical paint |
| Share one congestion tracker per connection | New independent `SendWindow` instances for Terminal writers: [client.rs](../crates/phux-server/src/runtime/client.rs), 2957–2961; relay runtime 533/557 | Clone the shared tracker; measure actual setter calls and window changes during loss/growth. Preserve the cwnd-plus-slack policy |
| Remove small TLV scratch allocations | Every `write_field_with()` creates a temporary BytesMut: [encode.rs](../crates/phux-protocol/src/wire/encode.rs), 192–201 | Fixed-width field writers with byte-identical golden tests; count allocations/frame. Opaque payloads already use direct field writes |
| Slice owned receive buffers | Decoder copies opaque bytes from borrowed input: [decode.rs](../crates/phux-protocol/src/wire/decode.rs), 684, 1010, 1159–1161 | Owned-buffer decode can preserve Bytes slices; measure copy bytes and retained backing slabs, including compressed input |
| Avoid native staging copies | Owned prefix copied through scratch, then into payload: [native_state.rs](../crates/phux-server/src/native_state.rs), 318; [native.rs](../crates/phux-server/src/resource/terminal/native.rs), 247–266 | Preserve engine-owned slices where lifetimes permit; measure end-to-end allocation rather than broadcast clone cost alone |
| Reduce acknowledged-input hashing copies | Event clone and full canonical command serialization: [input lane](../crates/phux-server/src/runtime/input_lane/mod.rs), 649; [acknowledged.rs](../crates/phux-server/src/runtime/input_lane/acknowledged.rs), 238–253 | Hash borrowed canonical fields incrementally; preserve exact dedupe digest semantics |
| Validate and coalesce StateSync ACK work | Every accepted ACK advances the watermark and creates a RenderState cursor/mode capture: [consumers.rs](../crates/phux-server/src/resource/terminal/consumers.rs), 303–375, 445–472 | Reject ACKs beyond emitted sequence; measure cumulative ACK coalescing, allocations, RTT/cadence semantics. Native/raw correctly do not ACK |
| Release ready panes independently on single-stream attach | Captures and staged publication are serial; live gates open after aggregate publication: [attach.rs](../crates/phux-server/src/runtime/attach.rs), 3424–3545, 3761–3794 | One small pane beside history-heavy pane: record each READY/first-live and staging memory. Preserve atomic preflight rollback and generation fences |
| Tune existing compression by traffic class | Writer compresses bootstrap/history and excludes live output: [client.rs](../crates/phux-server/src/runtime/client.rs), 4388–4413 | Compare CPU, encoded bytes and p99 under realistic RTT/bandwidth; preserve small-frame bypass, inflation bounds and negotiation. Broader compression is a hypothesis |

Existing write batching and byte-sharing are valuable foundations. The
server already coalesces writes; adding another timer-based batcher can
increase typing latency. No table entry is a measured speedup estimate.

**Implementation evidence (2026-09-13):** phux-au1s.18 reuses encoder-local
TLV scratch while preserving zero-based builder views and wire goldens.
Measured ACK and 1 KiB output frame allocations fall from four to one per
frame; Ping remains one. See the
[wire encoding measurement report](2026-09-13-wire-encoding-measurements.md).

## Control traffic includes bulk payloads

**Medium-to-high impact on thin paths; source-proven framing exposure.**
All `COMMAND` frames stay on control, including `PUT_FILE`. A legal upload
chunk can carry 8 MiB
([frame constants](../crates/phux-protocol/src/wire/frame/mod.rs), lines 38–42).
A control ping sent after such a frame cannot pass it within the same ordered
direction. At 10 Mbit/s, 8 MiB alone needs about 6.71 seconds to serialize;
this is a worst-case arithmetic bound, not an observed upload or a claim
that shipping clients normally choose maximum-size chunks.

“Control is small” is therefore not a safe scheduling assumption. First
measure actual upload sizes and introduce bounded chunking/in-flight budgets
in clients. If bulk commands are common, negotiate a bulk transfer stream
or another explicitly scoped transfer facility. Preserve upload offset
acknowledgments, replay verification, and whole-file hashing. A new stream
does not remove the connection-wide congestion budget.

Tracked work: phux-wgav.

**Measurement evidence (2026-09-13):** a real `Connection`/`ServerRuntime`
upload experiment confirms the wire-ceiling concern. At 10 Mbit/s with 50 ms
shaped RTT, one 8 MiB frame occupied mutable `Connection::send` for 6.069 s.
Smaller 16/64/256 KiB frames traded goodput for lower control delay. This is a
wire-ceiling experiment, not evidence that a shipping upload producer selects
8 MiB chunks. See the [production-path report](2026-09-13-protocol-path-measurements.md)
for the loaded-host measurements, timing origins, and remaining harness review
qualifications. No universal chunk policy or new bulk stream is inferred from
those diagnostics. Server blocking uploads and queued command work have
separate admission budgets; active cancellation and production dispatch
isolation remain acceptance gates.

**Acceptance experiment:** upload compressible and incompressible files at
0.3/3/10 Mbit/s while issuing control requests and typing in another pane.
Compare 16/64/256 KiB chunks against the legal maximum, tracking control p99,
upload goodput, memory, and cancellation latency.

## Polling clients repeatedly negotiate the same control connection

**Medium impact; source-proven redundant work.** Each wait poll calls
`get_screen_scrollback`
([wait.rs](../crates/phux-client/src/wait.rs), lines 479–519), which opens a
fresh UDS connection, negotiates, requests a JSON screen, and drops the
connection ([snapshot.rs](../crates/phux-client/src/snapshot.rs), lines 81–106).
The default poll gap is 150 ms; short idle waits reduce it to 25 ms.
Connection reuse can remove repeated setup without changing the wire or
the side-effect-free screen-read contract. This path is UDS-based; these
numbers are not repeated remote TLS-handshake measurements.

Tracked work: phux-69pq.8.

**Measurement evidence (2026-09-13):** the persistent production polling path
was compared with the unchanged one-shot screen helper against real
`ServerRuntime`/PTY fixtures over counted Unix sockets. Across paired 1/8/32
agent idle and changing-screen cases, accepted polling connections fell by
92.9–96.4% and whole-experiment process CPU by 46.1–52.5%. These are one
loaded-host run's diagnostics, not latency baselines or client-only CPU
attribution. An integrated parent rerun reproduced 35.1–52.1% lower process
CPU and the connection-count invariants; detection timing varied by less than
one requested polling interval, without a consistent speedup. The
[polling report](2026-09-13-polling-reuse-measurements.md)
records the reproduction command, raw case table, timing origins, and
reconnection/deadline regression.

**Acceptance experiment:** 1/8/32 concurrent waits on idle and changing
terminals, measuring connections opened, allocations, server CPU, and
condition-detection latency. Preserve deadline coverage and reconnection.
Event-driven acceleration must retain arbitrary text-match semantics and a
fallback for applications without shell integration.

## Observability does not yet demonstrate per-stream isolation

The server exposes process-wide `wire.write`, `pump.gap_resync`, and
`consumer.ack_rtt` metrics
([perf.rs](../crates/phux-server/src/perf.rs), lines 68–155), rather than
queue age/bytes, writer stalls, or scheduling delay by connection and stream.
The kernel's echo probe closes on the next output from the terminal, not a
wire-correlated echo ([kernel perf](../crates/phux-client-core/src/perf.rs),
lines 51–88). Continuous output can therefore make this metric look healthy
without proving a key reached the application. Browser latency histograms
are explicitly disabled on wasm32; counters still work.

The existing [UDP delay proxy](../scripts/bench/udp-delay.py) deliberately
adds delay without loss, reordering, or bandwidth shaping. The
[mux comparison](../scripts/bench/mux-compare.sh) has direct QUIC/WS/UDS
lanes and history workloads. These are useful foundations, but not evidence
that a slow pane cannot delay a quiet pane or that relayed multi-stream
traffic preserves the same behavior.

Use bounded diagnostic samples keyed by lane/connection/stream, with queue
bytes and oldest age, blocked-write duration, active streams, bootstrap
READY time, and resync reason. Avoid unlimited metric-label cardinality.
Correlate real test payloads at the PTY for echo acceptance; aggregate
histograms remain useful for routine diagnosis.

The native benchmark currently reports the engine READY offset as
`ready_bytes`, while production sends the whole encoded blob before protocol
READY. Its `chunks: 1`, `caller_buffer_growths: 1`, and `payload_copies: 0`
are constants ([server_measure.rs](../crates/phux-server/benches/server_measure.rs),
132–149). They are not allocation instrumentation. The fanout benchmark
measures Bytes broadcast, omitting the production pumps, encoding, transport,
decode, and engine ingestion. Its result cannot establish zero copies per
additional end-to-end subscriber.

`ATTACH_HANDLE` also stops at deferred aggregate attach on QUIC, before
per-Terminal bootstrap. Compare first usable READY/paint rather than declaring
the shorter handler span a latency win. Existing relay tests using a stub
connector and client mux tests using a scripted server do not validate
production connector ownership or the real subscription-admission loop.

The native adapter test named
`native_history_finish_rejects_trailing_and_post_finish_pages` actually
accepts ignored trailing and repeated history at this checkout
([ghostty.rs](../crates/phux-client-core/src/engine/ghostty.rs), 1677–1700).
Test names and self-reported counters require the same source review as the
implementation. Tracked benchmark work: phux-slogic.5.7 and phux-69pq.5.

Tracked work: phux-au1s.11.

**Harness implementation (2026-09-13):** the UDP proxy now supports
`--mbit`, `--loss-percent`, and `--seed`. Each direction has an independent
serialization clock and seeded datagram-loss stream, with byte/packet queue
bounds and separate random-drop, tail-drop, and shutdown-drop counts. Rates
count UDP payload bytes, excluding headers. It does not reorder packets or
model shared half-duplex capacity. The single-client routing assumption remains.
Delay-only overflow exits nonzero and marks the experiment invalid.

The mux harness exposes `--path-mbit`, `--loss-percent`, and `--loss-seed`,
records `relay-metrics.json`, and rejects nonzero relay exit status. A sample
invocation for a 150 ms RTT, 3 Mbit/s, 1% loss experiment is:

```sh
bash scripts/bench/mux-compare.sh --mux phux-quic --rtt-ms 150 \
  --path-mbit 3 --loss-percent 1 --loss-seed 17 --out target/bench/shaped
```

This is a runnable experiment specification, not a recorded performance result.
Nine proxy tests pass, including a real UDP echo, real queued shutdown under
SIGTERM/SIGINT, deterministic loss, serialization ordering, independent
directions, and invalid delay-only overflow. `bash -n` and a production shell
function smoke check pass; ShellCheck reports only the preexisting SC2059
dynamic-character `printf` finding. Independent source review dispositioned
the proxy's initial silent-overflow and missing queued-shutdown-test findings.
Lizard reports proxy ingress 3 → 3, new enqueue 5, and proxy run 3 → 9.

### Acceptance matrix

| Dimension | Cases |
|---|---|
| Route | Direct QUIC; relay plus production connector; federation; UDS; WS; WT |
| Terminals | 1, 8, 32; one quiet input target beside flood/history/upload |
| Path | 0/50/150/300 ms RTT; 0/1/3 percent loss; 0.3/3/30 Mbit/s |
| Reader behavior | Healthy, one stopped stream, globally slow client, abrupt disconnect |
| Lifecycle | Rebind, close while binding, delayed control versus content, exhausted stream credit |
| Payload | Quiet shell, full-screen TUI, Unicode history, compressible and incompressible bulk |

Report echo/control p50/p95/p99/max, time to first usable READY, goodput,
RSS, queue bytes/oldest age, resyncs, and cancellation latency. Separate
deliberate output skipping from loss of byte-exact recorder data. Measure
builds and runtime performance serially on an otherwise quiet host. Define
per-route pass thresholds from fresh baselines, and retain the existing
local echo and native fidelity requirements.

## Protocol claims need a precision pass

The [transport architecture](../docs/architecture/transport.md), lines 81–87,
still describes raw QUIC as one stream. The newer
[multi-stream spec](../docs/spec/proto.md), lines 146–198, describes the new
shape. Readers cannot use both as current implementation descriptions.

The spec's claim that bidirectional stream order supplies input-to-echo
causality needs correction: a QUIC bidi stream has two independently ordered
directions. The application processing input and producing output supplies
that causality. Similarly, stream flow control is independent but congestion
control, connection credit, and CPU remain shared. “A flooding Terminal
stalls only its own stream” needs those qualifications and end-to-end proof.

The ADR filenames for the recent decisions are 0115/0116/0117 while their
H1 headings retain 0113/0114/0115. Correct the identity mismatch through the
repository's documentation policy; do not reinterpret the stale headings as
separate decisions.

Tracked work: phux-au1s.12.

## Constraints on optimization choices

- Preserve raw VT byte ordering and exact native bootstrap fidelity. Lossy
  datagrams require a separately negotiated state-recovery design; dropping
  arbitrary terminal bytes is not a throughput optimization.
- Preserve operation-id replay rules for acknowledged input. Faster connect
  or 0-RTT must not replay a paste, spawn, or another side effect.
- Preserve generation fencing when prioritizing traffic. A priority writer
  cannot move live bytes ahead of their bootstrap or resurrect a tombstoned
  generation.
- Keep the negotiated TLV codec decision from
  [ADR-0117](../docs/adr/0117-wire-codec-stays-tlv.md). Measure allocations,
  copies, batching, and compression around opaque payloads before pricing a
  serialization-format migration. The machine-readable schema and codec
  agreement tests are tracked work: phux-au1s.8.
- A broader WebTransport multi-stream design needs an explicit decision:
  the current spec intentionally keeps WebTransport single-stream. Browser
  improvements should first account for its actual JS/WASM queue boundaries.

## Recommended order

First establish route and lifecycle correctness with the real client and
server connected through each advertised lane. Then use the shaped-path
matrix to remove dispatch stalls and bound queue age/bytes. Optimize copies,
compression, connection reuse, and bootstrap scheduling against measured
stage costs. Price 0-RTT, extra bulk streams, and browser multi-stream support
after the earlier changes expose the remaining bottleneck.

### Durable follow-up map

These identify future work; Beads owns its current status and sequencing.
Existing tasks were reused where the scope already existed.

| Work area | Tracked work |
|---|---|
| Authenticated relay consumer grouping | phux-au1s.4 |
| Bilateral negotiation and hub/FFI route parity | phux-au1s.13 |
| Ingress framing, provenance, and admission bounds | phux-au1s.14 |
| Bind ordering, rebootstrap, teardown, stream caps and semantic parity | phux-au1s.15 |
| Fair dispatch, mux burst draining and shared window tracker | phux-au1s.16 |
| Native progressive history and aggregate staging | phux-erut |
| Exact native live bytes under restrictive caps | phux-958u |
| Input replay certainty; paste/key submission order | phux-g3ea; phux-4qn0 |
| Explicit-mTLS startup refusal; scope enforcement | phux-au1s.17; phux-pjc5.5 |
| Slow command execution; control bulk budgets | phux-jyt5; phux-wgav |
| Federation saturation and bidirectional progress | phux-yxxv |
| WT admission; browser transport/fallback | phux-woo7; phux-42sg |
| Gap recovery; reconnect deadlines and connection reuse | phux-qobt; phux-1vob |
| Persistent polling connections | phux-69pq.8 |
| ACK validation and measured coalescing | phux-ghfu |
| Shaped-path isolation; actual native/copy measurements | phux-au1s.11; phux-slogic.5.7; phux-69pq.5 |
| Protocol documentation; machine-readable TLV schema | phux-au1s.12; phux-au1s.8 |

## Validation performed

- `bash scripts/doctor.sh docs`: passed, zero prerequisite problems.
- `bash scripts/doctor.sh core`: passed, zero prerequisite problems.
- `bash scripts/check-docs.sh`: passed, 199 files checked, zero violations.
- `cargo test --locked -p phux-protocol`: passed in this worktree's private
  target directory: 75 unit tests, 172 integration tests, and one doctest.
  Includes STREAM_BIND malformed-input tests, framing bounds, compression,
  allocation-abuse checks, and wire round trips/snapshots.
- `cargo test --locked -p phux-dial window::tests`: passed, three tests,
  including the real-QUIC dropped-ACK test that bounds tracked buffering.
  This validates the existing send-window mechanism, not multi-pane fairness.
- Independent fresh-context source review confirmed the major identity,
  compatibility, native-history, input, framing, and mTLS findings. Its
  qualifications are incorporated: relay exposure is conditional, EOF
  starvation depends on event-channel timing, native rewriting depends on
  restrictive capabilities, and retry refusal does not prove an automatic
  duplicate write.

These are codec baseline checks, not real-server multi-stream, browser, relay,
or full-CI acceptance. No runtime functions changed, so before/after
cyclomatic-complexity measurement is not applicable to the audit artifact.
