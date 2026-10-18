---
name: using-phux-muse
description: Drives phux persistent terminals as Muse via the muse-lib.sh shell library. Use when Muse needs terminal state to survive across steps, an interactive program, a dev server, or coordination with other agents through phux; prefer a one-shot shell for independent commands.
compatibility: Requires the phux CLI and jq on PATH. Muse runs headless: there is no terminal UI and no plugin host.
metadata:
  version: "0.2.0"
---

# Using phux as Muse

Source the library, then drive phux with short functions. Every function
prints its result on stdout and diagnostics on stderr; every wait carries
a finite timeout (default 20s, override with `MUSE_DEFAULT_TIMEOUT`).

```sh
. ./scripts/muse-lib.sh   # from integrations/muse/

muse_init            # ensure phux + server (idempotent, auto-starts)
s=$(mspawn mysess)   # spawn ephemeral session, prints name
mrun "$s" "make -j"  # run a command, prints its output
mwait "$s" "done"    # wait for text with finite timeout
msnap "$s"           # snapshot pane text
mls                  # list sessions, one name per line
mkill "$s"           # kill session, no prompt
mdo job "cmd"        # spawn, run, snapshot, kill — one shot
```

## When to use phux

Use phux when terminal state or a process must survive across steps:
REPLs, debuggers, dev servers, interactive CLIs, long builds, or any
multi-agent terminal workflow. Use a one-shot shell call when one command
can run and exit in a single call.

## Setup

```sh
./scripts/setup.sh   # verifies phux, jq, server, pinned toolchain
./scripts/smoke.sh   # end-to-end drive test through the library
```

`muse_init` (and `setup.sh`) start the per-user server when it is not
running and wait for the socket. No manual daemon management.

If the checked-in skill and an installed binary differ, trust the binary:

```sh
phux --skill=quick
phux help <verb>
phux runtime-info --json
```

## Raw CLI workflow

When the library does not cover an operation, use the CLI directly. These
are the verified forms — do not guess flag spellings:

1. **Discover.** `phux agent list --json` for exact pane selectors and
   ownership; `phux ls --json` lists sessions. If `PHUX_TERMINAL_ID` is
   set, never send input to that pane: it hosts you, not a shell.
   `PHUX_SOCKET` already selects its server.
2. **Choose one target.** Prefer a returned `@N` (or satellite `host/@N`)
   for writes. `name`, `name:W`, `name:W.P`, and `#tag` may select sets;
   `.` means the focused session; `%name` identifies one named agent.
   Headless `=` is refused because focus history belongs to an attached
   client.
3. **Read.** `phux snapshot --json @N` before acting.
4. **Act.** `phux run --json --timeout SECS @N "COMMAND"` for a discrete
   command. `send-keys` for interactive keys, `paste` for multiline text.
   Put flags before the target so they are not swallowed as input.
5. **Observe under a finite bound.** `wait --until TEXT --timeout SECS`
   for screen conditions, `watch` for events, `agent wait` for lifecycle
   transitions, `resource wait` for a process's exit (spawn with
   `--retain` so a finished run keeps its status). Exit 124 means an
   observation timed out; `phux run` reserves 125 for its own timeout.
6. **Verify.** Snapshot or list state again. A quiet pane, a successful
   write, or an ended watcher is not proof of completion — completion
   requires an observed transition.

Session creation: `phux new --json -s NAME --ephemeral` (create-only,
no attach; `--json` requires `-s`). Destruction: `phux kill --yes
TARGET` (the `--yes` skips the confirmation prompt).

With `--json`, branch on structured fields and `error.code`, not prose.
Parse JSON with `jq`, never with shell pattern matching.

## Muse-specific notes

- **Headless.** Muse has no terminal UI. Never assume a human is watching
  a pane; never wait on human input. All waits carry timeouts.
- **Server lifecycle.** The server persists beyond one conversation. Name
  sessions after their purpose (`muse-build-phux`, not `tmp1`) so later
  sessions — yours or another agent's — can find and reuse them. Clean up
  ephemeral sessions when done (`mkill`, or `phux kill --yes`).
- **Identity.** When working in a shared checkout, declare yourself the
  way the other integrations do: you speak as your own pane. Coordinate
  through `phux_mail_send` / `phux_mail_read` / `phux_mail_ack` for
  messages and `phux_claim_take` / `phux_claim_renew` / `phux_claim_release`
  for shared paths. Never send keys into another agent's pane.
- **Destructive operations.** `phux_kill`, `phux_rm`, and `phux_relaunch`
  terminate real processes. Prefer exact `@N` selectors; session names and
  tags may select sets.

## Safety

- A local timeout or cancellation stops observation, not the terminal
  process or input already sent. Re-observe before deciding what to do;
  do not repeat a command because its response was lost.
- `delivery_unknown` is terminal: inspect the pane, do not resend — the
  first operation may still land.
- Serialize prompts to one pane; different panes may be driven in parallel.
- A paste inserts text but does not submit it.
