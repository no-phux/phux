---
audience: consumers, contributors
stability: evolving
last-reviewed: 2026-09-13
---

# Build a client against the wire

**TL;DR.** Three honest paths: the CLI and MCP if you are an agent, the
publishable `phux-protocol` crate plus your own engine if you are a
client, `phux-client-ffi` if you are a native embedder. There is no
public Rust SDK crate. The TUI is not a privileged peer.

---

Pick the path that matches the job. Do not start from ADRs.

| You are | Use | Start here |
|---|---|---|
| A coding agent or script | CLI / MCP | [`agents.md`](./agents.md), [`mcp.md`](./mcp.md) |
| A harness that can emit lifecycle | AgentSession producer | [`harness.md`](./harness.md) |
| A new graphical or headless client | Speak L1, carry an engine | [`../spec/L1.md`](../spec/L1.md), [`web.md`](./web.md) |
| A native app | `phux-client-ffi` C ABI | [`cockpit.md`](./cockpit.md) |

`phux-protocol` is the publishable codec (`publish = true`). It is the
bytes. `phux-client` is workspace-internal (`publish = false`) and is
not a downstream dependency; see [`sdk.md`](./sdk.md).

## Minimum client loop

1. Dial (UDS locally, QUIC or WebSocket remotely). Framing is
   length-prefixed phux frames ([`../spec/proto.md`](../spec/proto.md)).
2. `HELLO`, then wait for `HELLO_OK`. Negotiate features. AgentSession
   exists only when the server advertises `RESOURCE_KINDS`.
3. Attach or subscribe. Terminal output is raw VT bytes. Input is
   structured key / mouse / paste atoms. Do not invent a cell-diff
   wire.
4. If you render a terminal, run an engine on the bytes. The reference
   shape is [phux-web](./web.md): carry libghostty, project locally.
5. If you show agent state, prefer a live AgentSession stream over
   scraping the grid. [`harness.md`](./harness.md).

A worked byte-level walkthrough is [`../spec/TUTORIAL.md`](../spec/TUTORIAL.md).
Protocol version and changelog: [`../spec/CHANGELOG.md`](../spec/CHANGELOG.md).
The TUI has no extra standing ([ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md)).
