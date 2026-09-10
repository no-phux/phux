---
audience: contributors
stability: stable
last-reviewed: 2026-09-10
---

# 0105 — Sessions can outlive their last window

**TL;DR.** A session may be marked keep-empty, and a keep-empty session is
not reaped when its last window closes; only an explicit kill removes it.
A session created with no seed terminal is keep-empty by construction.
Default sessions keep today's tmux cascade, so the "exit closes everything"
habit and the ADR-0063 self-exit rule are untouched. Clients render an empty
state instead of detaching.

Status: Accepted
Date: 2026-09-10

## Context

The server reaps `pane -> window -> session` as one cascade
(`phux-server/src/state/reap.rs`, phux-60s): the session goes with its last
window, and the server self-exits once it holds no sessions after serving a
client (`tests/lifecycle/server_self_exit.rs`, ADR-0063). Every create path
seeds a terminal, including `phux.session.create/v1` (`docs/spec/L3.md`), and
the TUI detaches when a fold empties its workspace (phux-4r1, consumer-owned
policy under ADR-0015).

So a session cannot exist without a running process. That blocks two flows
from the Superlogical reconstruction
(`research/2026-09-09-superlogical-demo/GAP.md`, F16 and F17): creating a
session from a CLI and filling it later from a GUI, and treating "close the
last tab" as different from "kill the session". Cockpit already keeps
workspaces across close on the client side, but a session only one client
remembers is invisible to the CLI and to every other client.

## Decision

1. **Keep-empty is a per-session property.** It is set at creation (a
   `keep_empty` field on `phux.session.create/v1`) or later through a
   metadata key, and it is reported in `ls` and session listings. Sessions
   without it reap exactly as today.
2. **A create with no seed terminal makes a keep-empty session.**
   `phux new --empty` and the same request with `command` omitted and
   `empty: true` create a session with zero windows, since an empty session
   that reaps immediately would be meaningless.
3. **The cascade stops at a keep-empty session.** `reap_window_if_empty`
   removes the window and keeps the session. Removal is explicit only:
   `phux kill SESSION` or `KILL_RESOURCES` group teardown.
4. **Server lifetime still counts sessions.** A keep-empty session holds the
   server up with zero processes, the same way an idle one does today.
   `--exit-after-idle` remains the lever for ephemeral servers.
5. **Clients show an empty state and do not detach.** When the last pane of a
   keep-empty session closes, the TUI and Cockpit render "Empty session" with
   a New Window action, keeping the attach alive. Default sessions still
   detach.

The wire change is a new optional field and one metadata key. It lands with
the implementation under the usual spec, CHANGELOG, and version-bump rule.

## Why

Opting in per session is the only option that gives CLI/GUI-coherent
durable sessions without changing what `exit` means for everyone else.
Existing scripts, e2e tests and ADR-0063 harnesses depend on the cascade
tearing down a server when its last shell exits. Making the property part of
the session, not client configuration, means every client and the CLI agree
on whether the session still exists.

## Tradeoffs

- A server can live with zero processes. `ls` must mark empty sessions, or
  users will not see why the daemon is still running.
- Every surface gains a state it must render: the TUI, Cockpit, the
  web client and the agent verbs (for example, `phux wait` against an empty
  session).
- Two lifecycles to document and test instead of one.

## Alternatives

**Every session is durable.** This is simplest to explain, but it breaks
tmux muscle memory and the self-exit contract. Servers would effectively
never exit on their own.

**A global config switch.** One setting cannot let a user keep a long-lived
project session alongside throwaway ones, and a setting that differs
between machines makes scripts behave differently.

**Client-side placeholders only.** Cockpit already does this. The CLI cannot
see such a session, so `phux new` from a shell could never produce one a GUI
opens later.
