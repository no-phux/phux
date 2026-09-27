---
audience: contributors
stability: stable
last-reviewed: 2026-09-27
---

# 0140 — Sidebar machines come from a hosts provider

**TL;DR.** The TUI sidebar groups sessions by machine. The attached
server's sessions arrive live on the attach stream. Every other machine
(this one, when the attach is remote, and each `[[remote]]` host) comes
from a *hosts provider*, a command that prints the `phux.hosts/v1`
document. The built-in provider is `phux ls --all --json`. A click on
another machine's session replaces the process with `phux attach` there.

Status: Accepted
Date: 2026-09-27

## Context

A user on a tailnet registers a second machine with `phux host add`
and can attach to it with `phux attach mini`. Once attached, though,
the TUI only knows the server it dialed: the laptop's sessions vanish
when attached to the mini, and the mini never appears when attached
locally. The federation hub ([ADR-0107](./0107-satellite-sessions-are-listed-never-adopted.md),
[ADR-0136](./0136-hub-mirrors-satellite-agent-metadata.md)) solves a
different problem. It needs a hub started with `--hub`, a restart per
new satellite, and it relays satellite panes into a hub session rather
than attaching to the satellite's own layout.

The request that prompted this also asked for native features to be
built the way plugins are, so the sidebar does not grow a second,
private data path each time it learns something new.

## Decision

1. **`phux.hosts/v1` is the contract.** One row per machine: its
   `phux attach` name (`local` for this machine), a label, `local` or
   `remote` kind, reachability with the reason, and its sessions as
   `phux ls --json` spells them. The type lives in
   `phux_core::host_list`. Added keys are non-breaking.
2. **A provider is a command.** The sidebar runs it on a cadence
   (`[sidebar] hosts-refresh-secs`, default 10, floor 2) and draws what
   it prints. `[sidebar] hosts-provider` replaces the default argv;
   `[sidebar] hosts = false` runs none. The built-in provider is this
   binary's `ls --all --json`, which dials every host at once with a
   3 s deadline each and keeps a host that did not answer as an
   unreachable row.
3. **The attached machine is never listed twice.** The CLI records
   which machine it attached to (`local`, or the registry name) before
   the attach loop starts. That machine's row comes from the attach
   stream, labelled by that name; the provider's copy of it is dropped.
   An attach with no registry name (`--quic`, `--ws`) records nothing
   and runs no provider.
4. **Switching machines is a process hand-off.** `switch-host
   { host, name }` detaches, restores the terminal as a detach does,
   and execs `phux attach --remote HOST NAME` (or `--socket` for
   `local`). The dial, repair, and reconnect rules are the CLI's,
   unchanged.

## Why

A command with a JSON contract is the seam plugins already use for
actions, events, and panes. Putting the built-in hosts list behind the
same seam means a plugin (a Tailscale-aware discovery tool, a fleet
inventory) can replace or wrap it without the TUI learning a new
protocol. The subprocess costs one fork per refresh against a listing
that is dominated by network round trips anyway.

Exec rather than an in-process re-dial keeps one attach per process,
which the recorder, the replay journal
([ADR-0053](./0053-acknowledged-idempotent-input.md)), and the reconnect
loop all assume.

## Tradeoffs

- Another machine's sessions are a poll, not a subscription. They are
  up to one refresh stale, and carry no agent counts; those rows draw
  the quiet "nothing known" dot.
- The provider dials each registered host every refresh. Ten seconds is
  cheap for a handful of hosts; a large fleet should raise the interval
  or supply its own provider.
- A switch leaves the alt screen for one frame while the new process
  starts.

## Alternatives

- **Dial every host from inside the TUI.** Rejected: dial planning lives
  in the CLI, and a second, in-process copy of it is a private path a
  plugin cannot use.
- **Require a federation hub.** Rejected as the default: the hub is the
  right tool for relaying panes into one session, but it asks for a
  restart per satellite and a server flag the common two-machine setup
  does not need.
- **A generic plugin-declared sidebar section.** Deferred: the hosts
  provider proves the contract on one section first. Moving Agents and
  arbitrary plugin sections onto the same provider shape is follow-up
  work.
