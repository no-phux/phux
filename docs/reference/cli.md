---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux CLI reference

**TL;DR.** The complete `phux` command surface: one section per invocation path, each carrying the exact long help of the binary that generated it. Rendered from the argument parser itself, so the flags, defaults, and descriptions shown here are the ones the binary enforces.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Each section below is the verbatim `--help` text for one invocation path, rendered by the same argument parser the binary runs — flags, defaults, value names, and descriptions here are the ones the binary enforces. Hidden internal subcommands are omitted, exactly as they are from `--help` itself.

## `phux`

```text
phux — a libghostty-backed terminal multiplexer and control plane.

Run `phux` with no arguments to attach to your session (auto-starting a
server if needed). The control verbs below read and drive panes without a
TTY, and most accept `--json` for clean, scriptable output.

ATTACH / SERVE
  attach     Attach to a session (interactive)
  server     Run a server in the foreground
  host       Register the machines phux talks to: remotes and satellites
  service    Keep a server running across logout and reboot
  update     Update phux to the latest release, keeping sessions alive
  upgrade    Hot-swap the running server binary, keeping sessions alive

INSPECT
  ls         List sessions
  status     Report the running server: pid, uptime, protocol, clients, logs
  snapshot   Capture a pane's screen as JSON or a boxed view
  watch      Stream a pane's live events (bell, title, output, lifecycle)
  rec        Record a pane to an asciinema cast, a GIF, or an APNG
  play       Play a recording back as a live pane
  agent      List, show, explain, set, or clear per-pane agent state

DRIVE
  new        Create a session
  kill       Kill a session, window, or pane
  detach     Detach clients from a session
  insert-pane Insert an already-created pane into a layout
  move-pane  Move an existing pane beside another, across sessions too
  swap-pane  Swap two existing pane leaves
  rename     Rename a session
  resize     Set a pane's grid size, with no TTY
  send-keys  Send keys to a pane
  paste      Paste text into a pane (bracketed when the pane asks)
  run        Run a command in a pane and capture its exit code
  wait       Block until a pane meets a condition
  ask        Report an agent ask event for a pane

SUPERVISE
  take       Seize exclusive input authority over a pane
  give       Release the input authority taken with `take`
  signal     Send a POSIX signal to a pane's process group

ORGANIZE
  tag        Read and write a pane's tags (address them with #tag)
  skill      Print the agent skill this binary ships with
  completion Print a shell completion script for phux
  doctor     Diagnose the install: config, socket, server, plugins
  logs       Show where phux's logs live, or tail one of them
  config     Inspect config and run configured plugin actions
  plugin     Manage local plugin manifests in config
  workspace  Inspect worktrees and save/restore session archives
  worktree   Create, open, list, and remove worktree-bound sessions

FEDERATION
  pair       Mint a pairing token for a remote consumer
  relay      Run a standalone relay, or enroll a route with it

TARGET is the selector grammar: a session name, `name:window`,
`name:window.pane`, `@id`, or `.` (focused). `=` is reserved for the attached TUI's client-local focus MRU. The same
grammar works across kill/snapshot/send-keys/run/wait/ask.

Usage: phux [OPTIONS] [COMMAND]

Commands:
  attach       Attach to a session (interactive) [aliases: a]
  server       Run a phux server in the foreground
  ls           List sessions on the running server [aliases: list]
  status       Report the running server: pid, up since, protocol, clients, logs
  new          Create a new session and attach to it
  spawn        Spawn a Terminal without attaching (`SPAWN_TERMINAL`)
  launch       Launch an agent integration in a new pane
  kill         Kill a session, window, pane, or the server itself
  insert-pane  Insert an already-created pane into a session layout
  move-pane    Move one existing pane beside another, even across sessions
  swap-pane    Swap two existing pane leaves in the same session layout
  resize       Set a pane's grid size, with no TTY
  detach       Detach clients from a session, from outside the attach UI
  take         Take the input wheel of a pane
  give         Give back the input wheel of a pane
  signal       Signal a pane's process group
  update       Update phux to the latest release, keeping sessions alive
  upgrade      Graceful-upgrade the running server in place
  rename       Rename a session
  snapshot     Capture a pane's screen as JSON or a boxed text view
  send-keys    Send keys to a pane
  paste        Paste text into a pane (bracketed when the pane asks for it)
  wait         Block until a pane meets a condition
  watch        Stream a pane's live events (bell, title, dirty/idle, lifecycle)
  rec          Record a pane and export it as a cast, GIF, or APNG
  play         Play a recording back as a live pane
  ask          Report an agent ask event for a pane
  agent        List, show, explain, set, or clear per-pane agent state
  run          Run a command in a pane and capture its exit code
  config       Inspect, scaffold, and reload the phux config file
  plugin       Manage local plugin manifests in the phux config registry
  workspace    Inspect a git workspace and its worktrees for agent orchestration
  tag          Read and write a Terminal's L3 tags
  relay        Run a standalone relay, or enroll a route with it
  pair         Mint a pairing token for a remote consumer
  host         Register the machines phux talks to: remotes and satellites
  service      Keep a server running across logout and reboot
  completion   Print a shell completion script on stdout
  skill        Print the agent skill this binary ships with, on stdout
  doctor       Diagnose a phux install: config, socket, server, plugins
  worktree     Manage git worktrees and the sessions bound to them
  logs         Show where phux's logs live, or tail one of them
  help         Print this message or the help of the given subcommand(s)

Options:
      --rec <PATH>
          Record this session while it runs and write the result to PATH.

          The format follows the extension (.cast, .gif, .png, .apng); pass
          --rec-format to override. A path with no extension gets `.gif`.

          Examples:
            phux --rec demo.gif
            phux attach work --rec demo.cast

      --rec-format <FMT>
          Output format for --rec, overriding the extension

          Possible values:
          - cast: asciinema cast — the archival, re-renderable artifact
          - gif:  Animated GIF — shareable and embeddable anywhere
          - apng: Animated PNG — truecolor, no quantization, larger files

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

EXIT STATUS
  0     Success.
  1     Failure: no server, no such target, or the verb itself failed.
  2     Usage error, or the server refused the request.
  3     Unanswerable: the selector was resolved against a partial view
        of the fleet (a federation satellite was unreachable). Retry
        once the link is back — unlike 1, the target may exist.
  124   `phux wait` gave up because `--timeout` expired.
  125   `phux run` gave up because `--timeout` expired; otherwise
        `run` mirrors the exit code of the command it ran, so
        `phux run … && next` composes like a shell.

ENVIRONMENT
  PHUX_SOCKET        UDS path for the CLI verbs and the server. A `--socket`
                    flag overrides it; default is
                    $XDG_RUNTIME_DIR/phux/phux.sock (or /tmp/phux-$USER/...).
  PHUX_WS_ADDR       Also accept WebSocket clients on HOST:PORT. Equivalent to
                    `phux server --listen`, which overrides it.
  PHUX_WS_SECURE     Force TLS + token auth on a loopback --listen address
                    (exercise the remote path locally).
  PHUX_WS_TLS_CERT   Operator-supplied server cert/key (PEM), instead of the
  PHUX_WS_TLS_KEY    auto-provisioned self-signed pair used off-loopback.
  PHUX_WS_TOKENS     Pairing-token store the server reads and `phux pair` writes.
  PHUX_QUIC_ADDR     Also accept QUIC clients on HOST:PORT. Equivalent to
                    `phux server --quic`, which overrides it.
  PHUX_WT_ADDR       Also accept WebTransport (HTTP/3 over QUIC) clients on
                    HOST:PORT. Equivalent to `phux server --webtransport`.
  PHUX_SSH           OpenSSH-compatible program a federation hub spawns to
                    dial ssh:// satellites (default: `ssh` on PATH).
  PHUX_TAILSCALE     Tailscale-compatible CLI `phux pair` runs to detect the
                    overlay address (default: `tailscale` on PATH).
  PHUX_LOG           Write logs to this file (server tees; client writes here).
  PHUX_LOG_FORMAT    text (default) or json — log line format.
  RUST_LOG           tracing level filter, e.g. phux=debug.

Run `phux server --listen 127.0.0.1:8787` to expose a port; see
  `phux help server` for the remote/TLS details.
```

## `phux agent`

```text
List, show, explain, set, or clear per-pane agent state.

Inference (`list`/`show`/`explain`) reports the agent phux infers is running in each pane. `set`/`clear` write and delete an explicit per-pane agent identity that overrides inference.

Usage: phux agent [OPTIONS] <COMMAND>

Commands:
  list              List inferred agent state for every pane [aliases: ls]
  show              Show inferred state for one pane
  explain           Explain the evidence behind one pane's state
  set               Declare a pane's agent identity (writes the phux.agent/v1 L3 record)
  wait              Block until a pane's agent TRANSITIONS into a lifecycle state
  prompt            Hand an agent a turn's worth of work, with a delivery receipt
  send-keys         Send keys to a pane, but only if it still hosts the expected agent
  answer            Answer a pane's pending agent question by validated choice
  start             Start an agent INSIDE an existing shell pane, and return when it is ready for input
  clear             Clear a pane's declared agent identity (deletes phux.agent/v1)
  install-claude    Make plain `claude` launch inside phux and declare its identity
  uninstall-claude  Remove the claude-in-phux shim and shell activation
  help              Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux agent answer`

```text
Answer a pane's pending agent question by validated choice.

The `asked` event carries the question AND the suggestions the asking agent itself published, so an orchestrator can reply with a string the agent named instead of a blind keystroke. That is the contract: the bytes phux types are always one of the agent's own published answers, unless you pass `--allow-unlisted`.

`--id` is required, and the pane must still be asking that exact question. Answering one the agent already moved past would type into whatever is on screen now, which is the failure this verb exists to prevent — so a stale id, an unidentified ask, and a pane that is not asking at all are all refusals with nothing written.

The answer rides one acknowledged, idempotent input batch: a trusted paste followed by Enter, written and confirmed as a single operation.

Usage: phux agent answer [OPTIONS] --id <ID> <TARGET>

Arguments:
  <TARGET>
          Target selector (resolves to one pane)

Options:
      --id <ID>
          The id of the ask being answered, as carried by the `asked` event. Required: answering "whatever is being asked right now" is a level read, and a level read cannot tell one question from the next

      --choice <N>
          Send the Nth published suggestion, 1-based, verbatim

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --text <TEXT>
          Send exactly this text. Refused when the ask published a suggestion set and this is not in it (see `--allow-unlisted`)

      --allow-unlisted
          Permit a `--text` answer outside the ask's published suggestions

      --json
          Emit machine-readable JSON instead of the one-line confirmation

  -h, --help
          Print help (see a summary with '-h')
```

## `phux agent clear`

```text
Clear a pane's declared agent identity (deletes phux.agent/v1)

Usage: phux agent clear [OPTIONS] [TARGET]

Arguments:
  [TARGET]
          Target selector (resolves to one pane). Omit for the focused pane

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux agent explain`

```text
Explain the evidence behind one pane's state.

With `--file` this runs OFFLINE: it evaluates the compiled detection manifests against a captured screen and contacts no server at all. That is the mode for authoring and debugging a manifest, because it prints the text every region resolved to on that screen — a rule scoped to a region that comes back empty can never match, and nothing else makes that visible (the detector fails safe to `idle`, silently).

The capture is `phux snapshot --json` output or a plain text screen, one viewport row per line; `-` reads stdin. A capture carries no OSC title, so pass `--title` to exercise title-scoped rules.

Usage: phux agent explain [OPTIONS] [TARGET]

Arguments:
  [TARGET]
          Target selector (resolves to one pane). Omit for the focused pane. Not used in offline (`--file`) mode

Options:
      --json
          Emit machine-readable JSON instead of the table

      --file <PATH>
          Evaluate a captured screen offline instead of querying the server. `-` reads stdin

      --kind <KIND>
          Agent kind whose manifest to evaluate, or one of its binary aliases. Required with `--file`: offline there is no foreground process group to identify the agent from

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --title <TEXT>
          OSC 0/2 title to evaluate `title`-scoped rules against. Captures do not carry one, so it defaults to empty

      --format <FORMAT>
          How to read `--file`. `auto` picks JSON when the first non-whitespace byte is `{`

          [possible values: auto, json, text]

  -h, --help
          Print help (see a summary with '-h')
```

## `phux agent install-claude`

```text
Make plain `claude` launch inside phux and declare its identity

Usage: phux agent install-claude [OPTIONS]

Options:
      --shell <SHELL>
          Shell rc file to activate (auto-detected from SHELL)

          [possible values: zsh, bash, fish]

      --real <PATH>
          Absolute path to the real Claude executable (auto-detected from PATH)

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux agent list`

```text
List inferred agent state for every pane

Usage: phux agent list [OPTIONS]

Options:
      --json
          Emit machine-readable JSON instead of the table

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux agent prompt`

```text
Hand an agent a turn's worth of work, with a delivery receipt.

The prompt text and Enter ride ONE acknowledged, idempotent operation, so a caller that does not get an answer can ask again under the same operation id without risking a duplicate turn — the failure fire-and-forget input cannot avoid, because its only recovery is a resend. Enter is last, so a partial write can only drop the submission and leave unsubmitted text, never submit a truncated prompt.

The acknowledged path is required, not preferred: an older server or a satellite target is refused rather than downgraded, because a success code that means "the bytes are in the kernel queue" on one host and "accepted, maybe dropped" on another is not branchable.

An OK is a kernel-queue receipt, not a consumption receipt. If delivery comes back UNKNOWN, do not resend: read the pane.

With `--wait` the same process holds one connection across the submit, so every state change it sees is strictly post-write, and the gate is satisfied only by an observed TRANSITION — never by a level read of the current state, which a crashed agent also reads as.

The server has ONE acknowledged input lane, so do not prompt a fleet in parallel: serialize it, or all but one caller collides.

Usage: phux agent prompt [OPTIONS] <TARGET> <TEXT>

Arguments:
  <TARGET>
          Target selector (resolves to one pane)

  <TEXT>
          The prompt text. Single-line: a raw newline is refused, because a pane that has not enabled bracketed paste turns each one into a separate submission and no client can observe that mode

Options:
      --expect-agent <NAME>
          Require the pane's declared agent name to be this one

      --expect-kind <KIND>
          Require the pane's declared agent kind slug to be this one

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --wait
          After delivering, block until the agent transitions into a lifecycle state

      --until <STATE>
          Lifecycle state to wait for; repeat to OR several. Defaults to `idle`, `blocked`, `done`. Requires `--wait`

          [possible values: idle, working, blocked, done]

      --timeout <SECS>
          Give up waiting after this many seconds and exit 124. The prompt was still delivered. Requires `--wait`

      --json
          Emit the machine-readable result document instead of staying quiet on success

  -h, --help
          Print help (see a summary with '-h')
```

## `phux agent send-keys`

```text
Send keys to a pane, but only if it still hosts the expected agent.

The agent-addressed sibling of top-level `phux send-keys`, and it differs from it in exactly one way: it re-reads the pane's phux.agent/v1 record immediately before writing and refuses if the occupant changed. `phux send-keys` addresses a pane and deliberately checks no identity; use that one when a pane is what you mean.

Every key is validated before any byte is written, so a typo in the third key cannot leave the first two delivered — and since the whole batch now rides ONE acknowledged operation, that all-or-nothing promise covers delivery as well as validation. A caller that loses the answer can ask again under the same operation id instead of guessing whether the keys landed. For prose you want an agent to act on, `phux agent prompt` is the verb.

Usage: phux agent send-keys [OPTIONS] <TARGET> <KEYS>...

Arguments:
  <TARGET>
          Target selector (resolves to one pane)

  <KEYS>...
          Key specs: named keys (`Enter`, `C-c`, `M-x`, `Up`) or literal text. A literal run immediately before `Enter` is sent as one submission-safe paste

Options:
      --expect-agent <NAME>
          Require the pane's declared agent name to be this one

      --expect-kind <KIND>
          Require the pane's declared agent kind slug to be this one

      --json
          Emit machine-readable JSON instead of staying quiet on success

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux agent set`

```text
Declare a pane's agent identity (writes the phux.agent/v1 L3 record)

Usage: phux agent set [OPTIONS] --name <NAME> [TARGET]

Arguments:
  [TARGET]
          Target selector (resolves to one pane). Omit for the focused pane

Options:
      --name <NAME>
          Human-facing agent name (required, non-empty)

      --kind <KIND>
          Open-vocabulary kind slug, e.g. "claude" or "codex"

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --state <STATE>
          Declared lifecycle state

          [possible values: unknown, idle, working, blocked, done]

      --attention <ATTENTION>
          Declared attention priority (defaults derive from state)

          [possible values: none, low, normal, high]

      --session <SESSION>
          Free-form association label (fleet/job name)

  -h, --help
          Print help
```

## `phux agent show`

```text
Show inferred state for one pane

Usage: phux agent show [OPTIONS] [TARGET]

Arguments:
  [TARGET]
          Target selector (resolves to one pane). Omit for the focused pane

Options:
      --json
          Emit machine-readable JSON instead of the table

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux agent start`

```text
Start an agent INSIDE an existing shell pane, and return when it is ready for input.

The layout-free sibling of `phux launch`: it creates, splits, and moves nothing. `launch` returns a Terminal id ("a pane now exists"); this returns a readiness assertion about a pane that already existed, which is a different success statement and therefore a different verb. The launch resolver is shared — the same integration template, the same argv, the same provider-native session identity — only the delivery differs: the pane's child is a live shell, so the command is typed as one quoted line and submitted as one acknowledged `APPLY_INPUT` batch.

Ready is the FIRST detector publication after submit, not `state == idle`. No shipped detection manifest asserts `idle` positively — it is the fail-safe fallthrough — so a gate built on it would report ready for a pane where nothing launched. `--json` therefore reports the provenance of the answer (which rule matched, or that none did) rather than an opaque word.

A `--kind` with no detection manifest is refused up front: without one the readiness contract is unenforceable and the verb could only time out, after having typed into the pane. `phux launch` and `phux spawn` keep working for any agent whatsoever, because neither promises readiness.

Usage: phux agent start [OPTIONS] --kind <KIND> --target <TARGET> <NAME> [-- <ARGS>...]

Arguments:
  <NAME>
          Human-facing agent name to bind to the pane. Must match `^[a-z][a-z0-9_-]{0,31}$` so `%NAME` can address it afterwards

  [ARGS]...
          Extra arguments appended to the integration's launch command

Options:
      --kind <KIND>
          Detection-manifest kind the started agent must identify as (`claude`, `codex`, ...). `phux agent explain --file` lists the loaded roster

      --target <TARGET>
          Existing pane to start into. Never created, split, or moved

      --integration <ID>
          Launch integration id, when it is not spelled like the kind slug (e.g. `--kind claude --integration claude-code`)

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --timeout <SECS>
          Give up waiting for readiness after this many seconds and exit 124. The command was still typed

      --no-wait
          Submit and return without claiming readiness (exit 0, `ready: false`)

      --force
          Skip the available-shell precondition. Types the launch command into the pane whatever is running there

      --json
          Emit the machine-readable result document instead of a line

  -h, --help
          Print help (see a summary with '-h')
```

## `phux agent uninstall-claude`

```text
Remove the claude-in-phux shim and shell activation

Usage: phux agent uninstall-claude [OPTIONS]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux agent wait`

```text
Block until a pane's agent TRANSITIONS into a lifecycle state.

Satisfied only by an observed transition, never by a level read of the current state. That distinction is the point of the verb: `idle` is the detector's fail-safe fallthrough — no shipped detection manifest asserts it positively — so it is equally true of a finished agent, a half-painted TUI, a crashed agent, and a pane running `less`. A gate that fired on that level would return success on a corpse, instantly, and on any pane with no manifest at all.

The consequence is deliberate: a pane already resting in a target state when the wait begins times out (124) rather than succeeding. `phux agent show` is the level read; this verb reports transitions.

Subscribes before reading the baseline, so no transition is lost in between, and re-reads on the `phux wait` cadence to recover an edge a dropped notification never delivered. A record that goes away mid-wait ends it as a departure (exit 1), which is not a completion.

Usage: phux agent wait [OPTIONS] [TARGET]

Arguments:
  [TARGET]
          Target selector (resolves to one pane). Omit for the focused pane

Options:
      --until <STATE>
          Lifecycle state to wait for; repeat to OR several. Defaults to `idle`, `blocked`, `done` — the three ways a turn ends. `unknown` is not spellable: it is departure, not a state

          [possible values: idle, working, blocked, done]

      --timeout <SECS>
          Give up after this many seconds and exit 124. Unbounded when omitted, matching `phux wait` — always pass one in a script

      --json
          Emit the machine-readable result document instead of a line

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux ask`

```text
Report that an agent in a pane is waiting on a human answer.

This is the opt-in hook contract for configured integrations: it emits the same `asked` event as the `phux-ask` title sentinel without writing escape sequences into the target terminal. TARGET is resolved client-side and the command neither attaches nor resizes the pane.

Examples:
  phux ask work:1.0 --id deploy --suggest Yes --suggest No "Deploy?"
  phux ask @3 --json "Need approval"

Usage: phux ask [OPTIONS] <TARGET> <QUESTION>

Arguments:
  <TARGET>
          Target selector: session, session:window, session:window.pane, @id, or `.` (focused). `=` is unsupported by headless commands

  <QUESTION>
          Human-facing question text

Options:
      --id <ID>
          Stable question id for answer correlation

          [default: ""]

      --suggest <TEXT>
          Suggested answer. Repeat to preserve display order

      --elapsed-seconds <SECS>
          Seconds the agent has already been waiting

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux attach`

```text
Attach to a session (interactive).

With no name, attaches to the most-recently-focused session, auto-spawning a server if none is running. Requires a TTY.

A name enrolled in the host registry (`phux host enroll`, `phux host add`) shadows a local session of the same name: `phux attach NAME` dials the registered host instead of the local socket. Pass `--socket` to force the local reading of the name.

Usage: phux attach [OPTIONS] [SESSION]

Arguments:
  [SESSION]
          Session name (matches the name used at creation time).

          Omit to attach to the most-recently-focused session.

Options:
      --quic <HOST:PORT>
          Attach over QUIC to a remote `phux server --quic` listener at this `HOST:PORT` instead of the local Unix socket. HOST may be an IP literal or a DNS name (e.g. a Tailscale `MagicDNS` name), resolved before dialing. QUIC is always TLS 1.3-encrypted. A target resolving to loopback trusts the server's self-signed cert for local dev; any routable address requires `--cert-fingerprint` (the value `phux pair` prints on the server host)

      --ws <URL>
          Attach over WebSocket to a `phux server --listen` endpoint. Use `ws://HOST:PORT` for loopback dev, or `wss://HOST:PORT` with `--token` and `--cert-fingerprint` for routable remote attach. This is the TCP fallback when UDP/QUIC is blocked

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --token <TOKEN>
          Bearer pairing token (hex) for an authenticated QUIC listener, as minted by `phux pair`. QUIC sends it as the stream's opening preamble; WebSocket sends it as `Authorization: Bearer`. Requires `--quic` or `--ws`

      --cert-fingerprint <FP>
          Pin the QUIC server's certificate by its SHA-256 fingerprint (the value `phux pair` prints). Required to dial any non-loopback `--quic`/`--ws wss://` address. Requires `--quic` or `--ws`

      --tls-server-name <NAME>
          TLS server name (SNI) to offer the remote listener. QUIC defaults to `localhost`; WebSocket defaults to the URL host. Requires `--quic` or `--ws`

      --rec <PATH>
          Record this session while it runs and write the result to PATH.

          The format follows the extension (.cast, .gif, .png, .apng); pass
          --rec-format to override. A path with no extension gets `.gif`.

          Examples:
            phux --rec demo.gif
            phux attach work --rec demo.cast

      --rec-format <FMT>
          Output format for --rec, overriding the extension

          Possible values:
          - cast: asciinema cast — the archival, re-renderable artifact
          - gif:  Animated GIF — shareable and embeddable anywhere
          - apng: Animated PNG — truecolor, no quantization, larger files

  -h, --help
          Print help (see a summary with '-h')
```

## `phux completion`

```text
Print a shell completion script on stdout.

The script is generated from the binary's own argument parser, so it always matches the verbs this build actually accepts. It contacts no server and reads no config, which is what makes it safe to run from a shell startup file.

Regenerate after upgrading phux; a stale script completes verbs the installed binary no longer has.

Install it the way your shell prefers. Examples:
  phux completion zsh  > ~/.zfunc/_phux   (~/.zfunc must be on $fpath)
  phux completion bash > ~/.local/share/bash-completion/completions/phux
  phux completion fish > ~/.config/fish/completions/phux.fish

Usage: phux completion [OPTIONS] <SHELL>

Arguments:
  <SHELL>
          Shell dialect to generate for

          [possible values: bash, elvish, fish, powershell, zsh]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux config`

```text
Inspect, scaffold, and reload the phux config file.

phux is config-driven: defaults ship in the binary and your `config.toml` is a sparse overlay merged on top. These subcommands never touch a running server, except `reload`, which signals attached clients to re-read their config in place.

Usage: phux config [OPTIONS] <COMMAND>

Commands:
  init     Write a commented starter config to the canonical path
  path     Print the resolved config path. Pure path math — prints the path whether or not the file exists
  check    Validate the config and report every problem, with full key paths
  show     Print the effective config (shipped defaults + your overrides) as TOML. With `--default`, print the shipped defaults verbatim instead, ignoring any user config. With `--layers`, print which layer of the `extends` stack set each effective key instead of the values
  plugins  List plugin manifests declared by `[[plugins]]`
  agents   List agent states from configured plugin manifests, merged with live `phux.agent/v1` records when a server is running
  reload   Re-read the layered config and apply it to running clients in place
  run      Execute one action declared by a configured plugin manifest
  help     Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux config agents`

```text
List agent states from configured plugin manifests, merged with live `phux.agent/v1` records when a server is running

Usage: phux config agents [OPTIONS]

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux config check`

```text
Validate the config and report every problem, with full key paths.

The loader already refuses an unknown key, but it names only the leaf field (`unknown field 'enabledd'`) and stops at the first one. This reports `sidebar.enabledd`, names the layer file that introduced it, and finds every problem in one pass — so a config with four typos takes one edit, not four.

Exits 0 when clean and 1 when anything was found, so it can gate a dotfiles CI run.

Usage: phux config check [OPTIONS] [PATH]

Arguments:
  [PATH]
          Config file to check. Defaults to the resolved config path

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux config init`

```text
Write a commented starter config to the canonical path.

The file is the shipped defaults, fully commented out: inert until you uncomment a line, so the binary's defaults stay authoritative. Refuses to overwrite an existing config unless `--force`.

With `--distro`, the scaffold additionally carries one active `extends` line layering the named starter distribution (a bundled name like `herdr`, or a path to a distro layer `.toml`) between the shipped defaults and your file.

Usage: phux config init [OPTIONS]

Options:
      --force
          Overwrite an existing config file instead of refusing

      --distro <NAME_OR_PATH>
          Starter distribution to extend: a bundled name (resolved under `$PHUX_DISTROS_DIR`, the XDG data dir, or the repo checkout) or a path to a distro layer `.toml` / directory

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux config path`

```text
Print the resolved config path. Pure path math — prints the path whether or not the file exists

Usage: phux config path [OPTIONS]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux config plugins`

```text
List plugin manifests declared by `[[plugins]]`

Usage: phux config plugins [OPTIONS]

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux config reload`

```text
Re-read the layered config and apply it to running clients in place.

Validates the config locally first (a broken file fails here, with the parse error, and nothing is signalled), then rings the `phux.config.reload/v1` doorbell on the server so every attached client re-reads its own config file and rebuilds keybindings, theme, and status bar without restarting. Clients whose re-read fails keep their previous config. Deliberately explicit — the config file is never watched.

Usage: phux config reload [OPTIONS]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux config run`

```text
Execute one action declared by a configured plugin manifest

Usage: phux config run [OPTIONS] <PLUGIN> <ACTION>

Arguments:
  <PLUGIN>
          Configured plugin id

  <ACTION>
          Plugin-local action id

Options:
      --timeout <SECS>
          Give up after this many seconds. Omit to wait indefinitely

      --cwd <PATH>
          Override the action cwd. Relative paths resolve under plugin root

      --json
          Emit the structured action result as JSON

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux config show`

```text
Print the effective config (shipped defaults + your overrides) as TOML. With `--default`, print the shipped defaults verbatim instead, ignoring any user config. With `--layers`, print which layer of the `extends` stack set each effective key instead of the values

Usage: phux config show [OPTIONS]

Options:
      --default
          Show the shipped defaults verbatim, not the merged result

      --layers
          Attribute each effective key to the layer that set it (embedded defaults / `extends` layers / your config file)

      --json
          With --layers: emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux detach`

```text
Detach clients from a session, from outside the attach UI.

The CLI counterpart to the `C-a d` keybinding. With `SESSION`, detaches every client attached to that session; with no argument, detaches every attached client on the server. Each target client's TUI exits cleanly. Useful for scripting or reclaiming a session that's attached (or wedged) elsewhere.

Usage: phux detach [OPTIONS] [SESSION]

Arguments:
  [SESSION]
          Session to detach clients from. Omit to detach every attached client on the server

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux doctor`

```text
Diagnose a phux install: config, socket, server, plugins.

Composes the checks that already exist as separate verbs and reports one verdict, because knowing which four commands to run and how to read each one is exactly what someone debugging phux does not have.

Read-only. Exits 1 if any check failed; warnings alone exit 0, since a stopped server is a normal state and not a broken install.

Usage: phux doctor [OPTIONS]

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux give`

```text
Give back the input wheel of a pane.

Releases the input lease taken with `phux take`, returning the pane to open input. A no-op if you do not hold the lease. TARGET is a selector.

Usage: phux give [OPTIONS] <TARGET>

Arguments:
  <TARGET>
          Target selector (resolves to one pane)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux host`

```text
Register the machines phux talks to: remotes and satellites.

One namespace over both machine registries. `--role remote` (the default) manages the servers `phux attach <name>` dials; `--role satellite` manages the peers a federation hub dials for its users. The two registries stay separate in config (`[[remote]]` vs `[[satellites]]`) because they encode opposite trust directions; this verb absorbs the split into a flag.

Usage: phux host [OPTIONS] <COMMAND>

Commands:
  add     Register a machine, or replace an entry with the same name
  enroll  Set up a machine over ssh, end to end, and register it
  ls      List registered machines from both registries [aliases: list]
  rm      Remove a registered machine. Its token file is left in place [aliases: remove]
  help    Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux host add`

```text
Register a machine, or replace an entry with the same name.

`--role remote` (the default) registers a server `phux attach <name>` can dial; `--role satellite` registers a peer this hub dials for its users. Updating replaces the whole entry, so repeat `--token-file` / `--cert-fingerprint` when re-adding a name or the auth material is cleared.

Usage: phux host add [OPTIONS] <NAME> <ENDPOINT>

Arguments:
  <NAME>
          Local label for the machine

  <ENDPOINT>
          Endpoint URI: `quic://HOST:PORT`, `wss://HOST:PORT`, or `ssh://HOST`. `ssh://` rides your existing ssh trust and needs no pairing; the other two need a token and a certificate pin

Options:
      --role <ROLE>
          Which registry the entry lands in

          Possible values:
          - remote:    A server this machine attaches to — a `[[remote]]` entry
          - satellite: A peer this hub dials for its users — a `[[satellites]]` entry

          [default: remote]

      --token-file <PATH>
          Absolute path to a file holding the pairing token minted by `phux pair` on the other machine

      --cert-fingerprint <FP>
          The other machine's TLS certificate SHA-256 fingerprint, as printed by `phux pair`. Required for `quic://` and `wss://`

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --session <NAME>
          Session to attach on arrival (`--role remote` only). Omitted: the remote server's own last-attach memory decides

      --disabled
          Register the entry but leave it disabled (`--role satellite` only)

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux host enroll`

```text
Set up a machine over ssh, end to end, and register it.

Confirms phux is installed on HOST, installs its service unit so the server survives reboot, mints a pairing token there, and registers the result in the role-correct registry — `--role remote` (the default) yields an entry `phux attach <name>` dials with no flags and no hex strings typed by hand; `--role satellite` a peer this hub dials for its users. Uses the ssh trust you already have; it grants nothing ssh did not already grant.

A host with no reachable listener falls back to an ssh:// entry, which still gives you sessions that outlive the connection.

Usage: phux host enroll [OPTIONS] <HOST>

Arguments:
  <HOST>
          ssh destination, exactly as you would type it after `ssh` (`mini`, `me@mini`, or a `~/.ssh/config` alias)

Options:
      --role <ROLE>
          Which registry the enrolled machine lands in

          Possible values:
          - remote:    A server this machine attaches to — a `[[remote]]` entry
          - satellite: A peer this hub dials for its users — a `[[satellites]]` entry

          [default: remote]

      --name <NAME>
          Local label to register. Defaults to HOST without any `user@`

      --endpoint <HOST:PORT>
          Address to register instead of the remote's detected overlay address. Accepts `HOST:PORT` (dialed over QUIC) or a full `quic://`/`wss://` URI

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --quic-port <PORT>
          QUIC port to configure on the remote and register

          [default: 8788]

      --no-service
          Skip installing the remote's service unit. The server will not come back on its own after a reboot

      --ssh-only
          Register an ssh:// entry without contacting the host at all

      --session <NAME>
          Session to attach on arrival (`--role remote` only)

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux host ls`

```text
List registered machines from both registries.

With no `--role`, remotes and satellites are merged into one table with a ROLE column; `--role` filters to one registry.

Usage: phux host ls [OPTIONS]

Options:
      --role <ROLE>
          Show only this registry

          Possible values:
          - remote:    A server this machine attaches to — a `[[remote]]` entry
          - satellite: A peer this hub dials for its users — a `[[satellites]]` entry

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux host rm`

```text
Remove a registered machine. Its token file is left in place.

With no `--role`, the name is resolved across both registries; a name registered in both is refused until `--role` disambiguates.

Usage: phux host rm [OPTIONS] <NAME>

Arguments:
  <NAME>
          Registered name

Options:
      --role <ROLE>
          Which registry to remove from. Omitted: both are searched

          Possible values:
          - remote:    A server this machine attaches to — a `[[remote]]` entry
          - satellite: A peer this hub dials for its users — a `[[satellites]]` entry

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux insert-pane`

```text
Insert an already-created pane into a session layout.

Both selectors must each resolve to exactly one local pane in the same session. This command does not spawn: create `NEW_PANE` first with `phux spawn`, then insert it. Omitted direction defaults horizontal.

Usage: phux insert-pane [OPTIONS] <TARGET> <NEW_PANE>

Arguments:
  <TARGET>
          Existing layout leaf beside which `NEW_PANE` is inserted

  <NEW_PANE>
          Already-created pane to insert; no implicit spawn occurs

Options:
      --split <SPLIT>
          Split axis: `horizontal` stacks the panes, `vertical` places them side-by-side

          [default: horizontal]
          [possible values: horizontal, vertical]

      --ratio <RATIO>
          Fraction assigned to TARGET; must be strictly between 0 and 1

          [default: 0.5]

      --json
          Emit a schema-versioned JSON result or error

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux kill`

```text
Kill a session, window, pane, or the server itself.

`TARGET` uses the selector grammar (see the top-level help): `name`, `name:N`, `name:N.M`, `name:tag`, `@N`, `.`. The selector is resolved client-side against a server-state snapshot to a set of Terminals; the server is then asked to kill each.

`--server` stops the server process instead, ending every session on it. Local socket only.

Usage: phux kill [OPTIONS] <TARGET|--server>

Arguments:
  [TARGET]
          What to kill (selector)

Options:
      --server
          Stop the running server, ending every session it holds.

          The server exits cleanly, so a supervised one stays stopped rather than being restarted. Note that the next `phux attach`/`new` will auto-spawn a fresh server: this stops the current one, it does not disable phux.

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux launch`

```text
Launch an agent integration in a new pane.

Resolves INTEGRATION (a `phux launch --list` id) to its `[launch]` command from an enabled plugin's integration template, then spawns a pane running it. The template routes the agent through its identity wrapper, so the pane self-declares its `phux.agent/v1` identity with no alias or per-shell config: the server injects `PHUX_TERMINAL_ID`, the wrapper targets its own pane with it, and writes name + kind at launch.

`--print` resolves and prints the argv without spawning (a server-free dry run). Extra agent arguments follow `--`: `phux launch codex -- --model o3`.

Usage: phux launch [OPTIONS] [INTEGRATION] [-- <EXTRA>...]

Arguments:
  [INTEGRATION]
          Integration id to launch (from `phux launch --list`)

  [EXTRA]...
          Extra arguments appended to the agent command, after `--`

Options:
      --list
          List launchable integrations from enabled plugins and exit

      --print
          Resolve and print the launch argv (and cwd) without spawning a pane — a server-free dry run

          [aliases: --dry-run]

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --target <TARGET>
          Existing local pane beside which to place the launched pane

      --split <SPLIT>
          Split axis for explicit placement (requires `--target`)

          [default: horizontal]
          [possible values: horizontal, vertical]

      --ratio <RATIO>
          Fraction of the split retained by TARGET (requires `--target`)

          [default: 0.5]

  -c, --cwd <DIR>
          Working directory for a `working_directory = "workspace"` template. Defaults to the current directory

  -h, --help
          Print help (see a summary with '-h')
```

## `phux logs`

```text
Show where phux's logs live, or tail one of them.

Bare `phux logs` prints the inventory: the canonical server log (every spawn path writes it), the per-pid client logs, and the state dir that holds them — with existence, size, and age, so a fresh machine reads "not created yet" instead of an error. `--server` tails the server log and `--client` the newest client log (`--pid` picks a specific one); `-f` follows and `-n` sets the tail length. `--json` emits the inventory as a stable document.

Usage: phux logs [OPTIONS]

Options:
      --server
          Tail the canonical server log

      --client
          Tail the newest per-pid client log (or the one `--pid` names)

      --pid <PID>
          With --client: the client pid whose log to tail, instead of the newest

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -f, --follow
          Follow the tailed log as it grows (needs --server or --client)

  -n, --lines <LINES>
          How many trailing lines to show (needs --server or --client)

          [default: 200]

      --json
          Emit the path inventory as a stable JSON document instead of human text. Inventory only — it cannot combine with a tail

  -h, --help
          Print help (see a summary with '-h')
```

## `phux ls`

```text
List sessions on the running server.

Queries the running server and prints one line per session. Does not start a server: with no server running it reports as much and exits non-zero (like `tmux ls`). Pass `--json` for the stable, versioned machine shape instead of the human text.

Usage: phux ls [OPTIONS]

Options:
      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux move-pane`

```text
Move one existing pane beside another, even across sessions.

SOURCE is collapsed out of its current tree position and inserted beside TARGET. Both selectors must resolve to exactly one local pane. When TARGET lives in a different session the pane is re-parented on the server first — its process, scrollback, and id survive the move.

Usage: phux move-pane [OPTIONS] <SOURCE> <TARGET>

Arguments:
  <SOURCE>
          Pane to relocate

  <TARGET>
          Existing destination pane

Options:
      --split <SPLIT>
          Destination split axis: `horizontal` stacks the panes, `vertical` places them side-by-side

          [default: horizontal]
          [possible values: horizontal, vertical]

      --ratio <RATIO>
          Fraction assigned to TARGET; must be strictly between 0 and 1

          [default: 0.5]

      --json
          Emit a schema-versioned JSON result or error

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux new`

```text
Create a new session and attach to it.

Creates the named session if it does not already exist, then attaches. Auto-starts a server if none is running. A name already in use is an error; omit the name to take the configured `session-name-template`, disambiguated with a numeric suffix.

With `--json`, creates the session *without* attaching and prints the seed pane's id as JSON instead. This neither attaches nor resizes, and the create is atomic server-side (no attach race). `--json` requires an explicit `-s NAME`, and a name already in use is an error (create-only, never create-or-attach).

Usage: phux new [OPTIONS] [NAME] [-- <COMMAND>...]

Arguments:
  [NAME]
          Session name. `phux new work` creates a session named "work". Omitted ⇒ the `session-name-template` (e.g. "default"), disambiguated with a numeric suffix if that name is taken

  [COMMAND]...
          Command (and arguments) to run in the seed pane instead of the default shell. Must follow `--`: `phux new work -- htop`

Options:
  -s, --session <SESSION>
          Session name in flag form — equivalent to the positional NAME, and the form required by `--json`. An error if it conflicts with NAME

  -c, --cwd <CWD>
          Working directory for the seed pane

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -e, --env <KEY=VALUE>
          Environment assignment for the seed process. Repeat for multiple variables. Headless `--json` mode only

  -h, --help
          Print help (see a summary with '-h')
```

## `phux pair`

```text
Mint a pairing token for a remote consumer.

Remote consumers (e.g. the native mobile app) attach over `wss://` without an SSH tunnel: TLS encrypts the link and an opaque bearer token authenticates the device. This mints one token into the store the server reads (`PHUX_WS_TOKENS`) and prints it once alongside the server certificate's SHA-256 fingerprint. Pair both into the device: the token is the credential, and verifying the fingerprint on first connect defeats a man-in-the-middle. Revoke a device by deleting its line from the token file. When an overlay network address (Tailscale/WireGuard) is detected, it is printed alongside the credentials.

This never contacts a running server — it only writes the token file.

Usage: phux pair [OPTIONS]

Options:
      --tokens <PATH>
          Token store to append to. Defaults to `PHUX_WS_TOKENS`

      --cert <PATH>
          Server certificate PEM, used to print the pairing fingerprint. Defaults to `PHUX_WS_TLS_CERT`

      --qr
          Also render the pairing payload as a scannable QR code. The QR encodes the same `phux://connect` one-tap link printed as text, so a phone can pair by scanning instead of typing. Needs a server address: pass `--host`, or let it fall back to a detected overlay address plus the `PHUX_WS_ADDR` port

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --host <HOST:PORT>
          Server address (`host:port`, or a full `ws://`/`wss://` URL) to embed in the connect link so it is fully self-contained. Omitted: derived from the detected overlay address and the `PHUX_WS_ADDR` port when possible; otherwise no link is printed (the device enters the address itself)

      --name <NAME>
          Human-readable server name to embed in the connect link, shown by the device in its server list. Omitted: the device picks a default

      --json
          Emit the pairing material as JSON on stdout instead of the human-readable report. This is what `phux host enroll` reads over ssh

  -h, --help
          Print help (see a summary with '-h')
```

## `phux paste`

```text
Paste text into a pane.

Delivers the payload as ONE paste event to the resolved pane (`ROUTE_INPUT`), so the live pane is neither attached nor resized. When the pane's program has bracketed paste (DEC mode 2004) switched on, the server wraps the payload in paste markers and the program receives it as a single block — auto-indent stays off and multiline text arrives intact. Without the mode, the raw bytes are delivered as if typed.

A paste INSERTS; it does not SUBMIT. Paste-aware shells and REPLs buffer the block until a real Enter — follow with `phux send-keys TARGET Enter` to run what you pasted.

TEXT is the payload; omit it to read the payload from stdin. Payloads are trusted by default (you vouch for content you composed); `--untrusted` opts into the server's safety gate.

Examples:
  phux paste demo 'SELECT count(*) FROM users;'
  git diff | phux paste review

Usage: phux paste [OPTIONS] <TARGET> [TEXT]

Arguments:
  <TARGET>
          Target selector: session, session:window, session:window.pane, @id, or `.` (focused). `=` is unsupported by headless commands

  [TEXT]
          Text to paste. Omit to read the payload from stdin

Options:
      --untrusted
          Mark the payload untrusted: the server classifies it and the pane's untrusted-paste policy (reject by default) may silently drop an unsafe payload — e.g. anything multiline. Without this flag the paste is trusted and forwarded verbatim

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux play`

```text
Play a recording back as a live pane.

Creates a new Terminal whose PTY is fed from FILE, then prints its id. The result is an ordinary pane: attach it, `phux snapshot` it, `phux resize` it, watch it from an agent, or `phux kill` it. It is not a viewer for your own shell — for that, `asciinema play FILE` is the right tool and needs no server.

TARGET says WHERE the pane goes: the playback pane is created beside it, splitting its window. TARGET is never written to, and no flag makes playback take over a pane that already has a shell in it. The default is `.`, the focused pane.

The pane is resized to the recording's own grid first, and to each resize the recording contains, so lines wrap where they wrapped when it was captured; --no-fit leaves the grid alone. When the recording ends the pane holds its final frame until you kill it, so nothing races the last byte; --close ends the pane instead.

Examples:
  phux play demo.cast
  phux play demo.cast work:1.0 --speed 2
  phux play demo.cast --loop --idle-limit 0.5 --json

Usage: phux play [OPTIONS] <FILE> [TARGET]

Arguments:
  <FILE>
          The .cast file to play

  [TARGET]
          Selector for the pane the playback pane is created beside. Defaults to `.` (the focused pane). Never written to

Options:
      --speed <N>
          Playback rate. 1 is real time, 2 is twice as fast, 0.5 half speed. Between 0.01 and 100; no events are ever dropped

          [default: 1]

      --idle-limit <SECS>
          Collapse any pause longer than SECS down to SECS. Defaults to the idle limit the recording itself declares; 0 plays the raw timeline

      --loop [<N>]
          Repeat the recording. Bare `--loop` repeats until the pane is killed; `--loop N` plays it N times

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --split <SPLIT>
          Split axis for the new pane

          [default: horizontal]
          [possible values: horizontal, vertical]

      --ratio <RATIO>
          Fraction of the split retained by TARGET

          [default: 0.5]

      --no-fit
          Leave the pane's grid alone instead of fitting it to the recording's. Output wider than the pane will wrap

      --close
          Close the pane when playback ends, instead of holding the final frame until it is killed

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux plugin`

```text
Manage local plugin manifests in the phux config registry.

This is a client-local config operation: it validates `phux-plugin.toml` manifests and edits `[[plugins]]` entries in the user's config without contacting a running server.

Usage: phux plugin [OPTIONS] <COMMAND>

Commands:
  list      List configured plugin manifests [aliases: ls]
  link      Add or update a manifest entry in `config.toml`
  install   Fetch, build, validate, and link a plugin package
  update    Re-fetch, rebuild, and revalidate installed plugins
  unlink    Remove a configured plugin by id [aliases: rm, remove]
  enable    Enable a configured plugin by id
  disable   Disable a configured plugin by id
  validate  Validate one manifest, or every configured manifest when omitted
  help      Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux plugin disable`

```text
Disable a configured plugin by id

Usage: phux plugin disable [OPTIONS] <ID>

Arguments:
  <ID>
          Plugin id from its manifest

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux plugin enable`

```text
Enable a configured plugin by id

Usage: phux plugin enable [OPTIONS] <ID>

Arguments:
  <ID>
          Plugin id from its manifest

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux plugin install`

```text
Fetch, build, validate, and link a plugin package.

REF is a git URL (`https://…`, `git@…`, `file://…` — cloned with the system `git`), a local plugin directory (copied), or a local tarball (`.tar`, `.tar.gz`, `.tgz` — extracted with the system `tar`). The package lands under the managed plugins directory (`$XDG_DATA_HOME/phux/plugins`, else `~/.local/share/phux/plugins`), its manifest `[[build]]` steps for this platform run with a bounded timeout and captured output, the manifest is validated (including the `min_phux_version` gate), and the result is linked into `config.toml` like `phux plugin link`. Provenance (ref, branch, resolved commit) is recorded in the managed directory's `plugins.lock` so `phux plugin update` can re-fetch it later.

Usage: phux plugin install [OPTIONS] <REF>

Arguments:
  <REF>
          Git URL, local plugin directory, or local tarball path

Options:
      --rev <REV>
          Branch or tag to clone (git sources only)

      --disabled
          Install and link the plugin but leave it disabled

      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux plugin link`

```text
Add or update a manifest entry in `config.toml`

Usage: phux plugin link [OPTIONS] <MANIFEST>

Arguments:
  <MANIFEST>
          Path to a `phux-plugin.toml` file, or a directory containing one

Options:
      --disabled
          Register the plugin but leave it disabled

      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux plugin list`

```text
List configured plugin manifests

Usage: phux plugin list [OPTIONS]

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux plugin unlink`

```text
Remove a configured plugin by id

Usage: phux plugin unlink [OPTIONS] <ID>

Arguments:
  <ID>
          Plugin id from its manifest

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux plugin update`

```text
Re-fetch, rebuild, and revalidate installed plugins.

Reads the managed directory's `plugins.lock`, re-fetches each recorded source (all of them, or just NAME), reruns its `[[build]]` steps, revalidates the manifest, swaps the managed copy, and records the new resolved commit. `config.toml` is untouched — the linked manifest path does not move.

Usage: phux plugin update [OPTIONS] [NAME]

Arguments:
  [NAME]
          Plugin id to update. Omit to update every installed plugin

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux plugin validate`

```text
Validate one manifest, or every configured manifest when omitted

Usage: phux plugin validate [OPTIONS] [MANIFEST]

Arguments:
  [MANIFEST]
          Optional path to a `phux-plugin.toml` file or plugin directory

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux rec`

```text
Record a pane and export it as an asciinema cast, an animated GIF, or an APNG.

TARGET is a selector (default: the focused pane). Recording is a pure observer: it does not attach the session and never resizes the pane, so it is safe to run against a live session someone is using.

The format follows the output extension (.cast, .gif, .png, .apng); pass --format to override. Use --from to re-render an existing recording without capturing anything.

Examples:
  phux rec -o demo.gif
  phux rec work:1.0 -o demo.cast --duration 30
  phux rec --from demo.cast -o demo.gif --fps 20

Usage: phux rec [OPTIONS] --out <PATH> [TARGET]

Arguments:
  [TARGET]
          Pane selector. Defaults to the focused pane

Options:
  -o, --out <PATH>
          Output path. The extension picks the format unless --format is given; a path with no extension gets `.gif`

      --format <FMT>
          Output format, overriding the extension

          Possible values:
          - cast: asciinema cast — the archival, re-renderable artifact
          - gif:  Animated GIF — shareable and embeddable anywhere
          - apng: Animated PNG — truecolor, no quantization, larger files

      --from <FILE>
          Re-render an existing .cast instead of capturing a live pane

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --duration <SECS>
          Stop after SECS of recording (default: until Ctrl-C or the pane exits)

      --fps <FPS>
          Animation sample rate for GIF/APNG output

          [default: 10]

      --idle-limit <SECS>
          Collapse any pause longer than SECS down to SECS. 0 disables

          [default: 2]

      --max-bytes <BYTES>
          Stop encoding and warn once the output reaches BYTES

          [default: 8388608]

      --cast-version <N>
          asciicast format version to write (2 is the interoperable default)

          [default: 2]

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux relay`

```text
Run a standalone relay, or enroll a route with it.

The relay is a separate rendezvous process for reaching a phux server that cannot accept inbound connections: the server dials OUT to the relay and registers a tunnel for a named route, remote consumers dial IN naming that route, and the relay splices the two as opaque bytes — it never reads what crosses. `run` serves in the foreground; `pair` enrolls a route name and mints the token the server's tunnel authenticates with. Relay state (the route-token store and a self-signed certificate) lives at fixed paths under the phux state directory.

Usage: phux relay [OPTIONS] <COMMAND>

Commands:
  run   Run the relay in the foreground
  pair  Enroll a route and mint (or rotate) its tunnel token
  help  Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux relay pair`

```text
Enroll a route and mint (or rotate) its tunnel token.

Writes one entry binding a fresh secret token to NAME in the relay's route-token store and prints the token once, alongside the relay certificate's SHA-256 fingerprint. Give both to the phux server that will dial out to this relay: the token authenticates its tunnel, and the fingerprint pins the relay's certificate. Pairing a route that is already enrolled REPLACES its token (rotation) — exactly one token per route. Revoke a route by deleting its line from the store. This never contacts a running relay — it only writes the token file, and a running relay picks the change up at the next tunnel handshake.

Usage: phux relay pair [OPTIONS] --route <NAME>

Options:
      --route <NAME>
          Route name the token is bound to. Consumers select the route via the TLS server name, so it must be a lowercase DNS label: `[a-z0-9-]`, at most 63 characters, no leading or trailing hyphen. Anything else is rejected, never normalized

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux relay run`

```text
Run the relay in the foreground.

Binds one QUIC endpoint on LISTEN and serves both relay legs on it: phux servers dial out from behind NAT and register a tunnel for their enrolled route, and remote consumers dial in naming a route, each spliced onto that route's live tunnel as opaque bytes. Enroll routes with `phux relay pair`; the token store is re-read per connection attempt, so pairing a new route (or revoking one by deleting its line) needs no restart. Serves until Ctrl-C.

Usage: phux relay run [OPTIONS] --listen <HOST:PORT>

Options:
      --listen <HOST:PORT>
          Address the relay's QUIC endpoint binds (e.g. `0.0.0.0:4433`). Always explicit — there is no default listen address, so exposing the relay requires typing where

      --max-conns <N>
          Maximum concurrent connections, tunnels and consumers combined. An over-cap connection is refused after its handshake completes; existing connections are unaffected

          [default: 64]

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux rename`

```text
Rename a session.

Reassigns `SESSION`'s human-readable name to `NEW_NAME` in one round-trip. The server is authoritative; attached clients pick up the new name on their next snapshot. An unknown `SESSION` or a `NEW_NAME` already in use is an error.

Usage: phux rename [OPTIONS] <SESSION> <NEW_NAME>

Arguments:
  <SESSION>
          Current session name

  <NEW_NAME>
          New session name

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux resize`

```text
Set a pane's grid size, with no TTY.

The headless counterpart to resizing your terminal window: names one pane and gives it an exact cell geometry. Nothing attaches and nothing subscribes, so the pane is never dragged toward the 80x24 size a program with no terminal would otherwise report.

The new size takes effect immediately, even with someone attached. It is not permanent against an attached view: under the default `window-size = "smallest"` policy the next attach, detach, or window resize recomputes the pane's geometry from the attached views and overrides it. Set `window-size = "manual"` when an explicit size must hold. Either way this verb reads the server's real size back before exiting, and exits nonzero if it is not the one you asked for, so a script can never mistake a delivered request for an applied one.

Examples:
  phux resize demo 120x40
  phux resize @7 200x50 --json

Usage: phux resize [OPTIONS] <TARGET> <COLSxROWS>

Arguments:
  <TARGET>
          Target selector: session, session:window, session:window.pane, @id, or `.` (focused). `=` is unsupported by headless commands

  <COLSxROWS>
          New grid size, e.g. 120x40. Both axes are whole numbers of cells and at least 1

Options:
      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux run`

```text
Run a command in a pane and capture its exit code.

Reports the command's exit code, output, and duration. Brackets the command with sentinels to capture `$?`, so it assumes a POSIX shell (sh/bash/zsh). The process exit code mirrors the command's — and is 125 when `phux` gives up on `--timeout` — so `phux run … && next` composes like a shell. TARGET is a selector (see the top-level help), resolved client-side to one pane; the command routes to it by id (no attach, no resize).

Flags (`--timeout`, `--json`, `--socket`) MUST precede TARGET, or they are swallowed into the trailing command.

Examples:
  phux run build "cargo test"
  phux run --timeout 30 work:1.0 "cargo test"

Usage: phux run [OPTIONS] <TARGET> <COMMAND>...

Arguments:
  <TARGET>
          Target selector: session, session:window, session:window.pane, @id, or `.` (focused). `=` is unsupported by headless commands

  <COMMAND>...
          The command line: all trailing args, joined with spaces

Options:
      --timeout <SECS>
          Give up after this many seconds (exit 125). Default: 600s. Pass 0 to wait indefinitely

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux send-keys`

```text
Send keys to a pane.

tmux-shaped: each KEY is a named key (`Enter`, `Tab`, `Escape`, `Up`, `C-c`, `M-x`, …) or a literal string. Literals normally type character by character; a literal run immediately before `Enter` is delivered as a submission-safe paste followed by the real key, honoring the pane's live bracketed-paste mode. TARGET is resolved client-side to one pane, so the live pane is neither attached nor resized.

Flags (`--socket`) MUST precede TARGET: KEYS is a trailing var-arg, so anything after TARGET is taken as a key to send.

Examples:
  phux send-keys demo "echo hi" Enter
  phux send-keys work:1.0 C-c

Usage: phux send-keys [OPTIONS] <TARGET> <KEYS>...

Arguments:
  <TARGET>
          Target selector: session, session:window, session:window.pane, @id, or `.` (focused). `=` is unsupported by headless commands

  <KEYS>...
          Keys to send: named keys and/or literal strings, in order

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux server`

```text
Run a phux server in the foreground.

Binds a Unix domain socket, pre-seeds a session whose initial pane spawns the user's `$SHELL` inside a real PTY, and serves `ATTACH` requests until Ctrl-C.

Usage: phux server [OPTIONS]

Options:
      --session <SESSION>
          Name of the pre-seeded session. Matches what `phux attach <name>` will request

          [default: default]

      --listen <HOST:PORT>
          Also accept WebSocket clients on this `HOST:PORT` (the UDS stays on). Loopback (e.g. `127.0.0.1:8787`) is plaintext for local browser dev; any routable address (e.g. `0.0.0.0:8787`) auto-provisions TLS and requires a `phux pair` token. Overrides `$PHUX_WS_ADDR`

      --quic <HOST:PORT>
          Also accept QUIC clients on this `HOST:PORT` (the UDS stays on). QUIC is always TLS 1.3-encrypted; a loopback address skips token auth (local dev), while any routable address requires a `phux pair` token sent as the stream's opening preamble. Overrides `$PHUX_QUIC_ADDR`

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --webtransport <HOST:PORT>
          Also accept WebTransport (HTTP/3 over QUIC) clients on this `HOST:PORT` (the UDS stays on) — the browser's door to QUIC-class transport; the browser client dials it, falling back to WebSocket. Always TLS 1.3-encrypted; a loopback address skips token auth (local dev), while any routable address requires a `phux pair` token carried in the CONNECT request (`Authorization: Bearer` from native consumers, `?token=<hex>` on the session URL from browsers). Overrides `$PHUX_WT_ADDR`

      --connect <HOST:PORT>
          Dial one relay outbound on `HOST:PORT`. If a matching `[[connector]]` entry exists, its token file and certificate pin are used; otherwise only a loopback endpoint is accepted for unauthenticated development. Without this flag, every configured connector is supervised independently

      --hub
          Run as a federation hub: consume the `[[satellites]]` registry from `config.toml` at startup, validating every enabled entry's endpoint (`quic://`, `ws://`, `wss://`, or `ssh://`) into the runtime satellite table, then dial and maintain one outbound link per satellite (QUIC and WebSocket links authenticate with a bearer token; `ssh://` bridges over `ssh HOST phux stdio-bridge`), relaying satellite-tagged frames over the links. A malformed enabled endpoint or a duplicate satellite name fails startup. Without this flag the registry is ignored

      --exit-after-idle <SECS>
          Exit once no client has been connected for SECS, even if panes are still running. For ephemeral servers: a test harness or CI job that bootstraps a private server per run and cannot guarantee its own cleanup step will execute. The clock starts at startup, so a server nobody ever connects to also exits.

          Without this flag the server keeps the multiplexer contract and lives until its last pane is gone.

  -h, --help
          Print help (see a summary with '-h')
```

## `phux service`

```text
Keep a server running across logout and reboot.

Generates this host's native per-user service unit — a `launchd` `LaunchAgent` on macOS, a systemd user unit on Linux — with the server's environment baked in, so a rebooted host comes back with a server instead of waiting for someone to log in and start one. A restarted server has no terminals: every pane's process died with the host. `install --restore` brings back session names, layout, and cwd, not running processes.

Usage: phux service [OPTIONS] <COMMAND>

Commands:
  install     Write the unit and hand it to the init system
  uninstall   Unload the unit and remove what `install` wrote
  status      Report whether a unit is installed and running
  logs        Show the supervised server's log
  prune-logs  Delete the accumulated per-pid `client-*.log` files
  help        Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux service install`

```text
Write the unit and hand it to the init system.

Idempotent: rerunning reconciles an existing unit, so changing a listener address is `install` again with the new flag.

Usage: phux service install [OPTIONS]

Options:
      --quic <HOST:PORT>
          Accept QUIC clients on this `HOST:PORT`. A routable address (e.g. `0.0.0.0:8788`) engages TLS and requires a `phux pair` token. Prefer this over `--listen` where UDP is open

      --listen <HOST:PORT>
          Accept WebSocket clients on this `HOST:PORT`. The fallback for networks that block UDP

      --restore
          Save the workspace on stop and restore it on start. Off by default: a session list repopulated with fresh shells is a surprise unless asked for. Restores names, layout, and cwd — never running processes

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --hub
          Run the supervised server as a federation hub. The service loads enabled `[[satellites]]` entries and keeps their links connected across login, logout, and reboot

      --print
          Print the unit (and the restore wrapper) to stdout without writing or loading anything

  -h, --help
          Print help (see a summary with '-h')
```

## `phux service logs`

```text
Show the supervised server's log

Usage: phux service logs [OPTIONS]

Options:
  -f, --follow
          Follow the log as it grows

  -n, --lines <LINES>
          How many trailing lines to show

          [default: 200]

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux service prune-logs`

```text
Delete the accumulated per-pid `client-*.log` files

Usage: phux service prune-logs [OPTIONS]

Options:
      --dry-run
          Report how many would be removed, and remove nothing

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux service status`

```text
Report whether a unit is installed and running

Usage: phux service status [OPTIONS]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux service uninstall`

```text
Unload the unit and remove what `install` wrote

Usage: phux service uninstall [OPTIONS]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux signal`

```text
Signal a pane's process group.

Delivers a POSIX signal to the program running in the resolved pane and every subprocess it spawned — distinct from `phux kill`, which destroys the pane. `freeze` (SIGSTOP) pauses the process mid-step; `resume` (SIGCONT) lets it run again — the reversible brake for an agent about to do something rash. TARGET is a selector.

Examples:
  phux signal build freeze
  phux signal . kill

Usage: phux signal [OPTIONS] <TARGET> <SIGNAL>

Arguments:
  <TARGET>
          Target selector (resolves to one pane)

  <SIGNAL>
          Which signal to deliver

          Possible values:
          - interrupt: SIGINT — the Ctrl-C equivalent
          - freeze:    SIGSTOP — pause the process group (reversible via `resume`)
          - resume:    SIGCONT — resume a frozen process group
          - terminate: SIGTERM — request graceful termination
          - kill:      SIGKILL — force termination

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux skill`

```text
Print the agent skill this binary ships with, on stdout.

The text is compiled into the executable, so it describes the verbs and flags THIS build actually has — it cannot drift from the binary the way a copied file can. It contacts no server and reads no config.

Give it to any agent that needs to drive phux: it teaches the read-act-wait loop, the selector grammar, the difference between a level read and an observed transition, the exit codes, and the safety rules for driving a terminal a human may also be using.

Examples:
  phux skill
  phux skill > ~/.claude/skills/phux/SKILL.md
  phux skill | pbcopy

Usage: phux skill [OPTIONS]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux snapshot`

```text
Capture a pane's screen as JSON or a boxed text view.

The agent "floor": read what's on screen as JSON (`--json`) or a boxed text view, without a TTY or tmux. The read is side-effect-free — the server walks its own grid, so this neither attaches nor resizes the pane, and is safe to poll against a pane another client is using.

TARGET is a selector (see the top-level help); omit it for the most-recently-focused session.

Usage: phux snapshot [OPTIONS] [TARGET]

Arguments:
  [TARGET]
          Target selector. Omit for the most-recently-focused session

Options:
      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --scrollback [<N>]
          Include scrollback history above the viewport. Bare `--scrollback` requests all retained history; `--scrollback N` requests the most-recent N rows. History appears in the JSON `scrollback` field; the boxed view shows it above the viewport

      --cells
          Include per-cell OSC-133 semantic marks + styles. Populates the JSON `cells` array (sparse: only cells with a non-default style or a semantic mark). No effect on the boxed view, which is plain text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --tail [<N>]
          Return the last N rendered rows (history above the viewport, then the viewport). Bare `--tail` returns 80; `--tail 0` returns all, capped at 10000. The viewport is a floor — a grid is never returned in part — and `truncated` reports any dropped rows

      --unwrap
          Join soft-wrapped rows into logical lines (rows as written, not as painted). Cannot be combined with `--cells`: cell coordinates are grid coordinates and do not survive the join

      --rendered
          Emit the CLIENT's composited multi-pane view — the assembled frame (layout tiling + dividers + status bar) as the human's glass shows it — as dense structured cells. Unlike the default side-effect-free read this ATTACHES (drives the headless client render path). Mutually exclusive with `--cells` / `--scrollback` / `--tail` / `--unwrap`; sizes the composite via `--cols` / `--rows`

      --cols <COLS>
          Composited viewport width for `--rendered` (no TTY to measure)

          [default: 80]

      --rows <ROWS>
          Composited viewport height for `--rendered`

          [default: 24]

  -h, --help
          Print help (see a summary with '-h')
```

## `phux spawn`

```text
Spawn a Terminal without attaching (`SPAWN_TERMINAL`).

With `--target`, the pane is inserted beside an exact local owner; otherwise it joins the server's most recently active session. The new Terminal's id prints to stdout. With `--satellite NAME` on a federation hub (`phux server --hub`), the spawn is routed over the hub's link to that satellite and the returned id is satellite-tagged — addressable through the hub by every satellite-capable verb. Does not auto-start a server.

Usage: phux spawn [OPTIONS] [-- <COMMAND>...]

Arguments:
  [COMMAND]...
          Command (and arguments) to run instead of the default shell. Must follow `--`: `phux spawn -- htop`

Options:
      --satellite <NAME>
          Route the spawn to a configured federation satellite (a name from `phux host ls --role satellite`, on a server running `--hub`)

      --target <TARGET>
          Existing local pane beside which to place the new pane

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --split <SPLIT>
          Split axis for explicit placement (requires `--target`)

          [default: horizontal]
          [possible values: horizontal, vertical]

      --ratio <RATIO>
          Fraction of the split retained by TARGET (requires `--target`)

          [default: 0.5]

  -c, --cwd <CWD>
          Working directory for the new pane

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux status`

```text
Report the running server: pid, up since, protocol, clients, logs.

One glance at the server behind the socket: whether it is running and as which pid, since when, the protocol version it speaks, how many clients are attached, the sessions it holds, and where its logs live. Does not start a server: with no server running it reports as much and exits non-zero. Pass `--json` for the stable, versioned machine shape instead of the human text; with no server that shape is `{"running": false, ...}` on stdout, still exiting non-zero.

Usage: phux status [OPTIONS]

Options:
      --json
          Emit stable, versioned JSON on stdout instead of the human view. Exception to the shared failure contract: with no server running, stdout carries the `{"running": false, ...}` document (still exiting non-zero); any other failure leaves stdout empty and puts one JSON error object on stderr

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux swap-pane`

```text
Swap two existing pane leaves in the same session layout.

Both selectors must each resolve to exactly one local pane. Split geometry is preserved and attached clients retain their local focus.

Usage: phux swap-pane [OPTIONS] <FIRST> <SECOND>

Arguments:
  <FIRST>
          First pane selector

  <SECOND>
          Second pane selector

Options:
      --json
          Emit a schema-versioned JSON result or error

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux tag`

```text
Read and write a Terminal's L3 tags.

Tags are freeform strings stored as L3 metadata (`phux.tags/v1`), the server stores them opaquely. Once a Terminal is tagged, the `#tag` selector addresses every Terminal carrying that tag — e.g. `phux kill #build`, `phux snapshot #web`.

Usage: phux tag [OPTIONS] <COMMAND>

Commands:
  ls    List the tags on each Terminal a selector resolves to [aliases: list]
  add   Add one or more tags to each Terminal a selector resolves to
  rm    Remove one or more tags from each Terminal a selector resolves to [aliases: remove]
  help  Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux tag add`

```text
Add one or more tags to each Terminal a selector resolves to

Usage: phux tag add [OPTIONS] <TARGET> <TAGS>...

Arguments:
  <TARGET>
          Target selector

  <TAGS>...
          Tags to add (the leading `#` is optional)

Options:
      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux tag ls`

```text
List the tags on each Terminal a selector resolves to

Usage: phux tag ls [OPTIONS] <TARGET>

Arguments:
  <TARGET>
          Target selector (session, `session:window`, `@id`, `.`, `#tag`)

Options:
      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux tag rm`

```text
Remove one or more tags from each Terminal a selector resolves to

Usage: phux tag rm [OPTIONS] <TARGET> <TAGS>...

Arguments:
  <TARGET>
          Target selector

  <TAGS>...
          Tags to remove (the leading `#` is optional)

Options:
      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux take`

```text
Take the input wheel of a pane.

Seizes exclusive input authority over the resolved pane: while held, only this connection's input reaches the PTY — every other client's keystrokes (and any agent's `send-keys`) are locked out. Use it to grab control of a pane an agent is driving. Release with `phux give`. TARGET is a selector (see the top-level help).

Usage: phux take [OPTIONS] <TARGET>

Arguments:
  <TARGET>
          Target selector (resolves to one pane)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux update`

```text
Update phux to the latest release, keeping sessions alive.

Checks the published release, downloads the archive for this platform, verifies it against the checksum published beside it, replaces the binaries atomically, and asks a running server to re-exec so live panes survive. A server, its local clients, its satellites, and its relays must all run the same release, so this is the command that moves a whole deployment in one step.

phux updates only installs it maintains: a release archive unpacked into $PHUX_INSTALL_DIR, ~/.local/bin, ~/bin, /usr/local/bin, or /opt/phux/bin. A Homebrew, Cargo, or Nix install is never modified — the exact native command is printed instead — and an unrecognized location is refused rather than overwritten.

The previous binaries are kept beside the new ones; `--rollback` puts them back.

Examples:
  phux update --check
  phux update --check --json
  phux update
  phux update --dry-run --version v1.2.3
  phux update --rollback

Usage: phux update [OPTIONS]

Options:
      --check
          Report the current and latest release and the install source, then stop. Changes nothing and never downloads an archive

      --dry-run
          Do everything except the replacement: resolve, download, and verify the checksum, then report what would have been installed

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --version <TAG>
          Install this release tag instead of the latest one. Accepts any tag from the releases page, including an older one (a downgrade)

      --rollback
          Restore the binaries saved by the previous `phux update`

      --no-restart
          Replace the binaries but do not ask a running server to re-exec. Live panes keep the old image until the server is upgraded or restarted

      --json
          Emit the stable, versioned JSON document on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux upgrade`

```text
Graceful-upgrade the running server in place.

Asks the server to snapshot every pane, re-exec the on-disk binary, and re-adopt the live PTYs, so the shells / editors / agents in every session survive a binary update (e.g. after `cargo install` / `brew upgrade`). Clients briefly disconnect and reconnect. This is the low-level primitive: it re-execs whatever is already on disk and downloads nothing. `phux update` is the command that puts a new binary there first.

Usage: phux upgrade [OPTIONS]

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux wait`

```text
Block until a pane meets a condition.

Polls the side-effect-free screen read — the poll floor of the event surface: always works, no shell integration. Exits 0 when the condition is met, and 124 when `--timeout` expires first. TARGET is a selector (see the top-level help); omit it for the most-recently-focused session.

Matching is against the lines as WRITTEN: rows the terminal soft-wrapped at its right edge are joined first, so text that straddles a wrap is found rather than silently never matching.

Flags (`--until`, `--regex`, `--idle`, `--tail`, `--output-only`, `--timeout`, `--json`, `--socket`) MUST precede TARGET if you give one.

Examples:
  phux wait --until "BUILD SUCCESSFUL" build
  phux wait --regex "test result: (ok|FAILED)" --output-only build
  phux wait --idle 750 repl

Usage: phux wait [OPTIONS] [TARGET]

Arguments:
  [TARGET]
          Target selector. Omit for the most-recently-focused session

Options:
      --until <TEXT>
          Succeed once any line contains this substring. NOTE: this matches ANY line, including the shell's echo of a command you just typed — match on text that appears only in OUTPUT, or pass `--output-only`

      --regex <PATTERN>
          Succeed once any line matches this Rust regular expression. One line at a time, so `^` and `$` anchor to a line you can see. An invalid pattern is a usage error (exit 2) reported before the wait starts, never a wait that quietly never matches

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --tail [<N>]
          Match only within the last N lines, and read that much history to do it. Bare `--tail` uses 80; `--tail 0` uses all retained history, capped at 10000. Without it, only the viewport is read. N counts logical lines AFTER wrapped rows are joined and ignores the blank rows under the cursor, and unlike `snapshot --tail` the viewport is not a floor: `--tail 3` really does mean only the last three lines with content count — including the prompt block already back on screen, so leave room for it. A bare `--tail` reads the next word as N, so spell N out when you also pass TARGET (`--tail 80 build`, not `--tail build`)

      --output-only
          Ignore lines the shell marked as your own typed input, so a wait cannot be satisfied by the echo of the command that started the work. Needs a shell with OSC-133 integration; with none, nothing is filtered and phux says so on stderr rather than pretending

      --idle <MS>
          Succeed once the matched lines hold still for this many milliseconds (the pane has settled). Default when neither `--until` nor `--regex` is given. With `--tail N`, only those lines have to hold still — a spinner further up does not count

      --timeout <SECS>
          Give up after this many seconds (exit 124). Default: wait forever

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

  -h, --help
          Print help (see a summary with '-h')
```

## `phux watch`

```text
Stream a pane's live events (the push half of the agent surface).

Subscribes to the server's event stream and prints one event per line. The subscription neither attaches nor resizes the pane — safe to watch a pane a human or another agent is actively using. TARGET is a selector (see the top-level help); omit it for the most-recently-focused session.

With no bounds the stream runs until EOF or Ctrl-C. `--until EVENT` makes it a gate: the first matching event is printed and `watch` exits 0. `--timeout SECS` gives up and exits 124, the same code `phux wait` uses. If the server closes the stream before an `--until` event arrives, that is exit 1 — the event did not happen and can no longer happen.

With `--json` each line is one JSON object and nothing else is written to stdout: no per-line schema_version, and no summary line on timeout.

Examples:
  phux watch build
  phux watch --json work:1.0
  phux watch --until asked --timeout 120 reviewer

Usage: phux watch [OPTIONS] [TARGET]

Arguments:
  [TARGET]
          Target selector. Omit for the most-recently-focused session

Options:
      --until <EVENT>
          Exit 0 as soon as an event with this name arrives. Repeatable; any one of them satisfies the watch. The vocabulary is the one this stream prints: `agent_state`, `asked`, `bell`, `command_finished`, `command_started`, `dirty`, `idle`, `pane_closed`, `pane_spawned`, `title_changed`, `unknown`. An unrecognized name is a usage error (exit 2) reported before the watch starts, never a watch that quietly never matches

      --timeout <SECS>
          Give up after this many seconds (exit 124). Applies with or without `--until`. Default: stream until EOF or Ctrl-C

      --json
          Emit stable, versioned JSON on stdout instead of the human view. On failure, stdout stays empty and stderr carries one JSON error object

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux workspace`

```text
Inspect a git workspace and its worktrees for agent orchestration.

This is a local repo operation: it never contacts a running phux server and never creates or deletes worktrees. Agents use it to map code checkouts to phux sessions/panes before spawning or attaching work.

Usage: phux workspace [OPTIONS] <COMMAND>

Commands:
  inspect  Inspect the git repository and its checked-out worktrees
  save     Save the running phux workspace as a JSON archive
  restore  Restore missing sessions from a workspace archive
  help     Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux workspace inspect`

```text
Inspect the git repository and its checked-out worktrees

Usage: phux workspace inspect [OPTIONS] [PATH]

Arguments:
  [PATH]
          Path inside the repository or worktree to inspect

          [default: .]

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux workspace restore`

```text
Restore missing sessions from a workspace archive

Usage: phux workspace restore [OPTIONS] <ARCHIVE>

Arguments:
  <ARCHIVE>
          JSON archive path, or '-' to read from stdin

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux workspace save`

```text
Save the running phux workspace as a JSON archive

Usage: phux workspace save [OPTIONS]

Options:
  -o, --output <PATH>
          Write the archive to a path instead of stdout

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help
```

## `phux worktree`

```text
Manage git worktrees and the sessions bound to them.

Each worktree binds to one session whose name is derived from the worktree's directory basename. The derivation is a pure function of the path, so the binding is computed on demand and can never go stale — phux stores no worktree state and the server knows no git.

Usage: phux worktree [OPTIONS] <COMMAND>

Commands:
  list    List the repository's worktrees and their bound sessions [aliases: ls]
  new     Create a worktree and a session rooted in it
  open    Open the session bound to an existing worktree, creating it if absent
  remove  Remove a worktree, killing the session bound to it first [aliases: rm]
  help    Print this message or the help of the given subcommand(s)

Options:
      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux worktree list`

```text
List the repository's worktrees and their bound sessions.

The `bound` column reads `live` when a session by the derived name exists, `-` when it does not, and `?` when no server is running — "no server" and "no session" are different facts.

Usage: phux worktree list [OPTIONS] [PATH]

Arguments:
  [PATH]
          Path inside the repository or worktree to list from

          [default: .]

Options:
      --json
          Emit a stable JSON document instead of human text

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux worktree new`

```text
Create a worktree and a session rooted in it.

An existing local branch is checked out; a missing one is created, from `--from` when given and from the current HEAD otherwise. The worktree lands beside the repository as `<repo>-<branch>` unless `--path` says otherwise.

Usage: phux worktree new [OPTIONS] <BRANCH> [-- <COMMAND>...]

Arguments:
  <BRANCH>
          Branch to check out, or to create when it does not exist

  [COMMAND]...
          Command to run in the new session instead of the default shell

Options:
      --path <PATH>
          Where to put the worktree. Defaults to a sibling of the repo

      --from <REF>
          Start point for a newly created branch (default: current HEAD)

  -s, --session <NAME>
          Session name, overriding the name derived from the path

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

      --repo <PATH>
          Path inside the repository the worktree belongs to

          [default: .]

      --attach
          Attach to the new session instead of creating it headlessly

      --json
          Emit a stable JSON document — branch, path, session, and the seed pane's `terminal_id` — instead of human text. This is the first call in a fan-out script, and the id it returns is the pane the caller then sends its first prompt to. Cannot combine with `--attach`: an attached session owns stdout

  -h, --help
          Print help (see a summary with '-h')
```

## `phux worktree open`

```text
Open the session bound to an existing worktree, creating it if absent.

Idempotent: an already-live session is reported and left alone, so scripts and keybindings can call this without checking first.

Usage: phux worktree open [OPTIONS] <TARGET>

Arguments:
  <TARGET>
          Worktree path, branch, or derived session name

Options:
      --repo <PATH>
          Path inside the repository the worktree belongs to

          [default: .]

      --attach
          Attach to the session instead of only reporting its name

      --json
          Emit the same document `worktree new --json` emits, whether the session was created now or was already live — so a script that re-enters a fleet gets the seed pane without special-casing

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```

## `phux worktree remove`

```text
Remove a worktree, killing the session bound to it first.

The session is killed before git runs, because git refuses to remove a worktree whose files are held open and a shell sitting in that directory holds it open. Refuses the worktree you are standing in.

Usage: phux worktree remove [OPTIONS] <TARGET>

Arguments:
  <TARGET>
          Worktree path, branch, or derived session name

Options:
      --force
          Pass --force to git, removing a worktree with local changes

      --repo <PATH>
          Path inside the repository the worktree belongs to

          [default: .]

      --json
          Emit a stable JSON document instead of human text. A fan-out teardown script has the same parsing problem creation does

      --socket <PATH>
          Override the UDS path of the server to dial. Defaults to `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set)

  -h, --help
          Print help (see a summary with '-h')
```
