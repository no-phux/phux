---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-30
---

# Crate dependency graph

**TL;DR.** The crate edges that hold phux together and the boundaries
they enforce: `phux-core` and `phux-protocol` never depend on each other,
`phux-protocol` re-exports libghostty atoms directly, and the ratatui
chrome is fenced into `phux-tui` above a headless `phux-client`. Plus how
each crate participates in
the L1/L3 wire layering from ADR-0015 and ADR-0102.

---

```
                ┌────────────────────────────────────────────┐
                │                   phux                     │  binary; subcommands
                └─┬──────────┬──────────────┬───────────┬────┘
                  │          │              │           │
            ┌─────▼───┐  ┌───▼────┐   ┌─────▼────┐  ┌───▼────┐
            │ server  │  │  tui   │──►│  client  │  │ config │
            └──┬────┬─┘  └─┬──┬───┘   └─┬──┬─────┘  └────────┘
               │    │      │  │         │  │    tui    = attach driver +
               │    │      │  └────┐    │  │           ratatui chrome (ADR-0100)
               │    │      │       │    │  │    client = headless library:
               │    │      │  ┌────▼────▼──▼─┐         connection, transports,
               │    │      │  │  client-core │         agent verbs; NO ratatui
               │    │      │  └──────┬───────┘  client-core = pane-interior
       ┌───────▼─┐  │   ┌──▼─────────▼──────┐   substrate: layout, multi-pane,
       │  core   │  └──►│     protocol      │──► libghostty-vt   predict, session
       └─────────┘      │ (codec, input     │   ◄─ tui links it  kernel; NO
                        │  events, wire     │      too: a local  ratatui, NO tokio
                        │  envelopes)       │      Terminal per  (ADR-0020)
                        └───────────────────┘      pane (ADR-0013)
```

Five crate boundaries carry weight:

1. **`phux-core` and `phux-protocol` do not depend on each other.** Core
   holds the in-process domain (slotmap keys with generational tags,
   resource descriptors and their facets, layout tree, registry). Protocol
   holds the wire shape (`u32`-wide IDs, length-prefixed TLV,
   libghostty-derived input/style atoms). The two ID spaces meet in
   `phux-server::id_bridge::IdBridge` and nowhere else; this isolates wire
   stability from in-process refactors and vice versa. See ADR-0011 for the
   full rationale.
2. **`phux-protocol` depends on `libghostty-vt` directly** (ADR-0008,
   gated by the `server` cargo feature). The protocol crate re-exports
   libghostty's input and style atoms instead of mirroring them. The
   default-features-off shell exists so `crates.io`/`docs.rs` see a
   small surface without the full terminal-emulator dependency graph.
3. **The TUI is two crate edges away from the headless library and the
   substrate, and both edges are one-way** (ADR-0020, ADR-0100). `phux-tui`
   depends on `phux-client` (connection, transports, the stdin parser, the
   exit vocabulary, the agent-verb helpers its chrome reads) and on
   `phux-client-core` (layout, multi-pane, predict, the session kernel).
   `ratatui` lives only in `phux-tui`; neither dependency declares it, so a
   stray `use ratatui` in either fails to build, and `phux-mcp` cannot link
   the chrome because no edge leads there. The two-renderer rationale is
   owned by [`render-layering.md`](./render-layering.md). Both `phux-client`
   and `phux-tui` re-export `phux_client_core::{layout, multi_pane,
   predict}` so consumers keep stable paths.
4. **`phux-dial` is the one outbound-transport layer.** The client's
   remote attach and the federation hub (which dials satellites as an
   ordinary consumer, ADR-0038) both establish QUIC/WebSocket through it:
   fingerprint-pinned TLS 1.3 plus the ADR-0031 bearer token. It stops at
   the byte stream; framing stays with its consumers
   ([`transport.md`](./transport.md)).
5. **`phux-client-runtime` is the one orchestration layer below every
   binding** (ADR-0133): registry resolution, dial planning, the reconnect
   `Ladder` and fatal-refusal rule, the relay tunnel, the sans-IO control
   plane, the engine owner thread, and grid publication, as a Rust API
   ([`client-runtime.md`](./client-runtime.md)). A connection loop,
   `select!`, or backoff constant in a binding is in the wrong crate. The
   server's hub link keeps its own redial ladder; it is a federation
   dialer, not a client binding.

`server`, `client`, and `tui` all depend on `protocol`. `server` and `tui`
also depend on `libghostty-vt` directly: the server's `Terminal` is the
canonical state for each Terminal-kind resource and drives the
structured-input encoders (ADR-0006, ADR-0008); the TUI's `Terminal` is a
local replica fed by `RESOURCE_OUTPUT` bytes for the Terminals that client
has attached, with `RenderState` providing per-row dirty tracking and a
per-pane front buffer narrowing each dirty row to its changed cells for
efficient redraw. `client-core` links `libghostty-vt` only under its
`native-engine` feature (the wasm client leaves it off); `client` names
only libghostty's error type, for the shared exit vocabulary.
See ADR-0013 and `../../research/2026-05-25-libghostty-renderstate.md`
for the renderer-side contract on both ends.

`phux-config` is a sibling of `core` and is consumed by the binary, the
server, the client, and the TUI.

`phux-client-ffi` is the one binding leaf above the runtime (ADR-0135),
excluded on wasm: one `projection/` layer, a `c-abi` encoder for native
embedders and Cockpit, and a `uniffi` encoder for Swift/Kotlin. It owns no
terminal, session, topology, or transport state machine.

## Browser client crates (standalone wasm workspace)

The browser client lives under `clients/` as its **own cargo workspace**,
excluded from the native `cargo build --workspace` / CI (`exclude =
["clients"]` in the root manifest). It targets `wasm32-unknown-unknown` only
and never builds the native binary.

```
                     ghostty (Zig) ──zig build──► ghostty-vt.wasm
                                                       │ include_bytes!
   phux-protocol ───┐                                  ▼
   (default feats,  ├─────────────►  phux-web ◄── phux-vt-web (engine driver)
    wasm-safe)      │   wasm-pack         │
   phux-client-core─┤                     ▼
   (session kernel) │           phux_web_bg.wasm + phux_web.js
   web-sys ─────────┘
```

- **`clients/phux-vt-web`** — a safe Rust driver over `ghostty-vt.wasm` (the
  VT engine, built from ghostty's Zig). Loaded as a **separate** wasm instance,
  not linked in (ADR-0025). Depends on nothing phux.
- **`clients/phux-web`** — the browser client: `phux-vt-web` (engine) +
  `phux-protocol` (the wire codec, **default-features** so it's libghostty-free
  and wasm-safe per ADR-0024) + `phux-client-core` (the session kernel — it
  imports `engine`, `history`, and `session`) + `web-sys`
  (WebSocket/canvas/keyboard). The `phux-client-core` edge crosses the
  workspace boundary by path; wasm consumers depend on the crate directly and
  never enable its `native-engine` feature, which is what keeps it wasm-safe.
  ADR-0025 originally descoped a shared core for native + web; that call was
  reversed, and the ADR records the correction.

This is the one place `phux-protocol`'s default-features-off shell pays off: the
web client compiles the codec to wasm without the `server` feature's
libghostty-vt dependency graph. Full architecture + build steps: [the web
client consumer doc](../consumers/web.md).

## Protocol layering and this implementation

[ADR-0015](../adr/0015-protocol-layering.md) layers the wire into tiers
plus two orthogonal cross-cuts, and
[ADR-0102](../adr/0102-resources-the-server-serves-kinds.md) makes L1
the resource substrate. Mapping each onto code currently in tree:

| Layer | Concept | Implemented in tree as |
|---|---|---|
| **L1** | Resource: identity + kind + lifecycle + opaque output stream + bootstrap + events; the Terminal facet adds PTY, libghostty `Terminal`, structured input, and snapshots; the AgentSession facet adds producer-fed JSONL records | `ResourceCore` plus the Terminal and AgentSession engines in `phux-server::resource`; wire `ResourceId` and the `SPAWN_RESOURCE` / `RESOURCE_SPAWNED` / `RESOURCE_CLOSED` / `BOOTSTRAP_*` / `RESOURCE_OUTPUT` / `APPEND_RESOURCE_OUTPUT` / `INPUT_*` / `BELL` / `EVENT` messages (`OSC_EVENT` is spec-only) |
| **L2** | Reserved; no collection tier | unused; see [`../spec/L2.md`](../spec/L2.md) |
| **L3** | Opaque metadata KV scoped to Terminal / group / global | `MetadataStore` on `ServerState` (`phux-server::state::metadata`): three maps mirroring the three wire `Scope`s, values opaque `Vec<u8>`, with `GET` / `SET` / `LIST` / `DELETE` / `SUBSCRIBE` and `METADATA_CHANGED` fan-out |

Cross-cuts:

- **Federation** ([ADR-0007](../adr/0007-mosh-class-transport-and-satellites.md)) — hub-and-spoke resource routing. Normal servers construct `LOCAL` ids; a hub retags aggregate inventory, spawn replies, and relayed frames as `SATELLITE { host, id }`. Satellite session/window models are not merged, and routes do not chain.
- **Automation** — server-side event hooks (`phux-server::hooks`, `[[hooks.<name>]]` and plugin `[[events]]`) fire argv on L1 events; there is no in-process rule engine.

A consumer's tier set is declared at HELLO time. Today's `phux-tui`
is an L1+L3 TUI consumer. `phux-client`'s headless free
functions use L1 and the L3 keys they need. The reference TUI is **not**
protocol-privileged
([ADR-0017](../adr/0017-tui-not-protocol-privileged.md)) — the wire
carries nothing that exists for it alone.

Wire bytes are normative in [`../spec/L1.md`](../spec/L1.md).

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| L1 mountable without the L3 service | One `ServerRuntime` serves both tiers. `GET_STATE` still carries `SessionInfo` and `WindowInfo` rows; their split-tree slot is retired and always absent (L1.md §1.1). There are no `WINDOW_*`, `LAYOUT_CHANGED`, or `FOCUS_CHANGED` frames. | [ADR-0015](../adr/0015-protocol-layering.md) | not scheduled |
