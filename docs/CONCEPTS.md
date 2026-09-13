---
audience: humans, contributors, agents, consumers
stability: stable
last-reviewed: 2026-09-13
---

# How phux works

**TL;DR.** phux is a terminal multiplexer. Your shells live in a background server. You split them into panes, detach, and they keep running. Each pane is a live terminal object on a wire, so the TUI, Cockpit, a script, or an agent attach to the same one. Nobody screen-scrapes. Nobody holds a second copy.

---

```text
      your programs: zsh, vim, htop, an agent's shell
                          │
                          │  PTY
                          ▼
 ┌─────────────────────────────────────────────────┐
 │ phux server -- keeps running when you leave     │
 │                                                 │
 │ libghostty terminal: the real one. Screen,      │
 │ scrollback, and modes live here, so they        │
 │ survive detach and feed every attach.           │
 └───────────────┬──────────────────▲──────────────┘
                 │                  │
     output goes │                  │ input comes back
     down as raw │                  │ up as structured
     VT bytes,   │                  │ key, mouse, and
     verbatim    ▼                  │ paste events
 ┌──────────────────────────────────┴──────────────┐
 │ attach: TUI, CLI, web, Cockpit                  │
 │ several clients share one live terminal;        │
 │ detach does not copy it                         │
 └─────────────────────────────────────────────────┘
```

The server holds the terminals. The TUI, CLI, web, and Cockpit attach to those same ones. Detach does not copy.

## Resources

A resource is a server-owned, addressable thing. Every resource has:

- a kind and a stable id;
- a lifecycle: spawned, then closed with a reason (`Exited`, `Killed`, `ParentClosed`, `ServerShutdown`);
- an ordered, opaque output stream with a codec, and a bootstrap a consumer loads before live bytes;
- a kind-defined input channel;
- a tagged event stream;
- metadata, and an optional parent set at spawn and immutable.

Terminal is the first kind: a PTY child and a libghostty engine, with columns, rows, a title, and a working directory. Operations that only make sense there — typed input, resize, screen reads — are refused on any other kind.

AgentSession is the second kind, and this checkout serves it. The server advertises `RESOURCE_KINDS`; the runtime creates the resource; `phux agent session open|close`, `phux agent emit`, and `phux agent log` are the producer and reader verbs; `%name` resolves one. It is the structured event stream of an agent harness, bound to the Terminal the agent runs in. Closing the parent closes the child; closing the child never touches the parent. While that stream is live it is the source of agent lifecycle; the pane detector is compatibility for a harness that does not emit. Harness authors: [`consumers/harness.md`](./consumers/harness.md).

`phux agent show` is a different surface: it reads agent state from a pane, not from an AgentSession resource.

Sessions, windows, panes, and splits are not a lifecycle tier. "Pane" stays a TUI and CLI word for a Terminal-kind resource in a layout slot, expressed as metadata and client logic.

## The wire

The wire carries four things:

- **Identity.** A `ResourceId` is `Local { id }` or `Satellite { host, id }`. A hub retags inventory with satellite ids; it does not merge remote session or window models. Selectors render as `@42` and `prod-box-3/@42`.
- **Lifecycle.** Spawn with a kind and an optional parent; close with a reason; parent cascade; atomic `KILL_RESOURCES`.
- **Bytes.** Opaque per-kind output (bootstrap and live). Structured input atoms for a Terminal; appended records for an AgentSession. Both ends run the engine for the kinds they show; the wire is not a second screen model.
- **Metadata.** Opaque key-value pairs. The server stores them; it does not interpret them.

There is no L2 collection tier. Group membership is metadata plus client logic; atomic teardown is a single L1 operation. See [`spec/L2.md`](./spec/L2.md).

The byte-level codec is [`spec/appendix-encoding.md`](./spec/appendix-encoding.md). L1 is [`spec/L1.md`](./spec/L1.md); metadata is [`spec/L3.md`](./spec/L3.md).

## Consumers are peers

The reference TUI, the headless CLI, the browser client, and Cockpit are peers. None has protocol-level standing: if a consumer needs a capability the wire does not provide, the answer is an ADR that extends the spec, not a consumer-shaped hook ([ADR-0017](adr/0017-tui-not-protocol-privileged.md)).

- TUI: [`consumers/tui.md`](./consumers/tui.md)
- CLI: [`consumers/agents.md`](./consumers/agents.md)
- web: [`consumers/web.md`](./consumers/web.md)
- Cockpit: [`consumers/cockpit.md`](./consumers/cockpit.md)

A consumer that wants structured state carries the engine for the kinds it shows. One that does not render a kind lists it and draws none of it.

## Maturity

The protocol is 0.9.0, pinned in `phux-protocol` and mirrored by [`spec/`](./spec/README.md); a CI gate keeps the two in sync. Spec leads the code.

This checkout serves both resource kinds. Confirm with `phux status --json`: a server that advertises `RESOURCE_KINDS` has AgentSession. Older brew or curl releases may not.

The long arc lives in [`vision.md`](./vision.md). This page owns the Status table below; other docs link here rather than restating the gaps.

## Status

Target-versus-shipped gaps as of the last review. Each row names the ADR that owns the target and the bead that tracks the work.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Working-directory and command-boundary events as an L1 Terminal-facet frame | `TERMINAL_EVENT` has no codec entry. `cwd_changed`, `command_started`, and `command_finished` reach consumers only through the `SUBSCRIBE_RESOURCE_EVENTS` gate path. | [ADR-0015](adr/0015-protocol-layering.md) | phux-ue2r |
| On-disk output journal and crash recovery | The server keeps every resource in memory. Nothing is journaled and there is no recovery flag. | [ADR-0092](adr/0092-durable-work-coordinator-authority.md) | phux-p91i |
| Workload authentication enforcement | The mTLS + scope-matrix profile is specified in `workload-auth.md`. The reference server requests no client certificate and enforces no scope matrix. | [ADR-0116](adr/0116-workload-auth-is-mtls.md) | phux-cockpit-p1q.11.2 |
| Cockpit projection of agent sessions | Cockpit lists Terminal-kind resources only; AgentSession children are not shown under their parent. | [ADR-0103](adr/0103-agent-session-resource-and-producer-fed-streams.md) | phux-am9y.25 |

## Where to go next

| You want to | Read |
|---|---|
| Run it | [`QUICKSTART.md`](./QUICKSTART.md) |
| Understand the wire bytes | [`spec/README.md`](./spec/README.md) |
| Understand how the server is built | [`architecture/README.md`](./architecture/README.md) |
| Drive it from an agent | [`consumers/agents.md`](./consumers/agents.md) |
| Use the browser client | [`consumers/web.md`](./consumers/web.md) |
| Use Cockpit | [`consumers/cockpit.md`](./consumers/cockpit.md) |
| Understand the TUI surface | [`consumers/tui.md`](./consumers/tui.md) |
| See why we decided X | [`adr/README.md`](adr/README.md) |
| Read the long arc | [`vision.md`](./vision.md) |
| Contribute | [`../CONTRIBUTING.md`](../CONTRIBUTING.md) |
