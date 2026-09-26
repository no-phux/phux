---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Desktop NAPI binding

**TL;DR.** Enable `phux-client-ffi/napi` in the single desktop host cdylib.
JS owns a `DesktopClient`; the native painter looks up its actual runtime
`Client` with `phux_client_ffi::napi::initialize().client(&handle)`.
No cells or grid frames cross the JS boundary.

## Host seam

The wrapper links this crate as an rlib alongside GPUIX's native rlib and calls
`napi_build::setup()` in its final build script. Both must use the same NAPI
runtime: this encoder pins `napi = 3.12.7`, `napi-derive = 3.6.8`. Calling the
Rust registry accessor force-links this crate; its NAPI constructors register
`DesktopClient` into the shared runtime. Do not separately load this crate's
cdylib: a second loaded copy would have a second registry.

`initialize()` is idempotent. `Registry::client(&str)` returns a clone sharing
the connection, engine, and publication table, not a second session. A painter
may acquire immutable frames through that clone. It must not replace the
listener or drain events. A lookup after close returns `StaleHandle`; an
already-acquired clone observes the same closed runtime. A lookup before
connect returns `NotConnected`. IDs never repeat and fail on exhaustion.

## JS seam

- `new DesktopClient()` creates an opaque string `handle`.
- `connect({socketPath, cols, rows}, onActivity)` starts the runtime once.
  Success means the driver started; connection failure is observed through
  `status()` / `lastError()`. This slice supports local Unix sockets.
  Dimensions must be finite integer JS numbers in `1..=65535`; validation
  precedes narrowing to the runtime's dimensions.
- `status()`, `lastError()`, `connectionEpoch()`, `topology()`,
  `refreshTopology()` expose the shared runtime projection. The epoch is a
  decimal string, never a JS number. Status is the projection's `Connecting`,
  `Attached`, `Closed`, or `Failed` vocabulary.
- `attachSession(name)` selects an existing server session.
- `spawnTerminal(sessionId)`, `attachTerminal(resourceId)`, and
  `detachTerminal(resourceId)` return runtime request IDs. They complete via
  typed `SpawnAnswered`, `AttachAnswered`, and `DetachAnswered` events.
  `sessionId` must be a finite integer JS number in `0..=4294967295`.
  Invalid numeric input queues no command; NAPI's truncating integer coercion
  is not used for input IDs or dimensions.
- `inputReadiness(resourceId)` reports `ready` and `deliveryFenced`.
  `applyPaste(resourceId, text)` uses the runtime's acknowledged **untrusted**
  paste path and returns a decimal-string delivery ID. `InputDelivery` retains
  all three outcomes: `Delivered`, `Refused`, `Unknown`. Never blindly retry
  `Unknown`; it may have written any amount. The native consumer acknowledges
  a fresh authoritative projection only after actually consuming it.
- `takeEvents()` is the sole live event drain. Call it for **every wake**, even an
  empty wake, to rearm notifications. Then read state and schedule native
  painting. Events retain runtime order. Lifecycle events are encoded from
  `projection::event::lifecycle`; terminal signal/agent/file-transfer DTOs are
  outside this initial slice. No arbitrary wire frames are exposed.
- `close()` invalidates the native handle immediately, shuts down the shared
  client, and **returns the final event batch**. Process its returned events
  exactly like `takeEvents()`; it contains queued receipts and the runtime's
  final outcomes for unresolved acknowledged input (including `Unknown` for
  an in-flight attempt). Each receipt is returned once across live drains and
  this final batch. Repeat close and subsequent runtime operations throw
  `StaleHandle`; there is no post-close drain. Rust `Registry::close` likewise
  returns the final runtime event batch. Finalization
  and Node environment cleanup also close the client. Use explicit `close()`
  for disposal; a callback retaining its owning JS object can delay GC.

The runtime has one listener. Its weak, bounded TSFN queues `onActivity(handle)`
on the owning JS environment; it does not call JS from a runtime thread or
keep Node alive. A queued wake may arrive after close: ignore its stale handle.
Queue-full already represents a pending equivalent wake; it is not retried.
One cleanup hook is registered per live NAPI environment. It walks only the
still-live registry entries for that environment; explicit close and GC remove
their entries immediately. Client churn does not accumulate cleanup hooks or
per-disposed-client cleanup payloads. Environment exit removes its registration
key, so a later environment reusing that address gets a fresh hook.

## Independent views

`attachTerminal` and `detachTerminal` are resource subscription commands, not
view constructors. `createView(terminalId)` returns a decimal-string runtime
ViewId over the existing terminal. Each view owns its scrolling, selection,
search anchors and gestures. `destroyView` removes that presentation; it neither
kills the PTY nor detaches sibling views. Initial connection dimensions are not
view geometry. Focused resize authority and native painting remain integration
work, and **phux-d4x9.18** still requires complete duplicate-view UX.

`scrollView`, `followLiveView`, `viewInfo`, the view-anchor/selection methods,
`searchView` and `viewSelectionGesture` use runtime view ownership. Document
handles from another view are rejected. IDs and generations are decimal strings;
JS geometry and key numbers are checked before narrowing. No cell query exists.
Search accepts at most 4096 query bytes and returns at most 4096 matches. Copy
currently limits the returned JS text to one MiB **after** native formatting;
native allocation and traversal bounds remain an open integration requirement.

`commitText`, `keyEvent`, `mouseEvent` and `focusView` return whether input was
queued, not whether it was delivered. `pasteView` uses acknowledged untrusted
paste and returns a delivery correlation. Current engine membership, view target,
role and readiness checks occur under the same control lock as input admission.
Old view IDs cannot authorize input after engine replacement even if the terminal
ID is reused. Immediate paste refusals wake the existing event owner after the
lock is released. Marked/preedit text stays in native IME state; a composing key
event is rejected rather than transmitted as committed text.

Run `just desktop-native-view-test` after building the combined desktop addon.
Its isolated real-PTY fixture verifies two views of one terminal, independent
scroll/selection/search, cross-view handle rejection, input receipts and sibling
survival after view destruction. Unit regressions also exercise disconnect at
the admission boundary, same-terminal engine replacement and refusal wakes.

## Reproducible verification

Use a private worktree target directory. From the repository root on macOS:

```sh
cargo test --locked -p phux-client-ffi --no-default-features --features napi --lib
cargo build --locked -p phux-client-ffi --no-default-features --features napi --example napi_host
cp target/debug/examples/libnapi_host.dylib target/debug/examples/napi_host.node
cargo run --locked -p phux-client-ffi --no-default-features --features napi --example napi_smoke -- "$PWD/target/debug/examples/napi_host.node"
cargo check --locked -p phux-client-ffi
cargo check --locked -p phux-client-ffi --no-default-features --features uniffi
```

The host fixture is a separate cdylib consuming the FFI rlib. Its native probe
resolves the same handle JS created. The smoke fixture starts an isolated
PTY-backed server, drives real Node exports, and checks topology, attach,
input success/refusal, detach/reattach, spawn, stale handles, wake rearming, and
live Worker environment teardown, original-number validation, and final receipt
draining. Cleanup tests exercise 300,000 disposed clients with forced GC and
100 forced terminations of attached Workers; a Rust test directly asserts one
registration through 10,000 create/close cycles and environment-key reuse.
The fault proxy withholds an actual `APPLY_INPUT`: one case closes with the
attempt in flight, while another restarts the real server. Both require exactly
one `Unknown`; restart also verifies stable identity, advancing epochs on a
held native Client lease, and new listener wakes. Rust tests cover identity
exhaustion above JS's safe-integer range, close/lookup races, held native leases,
and `Unknown` input encoding. The `napi` feature is explicit so these tests
cannot silently vanish behind the default C ABI gate.
