---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-10
---

# phux gap map against the Superlogical demo

**TL;DR.** Against `main` at 1358c0b6, phux already covers local persistence,
remote attach (`phux --remote`), remote splits over one link, rename, and live
lifecycle events in the CLI and TUI. The gaps cluster in four places: Cockpit
has no remote endpoint at all; no surface merges local and remote sessions
into one host-grouped selector; sessions cannot exist with zero tabs; and
there is no directory listing on the wire, so no directory picker. Identity
and login semantics (F06, F07, F12 to F14) are absent and partly conflict
with ADR-0003. Each gap is filed as a child of beads epic `phux-c2td`.

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
