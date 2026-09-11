---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-11
---

# phux gap map against the Superlogical demo

**TL;DR.** Against `main` at 1358c0b6, phux already covers local persistence,
remote attach (`phux --remote`), remote splits over one link, rename, and live
lifecycle events in the CLI and TUI. The gaps cluster in four places: Cockpit
has no remote endpoint at all; no surface merges local and remote sessions
into one host-grouped selector; sessions cannot exist with zero tabs; and
there is no directory listing on the wire, so no directory picker. Identity
and login semantics (F06, F07, F12 to F14) are absent and partly conflict
with ADR-0003. Each gap is filed as a child of beads epic `phux-c2td`. The
[status after the epic](#status-after-epic-phux-c2td) section records what
landed: every feature now has a surface on every client that can offer it,
except F06, F07 and F14 (out of scope by ADR-0106) and F11 and F24
(unchanged).

Feature IDs refer to the [reconstructed spec](SPEC.md). Status: **HAVE**,
**PART** (partial), **MISS** (missing). Citations are paths on `main`.

## Feature map

| ID | CLI / server | TUI | Cockpit | Evidence |
|---|---|---|---|---|
| F01 persistence | HAVE | HAVE | HAVE (Phux-backed panes) | `crates/phux/src/lib.rs` detach/attach; `clients/cockpit/README.md` |
| F02 palette + Add Host | PART (`phux host add`) | PART (palette, no host action) | PART (navigation switcher only) | `commands/host.rs`; `attach/action_registry.rs`; `clients/cockpit/src/core.ts` |
| F03 hostname connect | HAVE (`--remote`) | HAVE | MISS | `clients/cockpit/src/cockpit/startup.zig` admits only `.unix` |
| F04 remote splits | HAVE | HAVE | MISS | layout is transport-independent metadata |
| F05 one connection | HAVE | HAVE | MISS | one QUIC/WSS link; hub keeps one link per satellite |
| F06/F07 system login | MISS | MISS | MISS | ADR-0003 one server per user; PTYs are daemon children, no PAM/utmp |
| F08 host grouping, generated names | PART | PART | MISS | `commands/ls.rs`; names are `default`, `default-2` (`commands/new.rs`) |
| F09 rename from palette | HAVE | HAVE | MISS | `phux.session.name/v1`; `rename-session` palette row |
| F10 one local+remote selector | PART | PART | MISS | switcher is per-server; satellites not merged |
| F11 CLI inside shells | PART | PART | PART | `PHUX_SOCKET`/`PHUX_TERMINAL_ID` injected; binary via `host enroll` |
| F12/F13 whoami, effective user | MISS | MISS | MISS | `user@` is a label, not wire identity (`commands/remote_target.rs`) |
| F14 principal to user map | PART | n/a | MISS | credential principals in `phux-server/src/auth.rs`; SSH trust via `ssh://` |
| F15 remote `session new` from CLI | PART | n/a | MISS | headless verbs take `--socket` only; `spawn --satellite` makes a terminal |
| F16/F17 empty session, first tab | MISS | MISS | MISS | reap cascade deletes the session with its last window (`state/reap.rs`) |
| F18 reconnect restores remote | PART | HAVE | local only | server-side layout; Cockpit topology snapshots are local |
| F19 kill remote by ID | PART | n/a | local only | `host/@id` pane selectors via hub; sessions hub-local (`commands/kill.rs`) |
| F20 live propagation | HAVE | HAVE | HAVE (local) | `RESOURCE_SPAWNED`/`CLOSED`, `METADATA_CHANGED` |
| F21 local directory picker | MISS | MISS | MISS | only `-c/--cwd`; Cockpit binds Cmd+Shift+G to find-previous |
| F22 remote directory picker | MISS | MISS | MISS | no directory listing on the wire |
| F23 picker opens a new tab | PART | PART | PART | `SPAWN_RESOURCE.cwd` exists; no picker |
| F24 Mosh-like input | PART | HAVE (opt-in predictive echo) | MISS | QUIC migration; SSP residual is `phux-205m` |

## What transfers

The idea worth taking is host-aware *context*: the selector, new-session,
new-tab and directory commands all act on the active host. phux has the
parts (satellites, hub selectors, `--remote`, `SPAWN_RESOURCE.cwd`) but
not one context those verbs share. Order of leverage:

1. **Directory listing on the wire, then a picker** (`phux-c2td.1`).
   This is new L3 surface, so it needs a spec change. It unlocks F21 to
   F23 on every surface and works through the hub for satellites.
2. **Remote targeting for headless verbs** (`phux-c2td.2`): `ls`, `new`,
   `kill` and `rename` with the `--remote` resolution attach already uses.
   Covers F15 and F19.
3. **Host-grouped selector** (`phux-c2td.3`) that merges hub-local and
   satellite sessions in the TUI switcher and the Cockpit catalog. Covers
   F08 and F10.
4. **Empty sessions** (`phux-c2td.4`) that survive their last window.
   This changes the reap cascade and `session.create`, so it needs an ADR.
   Covers F16 and F17.
5. **Cockpit remote endpoint** (`phux-c2td.5`), a QUIC/WSS provider. This
   is the largest item and unblocks the Cockpit column of F03 to F05, F10
   and F18.
6. **Generated human-readable session names** (`phux-c2td.6`) as an
   opt-in template token. Small, and only a CLI change.
7. **`phux whoami`** (`phux-c2td.7`) reporting the server principal, auth
   route and serving OS user. Effective-user mapping and real login
   sessions (F06, F07, F14) need an ADR that reconciles them with ADR-0003
   before any code.

Not recommended: copying the demo's Cmd+Shift+G binding into Cockpit, which
already uses it for find-previous.

## Status after epic phux-c2td

The map above is the starting point (`main` at 1358c0b6). This is the state
after the epic's work landed, with the commits that delivered each surface.

| ID | CLI / server | TUI | Cockpit | Landed in |
|---|---|---|---|---|
| F01 persistence | HAVE | HAVE | HAVE, remote hosts included | 10c88e0e, 980282f8 |
| F02 palette + Add Host | HAVE (`phux host add`) | PART (no host action) | HAVE (Connect to Host, Cmd+Shift+O) | 10c88e0e, 057cf9f6 |
| F03 hostname connect | HAVE | HAVE | HAVE (QUIC/WSS remote tunnel) | 10c88e0e |
| F04 remote splits | HAVE | HAVE, satellite splits too | HAVE | 327cc43a, 10c88e0e |
| F05 one connection | HAVE | HAVE | HAVE (one tunnel per coordinator) | 10c88e0e |
| F06/F07 system login | out of scope | out of scope | out of scope | ADR-0106 |
| F08 host grouping, generated names | HAVE (grouped `ls`, `${random-name}`) | HAVE (grouped picker) | HAVE (grouped by coordinator) | 9d7956e0, 921080fc, be903928 |
| F09 rename from palette | HAVE | HAVE | HAVE (Rename Session, sent to the owning coordinator) | b354fae5 |
| F10 one local+remote selector | HAVE (hub lists satellite sessions) | HAVE (satellite session opens its active pane) | HAVE (up to four coordinators side by side, edits routed to the owner) | 921080fc, bba83a59, be903928, 1c692aa7 |
| F11 CLI inside shells | PART | PART | PART | unchanged |
| F12/F13 whoami, effective user | HAVE (`phux whoami [--remote]`, `ssh-stdio` route) | n/a | n/a (MCP `phux_whoami`) | f630b03b, 79b54785, 712259a3 |
| F14 principal to user map | out of scope | n/a | n/a | ADR-0106 |
| F15 remote `session new` from CLI | HAVE (`--remote` on `ls`, `new`, `kill`, `rename`, `detach`) | n/a | n/a | c3cf97cc |
| F16/F17 empty session, first tab | HAVE (`phux new --empty`) | HAVE (Empty session state) | HAVE (Empty session panel with New Tab) | 64586db6, aff34049 |
| F18 reconnect restores remote | HAVE | HAVE | HAVE (remembered hosts restore; the front peer's session is re-shown) | 10c88e0e, 980282f8, 0a87a6eb |
| F19 kill remote by ID | HAVE (`phux kill --remote`) | n/a | n/a | c3cf97cc |
| F20 live propagation | HAVE | HAVE | HAVE | unchanged |
| F21 local directory picker | HAVE | HAVE (`go-to-directory`, `C-a G`) | HAVE (Cmd+Shift+J) | b37c4d9f, abe9e9a9, 15b6d549 |
| F22 remote directory picker | HAVE (through `--remote`; satellites via the hub) | HAVE | HAVE | f3521b6e, 0ef32327, d7e7e7eb |
| F23 picker opens a new tab | HAVE | HAVE | HAVE | b37c4d9f, 15b6d549 |
| F24 Mosh-like input | PART (SSP residual `phux-205m`) | HAVE (opt-in predictive echo) | MISS | unchanged |

F06, F07 and F14 are out of scope by decision rather than left undone:
ADR-0106 keeps one server per OS user, so the effective user is chosen by
reaching that user's own server (`root@host`), and login semantics stay with
the service manager.

### Decisions recorded

- ADR-0105: a session can be marked keep-empty and outlive its last window.
- ADR-0106: identity is the serving user; `phux whoami` reports it.
- ADR-0107: satellite sessions are listed, never adopted into the hub's ids.
- ADR-0108: a hub relays host queries to satellites per request.
- ADR-0109: late kills are conditional on the satellite's instance and on no
  one else having attached or used the pane.
- ADR-0110: at launch, a showing peer is re-shown only if its tab was in
  front, and only after a real frame has measured the window.

### Safety work beyond the demo

- e2723adb: the server keeps its `PHUX_UPGRADE_*` handoff private, so a pane
  of an upgraded server no longer makes later servers re-exec into the wrong
  binary.
- a90f5c8a, 92f193a2, 0b347779, 1e6aeaf1: a satellite pane whose attach is
  refused is cleaned up, ending in a kill the satellite itself refuses if the
  pane was used or the satellite restarted.

### Still open under the epic

- `phux-c2td.17`: live Cockpit acceptance against a real enrolled remote host.
- `phux-c2td.32`: relaunch restore beyond the front host, surviving a server
  restart, and the untested ordering race.

Outside the epic, `phux-q0i3` tracks a regression it introduced: Cockpit built
without the Phux FFI no longer compiles, and CI never builds that
configuration.
