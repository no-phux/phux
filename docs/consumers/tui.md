---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-08-02
---

# The phux reference TUI

**TL;DR.** The reference TUI's consumer-facing product surface:
subcommands, keybinds, status bar, layout, hooks, recording. The TUI is
the wedge — the daily-driver adoption surface — and its differentiator is
the wire: attach/detach, remoting, and a human and their agents sharing
the same live terminals. It is held a pure consumer with no protocol
privilege by [ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md).
What's normative lives in [`../spec/`](../spec/); this file is the
human-facing reference for the tmux-shaped consumer that ships in tree.

---

## 0. What this is, what this isn't

This document is the **reference TUI consumer's product surface**: the
things a tmux-shaped phux user sees and configures — how a user invokes
the TUI, configures it, binds keys, reads its status output, and extends
it. Where this document conflicts with the normative wire spec under
[`../spec/`](../spec/), the spec wins; file an issue.

### 0.1 The TUI is the wedge, not a second local multiplexer

The reference TUI is worth heavy product investment because it is the
adoption surface that bootstraps a population of terminals-on-the-wire
([ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)
§6). What distinguishes it from a local multiplexer is not local splits —
those are table stakes — but the wire underneath: a phux session lives on
the server, so a client can **attach and detach** without killing it,
**remote** over a transport, and let a **human and their agents share the
same live terminals** ([`agents.md`](./agents.md) drives those terminals
side-effect-free while a human watches). The local-tiling features in this
doc are the familiar shape that gets a tmux user in the door; the wire is
why they stay.

Investing in the TUI as a product and holding it as a pure consumer are
not in tension. The constraint that keeps the wedge from corrupting the
platform is [ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md): the
TUI gets no protocol-level standing, and its needs land as L3 conventions
and client logic, never as new wire surface. Other consumers — the
[agent CLI](./agents.md), the [MCP adapter](./mcp.md), the
[browser client](./web.md), a future native GUI — are peers, each its own
file under [`docs/consumers/`](./).

For the long arc, read [`../vision.md`](../vision.md). For the wire
protocol, see [`../spec/`](../spec/). For internal structure, see
[`../architecture/`](../architecture/). This document is everything
between.

### 0.2 TUI vocabulary maps to the substrate

The user-facing vocabulary is tmux's. Under the hood, each TUI concept
maps to substrate concepts. Following
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md),
there is **no L2 collection tier**: a session is L3 grouping metadata plus
client logic, not a wire-level lifecycle entity.

| TUI vocabulary | Substrate mapping |
|---|---|
| Session | L3 metadata grouping a set of `TerminalId`s under a well-known key plus client logic; named via the `phux.session.name/v1` key. Not an L2 tier. Atomic teardown rides the single `KILL_TERMINALS` L1 op. |
| Window | TUI convention. An entry in a layout-tree blob stored in L3 metadata, keyed by `phux.tui.layout/v1` for the session's terminals. |
| Pane | L1 Terminal (`TerminalId`) referenced from a leaf of the TUI's layout tree. |
| Layout (split tree) | TUI convention. The shape stored in the L3 metadata blob above. ADR-0012's "binary split, not n-ary" still governs *this tree*; it is not a wire concept. |
| Active pane / window focus | TUI convention. Per-client, persisted in TUI metadata if the client wants it to come back on reattach. |
| Status bar / hooks / keybindings | TUI-local. Not on the wire. |
| Mouse routing (click-to-focus, drag-to-resize) | TUI-local. The wire carries `INPUT_MOUSE`; what to do with it is the TUI's call. |

A consumer that doesn't want this vocabulary doesn't have to learn it;
the substrate doesn't carry it. `GroupId` survives only as a
documented opaque grouping key, not a lifecycle tier — its full removal is
tracked by bead phux-0bmc.

---

## 1. CLI surface

phux is a single binary with subcommands. The naked invocation —
`phux` — is the common case: attach to the user's server, lazily
spawning it if it isn't running. With no arguments it auto-spawns a server
if the socket is missing, then attaches via `AttachTarget::Last` with a
fallback to `AttachTarget::ByName("default")` when the server has no
prior-attach memory. Auto-spawn (the client forks itself as `phux server`
if the socket is missing, polls 25 ms / 2 s) covers both the naked and the
explicit-attach paths.

### 1.1 The shipped verbs

These are the main interactive and control entrypoints. `phux --help` is the
complete generated inventory, including supervision, upgrade, tags, pairing,
agents, and workspace commands:

```
phux                          # attach to default session, autostart server
phux attach [SESSION]         # attach explicitly; session optional (alias: a)
phux attach --quic HOST:PORT [--cert-fingerprint FP] [--token HEX]
                              # attach to a remote server over QUIC (TLS 1.3).
                              # loopback trusts the dev cert; routable hosts
                              # require --cert-fingerprint (from `phux pair`)
phux attach --ws ws://127.0.0.1:8787
                              # attach over the WebSocket/TCP fallback locally
phux attach --ws wss://HOST:PORT --cert-fingerprint FP --token HEX
                              # attach over TLS WebSocket when UDP/QUIC is blocked
phux server [--session N] [--listen HOST:PORT] [--quic HOST:PORT]
            [--connect HOST:PORT] [--hub] [--exit-after-idle SECS]
                              # run server in foreground
                              # --listen also accepts WebSocket clients (= PHUX_WS_ADDR)
                              # --quic also accepts QUIC clients (= PHUX_QUIC_ADDR)
                              # --connect selects one [[connector]] relay;
                              # without it every configured relay is supervised
                              # --hub validates [[satellites]] into the runtime
                              # satellite table at startup, dials each enabled
                              # satellite (quic/wss per ADR-0038; ssh:// over
                              # `ssh HOST phux stdio-bridge`), and relays
                              # satellite-tagged frames over the links (§4.2)
                              # --exit-after-idle bounds an EPHEMERAL server:
                              # it exits once no client has been connected for
                              # SECS, live panes and all. Off by default —
                              # without it the server lives until its last
                              # pane is gone (ADR-0063)
phux new [-s NAME] [-c CWD] [--] [COMMAND...]
                              # create a session
phux spawn [--satellite NAME | --target TARGET [--split DIR] [--ratio R]] [-c CWD] [--json] [--] [COMMAND...]
                              # explicit placement is local-only; absent target
                              # preserves legacy unplaced behavior
phux launch INTEGRATION [--print] [--target TARGET [--split DIR] [--ratio R]] [-c CWD] [--] [ARGS...]
                              # spawn a pane running an agent integration's
                              # [launch] command (ADR-0042); resolves the named
                              # template from an enabled plugin and routes the
                              # agent through its identity wrapper, so the pane
                              # self-declares its phux.agent/v1 identity with no
                              # alias. --list enumerates; --print is a
                              # server-free dry run of the resolved argv
phux ls                       # list sessions (alias: list)
phux kill TARGET              # kill session/window/pane by selector
phux insert-pane TARGET NEW    # insert an already-created pane (no spawn)
phux move-pane SOURCE TARGET   # relocate a pane beside another
phux swap-pane FIRST SECOND    # exchange two pane leaves
phux rename SESSION NEW-NAME  # rename a session
phux resize TARGET COLSxROWS  # set a pane's grid with no TTY (§4.2
                              # `window-size` for what happens when a
                              # client is attached)
phux snapshot [TARGET]        # dump pane grid (for piping/scripting)
phux snapshot --rendered      # dump the client's composited multi-pane view
phux send-keys TARGET KEYS... # send keys to a pane (scripting)
phux paste TARGET [TEXT]      # paste text into a pane (TEXT or stdin)
phux run TARGET CMD...        # run a command in a pane, capture $?
phux wait [TARGET]            # poll a pane until a condition holds
phux watch [TARGET]           # stream a pane's live events
phux rec [TARGET] -o PATH     # record a pane to a cast, GIF, or APNG (§10);
                              # a pure observer — never attaches or resizes
phux --rec PATH               # on `phux` / `phux attach` only: tee the
                              # attached session's composited output to PATH
phux play FILE.cast [TARGET]  # create a pane whose PTY is fed from a
                              # recording (§10). TARGET says WHERE the new
                              # pane goes; it is never written to
phux ask TARGET QUESTION      # report an agent ask event for a pane
phux agent install-claude     # make plain interactive `claude` enter phux
phux agent uninstall-claude   # remove its shim, hooks, and shell activation
phux config <init|path|show>  # scaffold + inspect config
phux config check [PATH] [--json]
                              # report every unknown key / wrong value with
                              # its full dotted path and originating layer
phux config plugins [--json]  # compatibility alias: inspect plugin manifests
phux config agents [--json]   # inspect configured plugin agent states
phux config run PLUGIN ACTION # execute a configured plugin action
phux plugin <COMMAND>         # install/update/link/list/toggle/unlink/validate plugins
phux satellite <COMMAND>      # enroll/add/list/remove federation satellites
phux stdio-bridge             # splice stdin/stdout to the local server socket
                              # (the remote end of the SSH-stdio transport)
phux worktree list [--json]   # worktrees + their bound session and liveness
phux worktree new BRANCH [--path P] [--from REF] [-s NAME] [--attach] [-- CMD...]
                              # git worktree add, then create the bound session
phux worktree open TARGET [--attach]
                              # ensure the bound session exists (idempotent)
phux worktree remove TARGET [--force]
                              # kill the bound session, then git worktree remove
phux doctor [--json]          # diagnose the install: config, socket path,
                              # server reachability, plugin manifests
phux completion SHELL         # print a shell completion script on stdout
                              # (bash, elvish, fish, powershell, zsh);
                              # generated from this binary's own parser, so it
                              # never advertises a verb the build lacks
phux enroll HOST [--name N] [--endpoint HOST:PORT] [--quic-port P]
                 [--no-service] [--ssh-only] [--session N]
                              # set up a remote server over ssh end to end
                              # (ADR-0055): confirm phux is installed there,
                              # install its service unit, mint a pairing
                              # token, and register the result locally, so
                              # `phux attach HOST` needs no flags afterwards.
                              # Falls back to an ssh:// entry when the host
                              # has nothing dialable
phux remote <add|list|remove> # the registry `phux attach NAME` resolves
phux service <install|uninstall|status|logs|prune-logs>
                              # per-user service unit (launchd LaunchAgent on
                              # macOS, systemd user unit on Linux) that keeps
                              # a server running across logout and reboot.
                              # `install --hub` persists federation hub mode;
                              # `install --restore` adds workspace save/restore
phux --version                # print version
phux help [COMMAND]
```

The agent-facing verbs — `new`, placed `launch`/`spawn`, `ls`, `snapshot`,
`send-keys`, `paste`, `run`, `wait`, `watch`, `ask`, `resize`, and the spatial verbs above — have
their JSON
contracts and exit-code semantics documented in [`agents.md`](./agents.md);
this file does not restate them.

### 1.2 new / kill / rename ride the wire mechanism; UX is unchanged

`new`, `kill`, and `rename` no longer ride dedicated session/collection
L1 verbs. Per
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)
they decompose onto the substrate, with no change to what the user types:

- **`new`** is `SPAWN_TERMINAL` plus an L3 metadata write
  (`phux.session.create/v1`, read back via `phux.session.created/v1`).
- **`rename`** is an L3 metadata SET on `phux.session.name/v1`.
- **`kill`** of a whole group is the atomic `KILL_TERMINALS { ids }` L1
  op (tag `0x09`), applied all-or-nothing under the server's single lock
  so no observer sees a partial teardown.

The command words, flags, and output are exactly as before; only the
wire path beneath them changed.

### 1.3 Headless spatial edits operate on existing panes

`insert-pane`, `move-pane`, and `swap-pane` edit persisted L3 layout envelopes;
they do not attach and do not change another client's local focus. Every
positional selector must resolve to exactly one local pane. Satellite topology
edits are rejected. `insert-pane` and `swap-pane` require one session;
`move-pane` may cross sessions, re-parenting the live Terminal on L1 before
updating the source and destination envelopes.

`insert-pane TARGET NEW_PANE [--horizontal|--vertical] [--ratio R]` is named
for what it honestly does: `NEW_PANE` must already exist (for example from
`phux spawn`) and must not already be in the layout. It does **not** implicitly
spawn. The omitted direction is horizontal (a horizontal divider, so panes are
stacked); `--vertical` means a vertical divider and side-by-side panes. `R`
defaults to `0.5`; ratios must be finite and strictly between zero and one.
`move-pane SOURCE TARGET` accepts the same user-facing direction and ratio
flags. A cross-session move preserves the Terminal's process, PTY, scrollback,
metadata, and id. `swap-pane FIRST SECOND` preserves
the existing split geometry. All three accept `--json` and `--socket`.

Detach (`C-a d`) remains an interactive TUI-only action because it acts on the
calling client's attachment.

### 1.3.1 `phux doctor` composes the checks that already exist

Every check `doctor` runs already existed as its own verb: `config check`,
`plugin validate`, a socket-length guard buried in the spawn path, a
`GET_STATE` probe inside `ls`. Knowing to run all four, in the right order,
and how to read each one is precisely the knowledge someone debugging phux
does not have.

```console
$ phux doctor
ok   config       ~/.config/phux/config.toml is valid
ok   socket-path  /run/user/1000/phux/phux.sock
warn server       no server at /run/user/1000/phux/phux.sock
                  -> start one with `phux` (auto-spawns) or `phux server`
ok   plugins      2 manifest(s) valid

no failures, 1 warning(s)
```

Three states, not two. A check that **could not run** reports `warn`, never
`ok` — a stopped server is a normal state, and rendering it green would be a
lie while rendering it red would train people to ignore red lines. Only
`FAIL` means verified-broken, and only `FAIL` sets the exit code to 1, so
`phux doctor` can gate a setup script without failing on a machine where
phux simply is not running yet.

Every non-passing check carries a next step. A diagnosis that names a
problem without naming an action is half a diagnosis.

`doctor` is strictly read-only. A diagnostic that repairs things is one
nobody can trust to describe the system.

The socket-path check earns its place: an over-long path fails as a connect
that times out with no explanation, and nobody guesses `sockaddr_un` on
their own. A socket file with nothing behind it is reported as a failure
rather than a missing server, because every CLI verb will refuse until it is
cleared.

### 1.4 Worktrees bind to sessions by derived name

`phux worktree` composes `git worktree` with `new` / `ls` / `kill`
(ADR-0054). The server learns nothing about git and stores no worktree
state; the binding between a checkout and a session is a **pure function of
the worktree path**. The directory basename is sanitized — anything outside
`[A-Za-z0-9._-]` collapses to `-`, runs of `-` collapse to one, and
selector sigils (`@`, `#`, `=`, `.`) are trimmed from the edges — so
`~/src/phux-feat-auth` binds to the session `phux-feat-auth`. Because the
name is derived and never stored, it cannot go stale when git deletes a
worktree or an operator moves the directory.

```sh
phux worktree new feat/auth        # git worktree add + create the session
phux worktree list                 # paths, branches, derived names, liveness
phux worktree open feat/auth       # idempotent: create-if-absent, else report
phux worktree remove feat/auth     # kill the session, then remove the worktree
```

`new` and `open` are **headless by default** and print the session name;
pass `--attach` for the interactive behavior. `new` puts the worktree beside
the repository as `<repo>-<branch>` unless `--path` says otherwise, checks
out an existing branch or creates a missing one (from `--from`, else the
current HEAD), and refuses when the derived name collides with another
worktree's — pass `-s NAME` to disambiguate.

`remove` checks cleanliness before it kills anything, so a refusal has no
side effects, then kills the bound session and **waits for it to leave the
snapshot** before handing over to git. That ordering is not cosmetic: git
refuses to remove a worktree whose files are held open, and a shell sitting
in that directory holds it open. It refuses the worktree you are standing in.

The `bound` column distinguishes three states, not two: `live` (a session by
that name exists), `-` (it does not), and `?` (no server is running, which is
a different fact from "no session").

A session created by hand in a worktree under some other name is not
recognized as bound — `list` shows the worktree as unbound. Closing that gap
needs pane cwd in the session snapshot, which is a wire change ADR-0054
deliberately does not make.

<!-- impl-status: spec-only; probe: Command::Windows,Command::Panes,ConfigAction::Edit -->
> **Status (design intent, not shipped):** `windows`, `panes`, and
> `messages` are listed in earlier drafts as future read verbs; none
> ships today. `config` ships `init` / `path` / `show` / `reload` (§4.3);
> `config edit` is design intent.

**The target convention.** The verbs that address an existing pane —
`kill`, `snapshot`, `send-keys`, `paste`, `run`, `wait`, `watch`, `ask`, and the
spatial verbs — take selectors as
**positional** `TARGET` (omitted on `snapshot`/`wait` to mean the
focused session, or on `watch` for server-wide events). `attach` likewise takes
its `[SESSION]` name
positionally. `new` is the exception: because its trailing `[COMMAND...]`
is a positional var-arg, the *new* session's name is the `-s`/`--session`
flag instead, keeping the command words unambiguous. So: positional target
to act on something that exists; `-s` to name something you are creating.

**Flags before the target.** `send-keys`, `run`, `wait`, and `ask` take a
trailing var-arg (the keys / command / nothing), so every flag —
`--json`, `--timeout`, `--until`, `--idle`, `--socket` — MUST precede the
positional `TARGET`; anything after it is swallowed into the trailing
words. Each command's `--help` calls this out.

**Output hygiene (for scripts and agents).** One-shot verbs print no
banner and keep stdout clean. With `--json`, stdout carries ONLY the JSON
document; diagnostics go to stderr with a nonzero exit, never interleaved
into the JSON. The agent-relevant JSON surfaces are `new`, `launch`, `spawn`,
`ls`, `snapshot`, `run`, `wait`, JSONL `watch`, `ask`, `agent`, the three
spatial verbs, `tag`, `config show/plugins/agents/run`, `plugin`, `workspace`,
and `satellite`. Their
per-verb JSON shapes and the stable exit-code semantics are owned by
[`agents.md`](./agents.md) §3–§4 — this file does not restate them.

---

## 2. The user model

Three nouns. Same as tmux. Don't reinvent vocabulary that users already
know.

- **Session** — top-level container. Named. Persists across client
  disconnects. Lives until explicitly killed or until the server exits.
- **Window** — tab within a session. Numbered from 0 within its session;
  optionally named.
- **Pane** — leaf in a window's layout. One PTY, one terminal grid, one
  shell or command.

A **client** is an attached frontend (TUI or GUI). Clients are
transient; they are not part of the session model. The protocol exposes
`ClientId` only for the duration of a connection.

---

## 3. Selectors

A selector identifies a session, window, or pane. Selectors appear in
CLI arguments, keybinding actions, and hook arguments.

| Selector              | Meaning                                          |
|-----------------------|--------------------------------------------------|
| `.`                   | current — the client's focused pane/window/session |
| `name`                | session by name                                  |
| `name:N`              | session `name`, window index `N`                 |
| `name:N.M`            | session `name`, window `N`, pane index `M`       |
| `name:tag`            | session `name`, window whose name is `tag`       |
| `@N`                  | opaque ID (pane/window/session) — stable for the |
|                       | server's lifetime                                |
| `=`                   | attached TUI only: previous pane (`C-a =`)       |
| `#tag`                | every Terminal carrying L3 tag `tag`             |

The `#tag` form (ADR-0027) resolves to the **set** of Terminals tagged
`tag`, exactly as a session name resolves to many panes. Tags are L3
metadata (`phux.tags/v1`), read and written with `phux tag`:

```text
phux tag add work:1.0 build ci    # tag a pane
phux tag ls .                      # list the focused pane's tags
phux kill #build                   # kill every Terminal tagged 'build'
phux tag rm @7 ci                  # untag
```

Headless CLI and MCP calls have no attached client's focus history, so an
explicit `=` target is rejected with an unsupported-selector error rather than
silently aliasing `.`. In the attached TUI, `C-a =` dispatches `last-pane`
against a one-entry, process-local MRU; repeating it toggles between two panes,
including panes in different windows. The MRU is neither persisted nor sent on
the wire, matching ADR-0019's client-local focus rule and accepted ADR-0049.
Shared topology writers never acquire focus authority.

All headless commands otherwise share one grammar. `kill`, `snapshot`, `wait`,
`watch`, `send-keys`, `paste`, `run`, `ask`, launch/spawn placement, and the three
spatial verbs accept the same `TARGET` (phux-n95) and resolve it client-side
against a `GET_STATE` snapshot (ADR-0021) — the server never parses a
selector. A selector that names several panes (a whole session or window)
resolves to a single **selected pane**: the focused pane if it is among
the matches, else the first in snapshot order. So `phux send-keys work …`
targets the pane you are looking at in session `work`, while
`phux send-keys work:1.0 …` targets exactly window 1, pane 0. `send-keys`
and `run` route input to that resolved pane by id — no attach, no resize
(phux-3j3). Omit the target on `snapshot`/`wait` to default to the
focused session.

The CLI infers what kind of selector is expected from the command. When
ambiguity matters, prefer the most specific form. Example:

```sh
phux kill work:edit.2         # second pane in window "edit" of session "work"
phux send-keys @42 "ls" Enter # send to the local pane with stable id 42
phux snapshot devbox/@7       # read satellite pane 7 through the hub
phux run work:1.0 "cargo test"# run in window 1, pane 0 of session "work"
phux kill .                   # kill the focused session
# `phux kill =` errors: headless clients have no focus MRU
```

---

## 4. Configuration

### 4.0 Philosophy and the `phux config` commands

phux is **config-driven**, in the Ghostty mold
([ADR-0023](../../ADR/0023-config-ux-philosophy.md)): one TOML file is the
whole source of truth, and phux never writes settings back from running
state. There is no `set-option` verb. The defaults you don't override
ship *inside the binary* as an embedded, annotated `default.toml`; your
`config.toml` is a sparse overlay merged on top of it leaf-by-leaf. A key
you omit keeps tracking the binary's default, so a phux upgrade that
improves a default reaches you automatically — your file is overrides,
not a frozen snapshot.

A missing config file is not an error; phux runs on the embedded defaults
alone. To get a documented starting point and to inspect what's active:

```
phux config path            # print the resolved config path (no I/O)
phux config init            # scaffold a commented starter config there;
                            #   refuses to overwrite (use --force)
phux config init --distro herdr
                            # same scaffold plus one active extends line
                            #   layering a starter distribution (bundled
                            #   name or path); see docs/CONFIG.md
phux config show            # print the effective config (defaults + your
                            #   overrides) as canonical TOML
phux config show --default  # print the shipped defaults verbatim,
                            #   comments and all — the annotated source
phux config show --layers   # provenance: which layer of the extends
                            #   stack (ADR-0039) set each effective key;
                            #   arrays list each element's contributor.
                            #   --json for the stable document
                            #   (schema_version 1)
phux config plugins --json  # print configured plugin manifests as JSON
phux config agents --json   # print configured plugin agent states as JSON
phux config check           # every unknown key and wrong value, each
                            #   with its full dotted path and the layer
                            #   file that introduced it. --json for the
                            #   stable document (schema_version 1)
phux config reload          # validate, then apply the config to running
                            #   clients in place (see 4.3)
phux plugin list --json     # inspect the plugin registry
phux plugin validate        # validate every configured plugin manifest
```

`phux config init` writes the shipped defaults *with every line commented
out*: the file documents every option next to its real default value, yet
imposes no overrides until you uncomment a line. That is what keeps the
binary's defaults authoritative — uncommenting is the only way the file
changes behavior. The `--distro` flavor adds exactly one live statement —
an `extends` line layering a curated starter distribution (ADR-0039)
between the defaults and your file; the distro layer is referenced, never
copied, so its updates keep reaching you. Distribution mechanics and the
bundled `herdr` starter are documented in
[docs/CONFIG.md](../CONFIG.md#starter-distributions-config-init---distro). `config show` renders the merged TOML *table*, so it
answers "what is my effective config" rather than reproducing your file's
comments or key order; `cat` the file for the latter.

For testing config changes inside a checkout without touching your real
`~/.config/phux`, `just scaffold-config` drops a starter into a
worktree-local `./.phux-xdg` (gitignored); point `XDG_CONFIG_HOME` at it
to exercise the result.

### 4.0.1 First-run onboarding hint

On attach, when **nothing exists at the resolved config path** (the path
`phux config path` prints), the TUI shows a small dismissible overlay
pointing at `phux config init` and the `C-a ?` help binding — the two
affordances that answer "now what?" on a fresh install. The exact rules:

* **Decided once per `phux attach` invocation**, by a single existence
  check at attach time. Switching sessions inside the same invocation
  does not re-show it; creating a config mid-attach does not retract an
  already-shown hint (the next attach simply won't show one).
* **Any key dismisses it** for the rest of that invocation. The
  keystroke is consumed by the overlay (like every modal), so the hint
  costs exactly one keystroke.
* **It never appears when anything exists at the config path** — a
  config file (even one that fails to parse) or a stray directory.
  Presence, not validity, is the test: an unparsable config means you
  have already found the config system, and `phux config init` refuses
  to overwrite, so the hint's advice would be wrong there. When the
  check itself is undetermined (e.g. a permission error on the config
  directory), the hint stays suppressed.
* **Nothing is persisted.** There is no "seen" flag or state file: while
  no config exists, every attach shows the hint once; running
  `phux config init` (or writing any config file) silences it
  permanently.

The hint hardcodes `C-a ?` deliberately — it only ever shows when no
config file exists, which is exactly when the embedded defaults (prefix
`C-a`, `?` = `show-help`, section 5.3) are guaranteed to be active.

### 4.1 File location

Config is read from `$XDG_CONFIG_HOME/phux/config.toml` (or
`~/.config/phux/config.toml`). Set `XDG_CONFIG_HOME` to isolate configuration
for a test or alternate environment; there is no global config-path flag.

Runtime and persistent state are split. The Unix socket lives in the
runtime dir (where it's expected to disappear on reboot); persistent
state lives in the state dir.

```
$XDG_RUNTIME_DIR/phux/phux.sock     # SOCK_STREAM, parent dir mode 0o700
                                    #   (fallback: /tmp/phux-$UID/phux.sock)

$XDG_STATE_HOME/phux/
├── server.log                      # canonical server log (both spawn paths)
├── client-<pid>.log                # default interactive-client log
├── remote-cert.pem                 # auto-provisioned remote certificate
├── remote-key.pem                  # owner-only private key
└── remote-tokens                   # owner-only pairing tokens
```

These files are real today. A server PID file, rotated server-log directory,
and per-terminal PTY journal remain design intent; workspace archives are
written only when requested with `phux workspace save`.

### 4.2 Format

Config is **TOML**. The config tree is shallow, so TOML's idioms
(`[table]`, `[[array.of.tables]]`, inline tables for parameterized values)
cover it without deep nesting.

A minimal config:

```toml
[defaults]
shell                 = "/bin/zsh"         # unset: $SHELL, fallback /bin/sh
term                  = "xterm-256color"   # TERM advertised to spawned panes
history-limit         = 50000
# Sane-default spawn knobs (phux-4li.1):
cwd-inheritance       = "inherit-focused"
session-name-template = "default"
window-size           = "smallest"   # geometry policy for shared Terminals (ADR-0027)
# spawn-on-attach     = "/usr/bin/some-launcher"  # default: defaults.shell

[keybindings]
prefix = "C-a"

# Bindings under the prefix.
# An action is either a bare string (no parameters) or an inline
# table whose `action` field names the action and remaining fields
# pass parameters.
[keybindings.prefix-table]
'"'        = { action = "split-pane", direction = "horizontal" }
"%"        = { action = "split-pane", direction = "vertical" }
"x"        = "kill-pane"
"c"        = "new-window"
"n"        = "next-window"
"h"        = { action = "focus-direction", direction = "left" }
"j"        = { action = "focus-direction", direction = "down" }
"k"        = { action = "focus-direction", direction = "up" }
"l"        = { action = "focus-direction", direction = "right" }
"w"        = "window-picker"
"s"        = "session-picker"
"d"        = "detach"
","        = "rename-window"

# Global table: bindings that fire without a prefix.
# Empty by default; opt in to hyper/super combos if your outer
# terminal forwards them.
[keybindings.global]
# "M-Enter" = "detach"

[status]
left   = [{ kind = "windows" }]
center = [{ kind = "help-hints" }]
right  = ["session-name", { kind = "time", format = " %H:%M" }]

[[plugins]]
manifest = "/path/to/plugin/phux-plugin.toml"
enabled = true

[[satellites]]
name = "devbox"
endpoint = "ssh://devbox"
enabled = true

[theme]
accent = "#cdd6f4"
section_header = "yellow"
```

**Spawn defaults under `[defaults]`** shape what happens when a new pane
or session comes into being:

- **`shell`** (string, default unset) is the program server-spawned
  panes run when nothing names a command: the seed session, attach-time
  session creation, and a `SPAWN_TERMINAL` whose wire frame carries no
  `command`; it is also the shell that wraps `spawn-on-attach` and
  `--seed-command` via `<shell> -c`. The server resolves it once at
  startup: `defaults.shell` when set, else `$SHELL`, else `/bin/sh`. A
  wire `command` always wins over this default (phux-i0e8.4.1).
- **`term`** (string, default `"xterm-256color"`) is the `TERM` the
  server advertises to the inner program of every spawned pane. The
  resolution order for one spawn, lowest to highest: compiled-in
  baseline → `defaults.term` → the `SPAWN_TERMINAL.term` wire field → a
  `TERM` entry in `SPAWN_TERMINAL.env` (spec L1 §3.1). The default is
  deliberately the safe xterm baseline rather than `ghostty`: ghostty's
  terminfo advertises the `fullkbd` capability, which ncurses apps read
  as "kitty keyboard protocol available" and push `CSI > N u` — and at
  least htop then fails to parse the CSI-u key reports it asked for, so
  its `q` quit dies (phux-7vx). The phux stack itself round-trips the
  kitty protocol — the phux-0o8 harness
  (`crates/phux-server/tests/kip_roundtrip.rs`) drives real TUIs through
  the full wire path under `TERM=ghostty` and proves nvim's CSI-u
  opt-in works end-to-end, with fzf/less/vim/btop regression-free — but
  the canonical ncurses reproducer (htop) remains unproven, so the
  default stays conservative. Set `term = "ghostty"` to opt into
  ghostty's extended terminfo (sixel, kitty graphics advertisement,
  ghostty SGR extensions) once the apps you run are known to round-trip
  it; apps that opt into kitty mode at runtime get it under either
  default.
- **`cwd-inheritance`** (string enum, default `"inherit-focused"`)
  controls how a freshly-spawned pane picks its working directory when a
  `SPAWN_TERMINAL` leaves `cwd` unset (an explicit `cwd` always wins).
  Values: `"inherit-focused"` (match the focused pane's CWD — tmux's
  default), `"home"` (always `$HOME`), `"session-root"` (the directory
  the session was created in), `"last-cwd-per-window"` (remember per
  window). `inherit-focused` and `home` are wired server-side
  (phux-cs6): `inherit-focused` reads the focused pane's *live* PTY
  working directory via a kernel query (`/proc/<pid>/cwd` on Linux,
  `proc_pidinfo` on macOS), so it tracks `cd` without any shell OSC 7
  setup. `session-root` and `last-cwd-per-window` are accepted but not
  yet resolved server-side (they fall back to no override); completing
  them is a phux-cs6 follow-up.
- **`spawn-on-attach`** (string, default unset) is the command `phux`
  spawns when it auto-creates a session on attach. Unset ⇒ honor
  `defaults.shell` (which honors `$SHELL`).
- **`session-name-template`** (string, default `"default"`) names
  auto-created sessions. Supports `${cwd-basename}` substitution against
  the client's working directory at session-create time. Unknown
  placeholders pass through verbatim.
- **`window-size`** (string enum, default `"smallest"`) picks one
  geometry when concurrent *views* of a single Terminal disagree on size.
  A Terminal is one PTY + one libghostty grid
  ([ADR-0027](../../ADR/0027-terminal-references-and-l3-links.md)), so it
  has exactly one authoritative `(cols, rows)`; mirrored panes or multiple
  attached clients share it, and a view that wants a different size
  letterboxes rather than reflowing the shared grid. The vocabulary
  mirrors tmux's `window-size`: `"smallest"` (use the smallest view —
  nothing is ever cropped; larger views letterbox), `"largest"` (use the
  largest view; smaller views may crop), `"latest"` (track the
  most-recently-resized view), `"manual"` (hold a fixed size, set by
  `phux resize`).

  The three view-derived values decide only among *views*. An explicit
  `phux resize TARGET COLSxROWS` is not a view: it names a size no viewport
  reported, and the server applies it immediately whether or not anyone is
  attached. What it does not do is win permanently. Under `"smallest"`,
  `"largest"`, and `"latest"` the next view event — an attach, a detach, or
  an attached client's window resize — recomputes the Terminal's geometry
  from the views and supersedes it. Under `"manual"` no view event ever
  recomputes, so an explicit resize is the only thing that sets the size and
  it holds. That is the setting to reach for when a pane must stay at a
  scripted geometry ([ADR-0062](../../ADR/0062-headless-resize-and-window-size-policy.md)).

  `phux resize` reads the server's real geometry back before exiting and
  fails loudly rather than silently, so a script never has to infer which of
  these applied — see [`agents.md`](./agents.md) §4.15.

**Experimental knobs** live under `[experimental]`. Today the only key
is `predictive-echo` (boolean, default `false`), which opts `phux attach`
into Mosh-class predictive local echo — a client-side guess for the next
keystroke, rendered with an underline, that is reconciled when the
server's authoritative output arrives. The TOML key is parsed by
`phux-config` and wired into the attach driver as `PredictiveConfig`.
The prediction set is the conservative mosh-proven subset
(single-grapheme inserts, end-of-line backspace, Ctrl-U at a known prompt
boundary, Enter, left/right arrows over known cells); a wrong guess is
stomped by the next authoritative frame, and repeated contradictions
trigger adaptive auto-backoff. Leave it unset or set it to `false` to keep
echo strictly authoritative; set it to `true` to opt in. Anything under
`[experimental]` may be renamed or removed without a SemVer bump.

**What it helps, and what it does not.** Predictive echo hides latency for
**shell-prompt typing** over a slow link — the characters you type appear
immediately instead of waiting a round trip for the server to echo them.
It is **inert in full-screen app mode**: vim/nvim, pagers (`less`), and
agent TUIs (Claude Code, codex) switch to the alternate screen, where a
keystroke is a command the program interprets rather than text the shell
echoes. The client detects the alternate screen and **predicts nothing
there**, so enabling the knob costs full-screen apps nothing (and gains
them nothing — their redraw is server-authoritative regardless). The win
also shrinks toward zero as the round trip shrinks: over the local UDS
transport the server echo is already near-instant, so the benefit is most
visible on the higher-latency remote transport
([ADR-0007](../../ADR/0007-mosh-class-transport-and-satellites.md)).

**Why it is still off by default.** Beyond the alternate-screen gate, two
main-screen cases the client cannot detect keep the default conservative:
readline **vi command-mode** at the prompt (`set -o vi`), where normal-mode
keys are mispredicted as inserts until the auto-backoff suspends (a brief
underlined flicker), and **no-echo prompts** (`sudo`/`ssh` passwords), where
echo is suppressed by the server PTY's termios — invisible to the client —
so a predicted insert would momentarily render the typed characters. Making
it safe on-by-default needs the mosh mechanisms not yet ported (an
RTT-adaptive gate and a display-timeout that expires unconfirmed ghosts);
until then it is a deliberate opt-in.

```toml
[experimental]
predictive-echo = false
```

**Plugin manifests** live under `[[plugins]]`. This is an external package
contract, not an in-process plugin host: phux validates and inspects local
`phux-plugin.toml` manifests, executes declared actions as child processes, and
keeps terminal/session state in first-party CLI surfaces. `manifest` is an
absolute path, or a path relative to `config.toml`; `enabled` defaults to
`true`.

```toml
[[plugins]]
manifest = "./plugins/agent-tools/phux-plugin.toml"
enabled = true
```

A manifest declares package metadata and argv entrypoints:

```toml
id = "example.agent-tools"
name = "Agent Tools"
version = "0.1.0"
min_phux_version = "0.0.2"
platforms = ["linux", "macos"]

[[build]]
command = ["cargo", "build", "--release"]

[[actions]]
id = "summarize"
title = "Summarize pane"
contexts = ["pane"]
command = ["python3", "summarize.py"]
# Optional: contribute a prefix-table keybinding for this action
# (chord syntax per section 5.1, e.g. "g" or "g s"). The TUI merges it
# at attach; a chord that conflicts with the user's own [keybindings]
# (exact chord or ambiguous prefix) is dropped with a logged warning —
# user config always wins. Plugin actions also always appear in the
# command palette (section 5.5) whether or not keys is set.
keys = "g"

[[events]]
id = "idle"
title = "Pane idle"
on = "pane.idle"
command = ["sh", "-c", "printf idle"]

# Optional: contribute status-bar widgets (section 8.3). Each entry is a
# widget table (kind + kind-specific options) plus a plugin-local id and
# the bar slot ("left" | "center" | "right", default "right") to append
# to. Contributions never displace user config: the TUI appends them
# after the user's own [status] widgets, and an entry whose spec fails
# widget validation is dropped with a logged warning.
[[widgets]]
id = "battery"
slot = "right"
kind = "exec"
command = "./battery.sh"
interval = "30s"

[[agents]]
id = "codex"
label = "Codex"
state = "working"
attention = "normal"
contexts = ["workspace", "pane"]

# A pane the TUI can open as a real server-side Terminal running this
# command (section 5.5). `placement` routes where it opens: "split"
# (beside the focused pane), "tab" (a new window named after `title`),
# or "zoomed" (a split that opens filling the window). "overlay" is
# accepted by the schema but NOT hosted yet — a floating live-terminal
# surface is deferred; overlay entries are skipped with a logged
# warning and do not appear in the palette.
[[panes]]
id = "board"
title = "Agent Board"
placement = "split"
command = ["agent-board"]

[[links]]
id = "ticket"
title = "Open ticket"
contexts = ["pane"]
patterns = ["https://linear.app/*"]
command = ["agent-ticket", "{url}"]

[[workspaces]]
id = "agent-bench"
title = "Agent Bench"
contexts = ["workspace"]
agents = ["codex"]
actions = ["summarize"]
events = ["idle"]

[[workspaces.panes]]
id = "board"
pane = "board"
role = "monitor"
```

`phux plugin list --json` is the stable lifecycle inspection surface for
agents and scripts; `phux config plugins --json` remains a compatibility
read path for the same configured manifests. The plugin verbs load the
user config, resolve every configured manifest, validate ids and
non-empty command argv values, reject duplicate provider ids, and emit
`schema_version = 1` JSON documents that enumerate `actions`, `events`,
`panes`, and `links`. Invalid manifests are hard failures: they are never
silently skipped, because a future runtime host should not execute a package
the config surface could not validate.

The lifecycle verbs edit `[[plugins]]` in `config.toml` without starting
a server:

```
phux plugin install https://example.com/agent-tools.git
phux plugin install ./plugins/agent-tools       # local dir or .tar/.tar.gz/.tgz
phux plugin update [example.agent-tools]
phux plugin link ./plugins/agent-tools/phux-plugin.toml
phux plugin list --json
phux plugin disable example.agent-tools
phux plugin enable example.agent-tools
phux plugin unlink example.agent-tools
```

Manifest validation includes the `min_phux_version` gate: a manifest whose
floor is newer than the running phux is rejected at link, install, and load
time with an error naming both versions (best-effort batch consumers such
as the attach TUI skip the gated plugin with a logged warning instead of
failing wholesale).

`phux plugin install REF` fetches a whole plugin package into the managed
plugins directory — `$XDG_DATA_HOME/phux/plugins`, else
`~/.local/share/phux/plugins`. `REF` is a git URL (`https://`, `git@`,
`file://`; cloned shallow with the system `git`, `--rev BRANCH_OR_TAG` picks
a ref), a local plugin directory (copied, `.git` excluded), or a local
tarball (`.tar`, `.tar.gz`, `.tgz`; extracted with the system `tar`). After
the fetch, the manifest's `[[build]]` steps for the current platform run as
child processes from the plugin root with a five-minute per-step timeout and
captured output; a failing or timed-out build aborts the install with the
step's stdout/stderr and leaves nothing linked. The validated package is
then linked into `[[plugins]]` exactly like `phux plugin link` (pass
`--disabled` to link it disabled), and its provenance — source kind, ref,
requested branch, and the resolved commit for git sources — is recorded in
the managed directory's `plugins.lock`. With `--json`, the result is a
`schema_version = 1` document under an `installed` key with `id`, `version`,
`dir`, `source`, `ref`, `branch`, `rev`, and `enabled`.

`phux plugin update [NAME]` re-fetches from the lockfile's recorded sources
(every entry, or just `NAME`), reruns the build steps, revalidates the
manifest (id changes are refused), swaps the managed copy, and records the
new resolved commit. `config.toml` is untouched because the linked manifest
path does not move. With `--json`, the result is a `schema_version = 1`
document whose `updated` array carries `id`, `version`, and `rev` per
plugin.

`phux config agents --json [--socket PATH]` projects `[[agents]]` entries
into a flat `schema_version = 2` document with `plugin_id`, `id`, `label`,
`state`, `attention`, `source`, `declared`, `runtime`, and `contexts`, so
consumers can render unknown/idle/working/blocked/done state without knowing
every plugin entrypoint. The projection is live (phux-r82.10): when a server
answers on the socket, per-pane `phux.agent/v1` records (ADR-0040) and asked
state override the declared manifest baseline; without a server the declared
values are reported with `source = "manifest"`. See
`docs/consumers/agents.md` §4.6 for the normative shape.
The config/plugin commands load the user config, resolve every configured
manifest, and validate ids and non-empty command argv values. Invalid manifests
are hard failures: they are never silently skipped, because the runtime host
should not execute a package the config surface could not validate.

`phux config run PLUGIN ACTION [--json]` executes one enabled action declared
by an inspected manifest. The runtime executes the manifest's argv directly
from the plugin root, captures stdout/stderr/exit status/duration, and kills
the child on `--timeout SECS` with wrapper exit code `125`. With `--json`, the
result is a `schema_version = 1` document containing `plugin_id`, `action_id`,
`command`, `cwd`, `outcome`, `exit_code`, `stdout`, `stderr`, and
`duration_ms`. There is no implicit shell; a plugin opts into shell behavior by
declaring `["sh", "-c", "..."]`.

`phux workspace save [--socket PATH] [--output PATH]` captures the running phux
workspace as a JSON archive. The archive records sessions, windows, pane
titles/cwds, focus, nullable commands, and layout orientation. It does not
pretend dead processes survive. `phux workspace restore ARCHIVE [--socket PATH]`
recreates missing sessions from that archive, using saved/authored cwd and
command fields where available. External packages compose this surface today:
the checked-in continuum demo autosaves/restores profile archives, and the
agent-tools demo launches and drives an `agent-bench` profile through
`phux config run`.

**Federation satellites** live under `[[satellites]]`. This is the
hub-side registry for remote phux servers; the registry name is the host
token that appears in `TerminalId::Satellite.host` — the address every
satellite-routed frame carries. `endpoint` is an opaque URI string in the
registry CRUD so `ssh://devbox`, `quic://host:8788`, and `wss://host:8787`
can share one control-plane shape; `enabled` defaults to `true`.

A server started with `phux server --hub` consumes this registry: at
startup it validates every enabled entry's endpoint by scheme (`quic://`
requires an explicit `host:port`; `ssh://` takes `[user@]host[:port]`
with a strict charset — the parts become `ssh` argv, so anything that
could read as an option or smuggle arguments is rejected) into a runtime
satellite table keyed by the registry name, and refuses to start on a
malformed enabled endpoint or a duplicate name. Disabled entries are
skipped. The hub then dials each table entry with capped exponential
backoff reconnect and routes satellite-tagged traffic over the
established links (SPEC L1 §9.1): per-terminal commands, input, and
subscribed streams relay both directions with ids re-tagged at the hub;
`phux ls` / `GET_STATE` on the hub aggregates every satellite's terminals
next to the local ones (an unreachable satellite degrades to an
un-correlated typed error, never a failed list — the CLI surfaces that as
a stderr warning and, under `--json`, as the `unreachable` list; a verb
that *resolves a target* refuses with exit `3` rather than claiming the
pane is gone, see [`agents.md`](./agents.md) §5.2); and
`phux spawn --satellite NAME` creates a terminal *on* the satellite,
returning a satellite-tagged id that routes through the hub immediately.
Without `--hub` the server ignores the registry entirely and refuses
satellite-tagged traffic with the typed `UnsupportedSatelliteRoute`.

For `quic://` and `wss://` endpoints the hub authenticates to a satellite
as an ordinary remote consumer (ADR-0038): a pairing bearer token plus a
TLS certificate-fingerprint pin, both produced by running `phux pair` on
the satellite host. The token is stored **by reference** — `token-file` is
an absolute path to an owner-only file holding the hex token (the same
shape as the server's token store); the secret never appears in
`config.toml` and is never printed by the lifecycle verbs.
`cert-fingerprint` is the satellite certificate's SHA-256 pin (64 hex
digits, optionally colon-separated; not a secret, stored inline). Routable
endpoints without both are refused, fail closed, without dialing.

`ssh://` endpoints take neither (ADR-0038 addendum): the hub spawns the
system `ssh` binary (override with `$PHUX_SSH`) running
`phux stdio-bridge` on the satellite host, which splices the connection
into the satellite server's local Unix socket. SSH authenticates and
encrypts the channel — use `BatchMode`-compatible key material (the hub
never answers a prompt) — and the bridge inherits the satellite UDS's
owner-only local trust, so `token-file` / `cert-fingerprint` on an
`ssh://` entry are ignored. The satellite host needs `phux` on the
non-interactive `PATH` of the SSH login.

```toml
[[satellites]]
name = "devbox"
endpoint = "quic://devbox.example:8788"
enabled = true
token-file = "/home/me/.local/state/phux/satellites/devbox.token"
cert-fingerprint = "AB:CD:..."
```

The lifecycle verbs edit `[[satellites]]` in `config.toml` without
starting a server:


The normal path is one capture-free command per box. Run it on the hub:

```
phux service install --hub
phux satellite enroll user@devbox
```

`enroll` verifies the satellite's `phux`, installs its always-on service,
mints and stores its credentials, and writes the complete registry entry. It
prefers pinned QUIC on a detected overlay address and falls back to
`ssh://user@devbox`; `--ssh-only` selects that fallback without probing.
The lower-level `add` form remains available for externally provisioned
credentials:
```
phux satellite add devbox quic://devbox.example:8788 \
    --token-file /home/me/.local/state/phux/satellites/devbox.token \
    --cert-fingerprint AB:CD:...
phux satellite list --json
phux satellite remove devbox
```

`add` is add-or-update and replaces the whole entry, so repeat the auth
flags when re-adding a name; omitting them clears the stored auth material.


**Outbound relay connectors** live under `[[connector]]`. Each entry names
the self-hosted reference relay endpoint this server dials and holds as a
reverse tunnel (ADR-0051/ADR-0052):

```toml
[[connector]]
relay = "relay.example:4433"
token-file = "/home/me/.local/state/phux/relay-studio.token"
cert-fingerprint = "AB:CD:..."
```

`token-file` contains the route token printed by `phux relay pair --route
ROUTE`; it must be owner-only and is re-read on every dial attempt.
`cert-fingerprint` pins the relay leaf certificate. Both are mandatory for
a routable relay and optional only on loopback for development. Unknown
keys, malformed `HOST:PORT` values, and incomplete routable entries fail
server startup before the local socket binds.

`phux server` supervises every entry independently with capped exponential
backoff. `phux server --connect HOST:PORT` selects the exact matching entry
and reuses its credentials; an endpoint not present in config is accepted
only when it is loopback. The relay token authorizes the tunnel, not a
consumer: each bridged consumer must still present a token from the server's
ordinary `phux pair` token store. See
[Remote access, Path D](../remote-access.md#path-d-via-a-reference-relay) for
the complete enrollment and rotation flow.

### 4.2.1 Validating: `phux config check`

The loader already refuses an unknown key — [`Config`](../../crates/phux-config/src/schema.rs)
carries `deny_unknown_fields`, so a typo is a hard error, not a silent
no-op. What the loader is not is *locatable*. It reports:

```text
config.toml: unknown field `enabledd`, expected one of `enabled`, `width`, `position`
```

Three things are wrong with that. It names only the leaf field, and
`enabledd` does not say which table it is in — several tables have an
`enabled`, a `width`, and a `position`. It carries no position, because what
is being deserialized is the *merged layer stack*, not your file (the loader
used to fabricate a `1:1` here; it now reports no position rather than a
confidently wrong one). And it stops at the first problem, so a config with
four typos takes four edit-run cycles.

`phux config check` fixes all three:

```console
$ phux config check
keybindings.which-key: bad value: invalid type: string "yes", expected a boolean
keybindings.wich-key: unknown key: unknown field `wich-key`, expected one of `prefix`, `prefix-table`, `global`, `which-key`, `which-key-delay-ms`
sidebar.enabledd: unknown key: unknown field `enabledd`, expected one of `enabled`, `width`, `position`
  from /etc/phux/team-baseline.toml
3 problems
```

The dotted paths come from the schema walk itself, so they cannot drift the
way a hand-maintained key list would. The `from` line appears only when the
key came from somewhere other than the file you named — with `extends`
(ADR-0039) in play, "is this typo mine or the distro's?" is the question you
actually have, and a line number in your own file would not answer it.

Once the stack deserializes, a semantic pass validates the keybindings —
the mistakes that load fine and then silently do nothing: every chord
string must parse under the chord grammar (§5.1), every action name must be
one the dispatcher actually handles (an unknown name comes with a
did-you-mean suggestion, e.g. ``unknown action `kill-pain` (did you mean
`kill-pane`?)``), and no binding's sequence may shadow another's as an
ambiguous prefix. Parameterized action *arguments* are deliberately not
validated here — argument schemas belong to the dispatcher, not the loader.

Faults are classified because they have different fixes: an **unknown key**
is a typo or a key removed in a later version; a **bad value** is a real key
with the wrong type; a **bad chord** is a binding key that does not parse
(or clashes with another binding); an **unknown name** is an action no
dispatcher arm handles. The same labels appear in the `--json` findings.

Exit codes are three-way so a dotfiles CI job can react differently to each:

| Exit | Meaning |
|---|---|
| 0 | clean, or no config file at all (the shipped defaults apply) |
| 1 | findings — the config loads nothing, or loads wrong |
| 2 | the check could not run: unreadable file, malformed TOML, cyclic `extends` |

A missing file reports `no config file (shipped defaults apply)` rather than
`ok`, because a bare "ok" would hide the common case of checking the wrong
path.

### 4.3 Reloading

Config reloads are **explicit, never automatic** (phux-foz.5). Two
surfaces trigger the same in-place reload of a running client:

- **The `reload-config` action** — a command-palette row ("Reload the
  config file"), also bindable to any chord: `R = "reload-config"` in
  `[keybindings.prefix-table]`. It ships unbound by default.
- **`phux config reload`** from any shell. The CLI validates the config
  locally first — a broken file fails right there with the parse error
  and signals nothing — then rings a reload doorbell on the server (the
  conventional L3 key `phux.config.reload/v1`, spec §3.8 of
  [`../spec/L3.md`](../spec/L3.md)) so **every** attached client re-reads
  its own config file. The config bytes never cross the wire.

A reload re-runs the full layered loader — `extends` stacks and `-append`
array merges resolve exactly as at startup — and rebuilds, atomically:
keybindings (prefix, both tables, plugin-contributed chords, the
which-key knobs), the theme, the status-bar composition (widgets,
plugin `[[widgets]]` contributions, and `[status] position`), and the
plugin action rows in the palette. Failure semantics are all-or-nothing: on any
parse or validation error the client keeps the **previous** config fully
in effect and surfaces the error as a dismissable toast — never a crash,
never a half-applied mix of old and new. This is deliberately stricter
than attach-time keybinding resolution, which degrades per binding with
a status-bar diagnostic (§5.1): a reload has a known-good previous
config to fall back on; attach does not.

Not covered by a reload (restart the client, or detach and re-attach):
pane-behavior settings read once at attach, such as `[predict]`,
`[sidebar]` geometry, and `[defaults]` (which the server owns anyway).

The file is deliberately **not watched**: watch-reload introduces a class
of "saved-mid-edit, now my keybindings are gone" papercuts, and an
explicit verb keeps a broken intermediate save inert until you ask for
it. This was the design intent recorded here before the verb shipped; it
is now the shipped behavior.

### 4.4 Theme color slots

`[theme]` is a free-form `slot = color` map. The renderer recognizes a
fixed set of named slots that color the chrome (status bar, dividers) and
overlays (help, prompt modals). Unknown slot keys are ignored; an
unparseable color keeps that slot's default. Both cases are logged at
`warn` rather than failing the load. Colors accept named values
(`"cyan"`), hex (`"#cdd6f4"`), and ANSI indices (`"12"`).

Recognized slots:

| Slot             | Default     | Used for                                  |
|------------------|-------------|-------------------------------------------|
| `accent`         | `#bef264`   | Modal titles (help / prompt border title) |
| `chord`          | `#86efac`   | Keybinding chords in the help table       |
| `action`         | terminal fg | Action labels                             |
| `dim`            | `#64748b`   | Footer hints, "no bindings" notice, sidebar branch/affordance/empty-state text |
| `border`         | `#334155`   | Modal borders + the sidebar separator rule |
| `title`          | `#bef264`   | Titles that diverge from `accent`         |
| `section_header` | yellow      | Section headings inside the help modal    |
| `error`          | red         | Error / alarm text                        |
| `surface`        | terminal bg | Modal interior background                 |
| `shadow`         | `#1c1c26`   | Modal drop shadow                         |
| `selection_fg`   | white       | Copy-mode status strip foreground         |
| `selection_bg`   | ANSI 240    | Copy-mode status strip background         |
| `attention`      | `#fbbf24`   | Agent-attention chrome (asked marker/hint, fleet-dashboard hot rows) |
| `sidebar_section`| `#64748b`   | Sidebar `spaces` / `agents` section headers + affordance action glyphs |
| `agent_idle`     | `#94a3b8`   | Sidebar agent row in the `idle` state      |
| `agent_working`  | `#86efac`   | Sidebar agent row in the `working` state   |
| `agent_blocked`  | `#fbbf24`   | Sidebar agent row in the `blocked` state   |
| `agent_done`     | `#60a5fa`   | Sidebar agent row in the `done` state      |

The default palette is deliberately **muted-chrome / bright-content**: the
always-on chrome (sidebar headers, branch sub-lines, affordances, the
separator rule, empty-state placeholders) sits in one cohesive recessive
slate scale (`#334155` → `#64748b`), so pane content and the `accent`
lime/active markers carry the eye. Every value is a slot, so a theme or
distro retints the whole chrome by overriding a handful of keys — the
bundled `herdr` distro maps this scale onto tokyonight (see
`distros/herdr/herdr.toml`).

```toml
[theme]
accent = "#bef264"
chord = "#86efac"
border = "#334155"
dim = "#64748b"
sidebar_section = "#64748b"
shadow = "#1c1c26"
```

---

## 5. Keybindings

### 5.1 The model

We support two binding tables, both always present:

- **Prefix table** (`[keybindings.prefix-table]`): bindings that fire
  after the prefix key has been pressed. This is tmux's familiar model.
- **Global table** (`[keybindings.global]`): bindings that fire any
  time. Reserved for combinations unlikely to conflict with inner
  programs — in practice, ones using `super`, `hyper`, or `meta`
  modifiers.

```toml
[keybindings]
prefix = "C-a"

[keybindings.global]
"hyper+left"  = { action = "focus-direction", direction = "left" }
"hyper+right" = { action = "focus-direction", direction = "right" }

[keybindings.prefix-table]
'"' = { action = "split-pane", direction = "horizontal" }
# ...
```

The global table is empty by default — no global bindings ship out of
the box because we cannot assume the user's outer terminal forwards
hyper/super at all. Users on Ghostty can opt in.

**A bad binding disables exactly that binding, visibly.** At attach,
keybinding resolution is lenient per binding: a chord that fails to
parse, or a binding whose sequence is a strict prefix of another's (the
later one, in table-key order, loses), is skipped — every other
binding, including `detach`, keeps working. Each skipped binding is
reported on the status-bar row as an error line naming the offending
chord and pointing at `phux config check`; when several bindings are
disabled, the line names the first and counts the rest. A `prefix`
string that fails to parse falls back to the default `C-a` so the
prefix table stays reachable. Config **reload** is the deliberate
exception to this leniency: it stays all-or-nothing (§4.3) — at attach
there is no known-good previous config to keep, but a reload has one,
so any bad binding keeps the previous config fully in effect instead of
applying a partial one.

### 5.2 The dispatcher

Bindings invoke **actions**: named identifiers with typed parameters, not
shell strings. Every action in §5.4 routes through one `run_action`
dispatch path — the command palette and the pickers commit the *same*
`ResolvedAction` a keybinding produces, so there is a single source of
truth for what each name does (see
[`action_registry.rs`](../../crates/phux-client/src/attach/action_registry.rs)).

### 5.3 Defaults

The defaults ship with `prefix = "C-a"` (tmux-shaped). Override it in one
line of config. The shipped prefix-table bindings:

| Chord       | Action                                                   |
|-------------|----------------------------------------------------------|
| `C-a "`     | `split-pane` horizontal (stacked panes)                  |
| `C-a %`     | `split-pane` vertical (side-by-side panes)               |
| `C-a x`     | `kill-pane`                                              |
| `C-a X`     | `kill-window`                                            |
| `C-a h/j/k/l` | `focus-direction` left/down/up/right                   |
| `C-a o`     | `next-pane`                                              |
| `C-a ;`     | `previous-pane`                                          |
| `C-a =`     | `last-pane` (jump back; repeat to toggle)                 |
| `C-a z`     | `toggle-zoom`                                            |
| `C-a b`     | `toggle-sidebar`                                         |
| `C-a [`     | `copy-mode`                                              |
| `C-a c`     | `new-window`                                             |
| `C-a n/p`   | `next-window` / `previous-window`                        |
| `C-a 0`–`9` | `select-window` by index                                |
| `C-a w`     | `window-picker` (grouped: sessions, windows nested)      |
| `C-a s`     | `session-picker` (`C-a a` is a kept alias)               |
| `C-a A`     | `agent-fleet` (fleet dashboard — §5.6)                   |
| `C-a q`     | `next-attention` (cycle asking panes, window + DFS order) |
| `C-a Q`     | `return-from-attention` (consume the saved local origin)  |
| `C-a C`     | `new-session`                                            |
| `C-a ,`     | `rename-window` (interactive prompt)                     |
| `C-a $`     | `rename-session` (interactive prompt)                    |
| `C-a H/J/K/L` | `resize-pane` left/down/up/right by 5                  |
| `C-a :`     | `command-palette`                                        |
| `C-a d`     | `detach`                                                 |
| `C-a ?`     | `show-help`                                              |

### 5.4 Action catalog

These are the actions the dispatcher actually handles today (the set is
kept in lockstep with `ACTION_NAMES` and the palette registry by a unit
test, so this table cannot silently drift):

| Action            | Parameters                                  |
|-------------------|---------------------------------------------|
| `split-pane`      | `direction` (`horizontal` \| `vertical`)    |
| `kill-pane`       |                                             |
| `new-window`      |                                             |
| `kill-window`     |                                             |
| `next-window`     |                                             |
| `previous-window` |                                             |
| `select-window`   | `index`                                     |
| `rename-window`   | `name?` (bare opens an interactive prompt)  |
| `rename-session`  | `name?` (bare opens an interactive prompt)  |
| `focus-direction` | `direction` (`left`/`right`/`up`/`down`)    |
| `resize-pane`     | `direction`, `amount`                       |
| `next-pane`       |                                             |
| `previous-pane`   |                                             |
| `last-pane`       | jump to this attached client's previous focus |
| `next-attention`  | cycle asking panes in deterministic window + DFS order |
| `return-from-attention` | return once to the client-local saved origin |
| `toggle-zoom`     |                                             |
| `toggle-sidebar`  |                                             |
| `copy-mode`       |                                             |
| `show-help`       |                                             |
| `command-palette` | (opens the palette — §5.5)                  |
| `context-menu`    | (opens the focused pane's context menu — §7.1, ADR-0058) |
| `window-picker`   | (opens the grouped window picker — §5.5)    |
| `session-picker`  | (opens the session picker — §5.5)           |
| `agent-fleet`     | (opens the fleet dashboard — §5.6)          |
| `focus-pane`      | `window`, `pane` — focus a pane by window index + DFS leaf ordinal (committed by fleet rows, §5.6) |
| `new-session`     | `name?` (bare opens an interactive prompt)  |
| `switch-session`  | `name`, `window?`, `pane?` (re-attaches this client; `window` selects that window index after the switch — §5.5; `pane` then focuses that DFS leaf ordinal — the one-step cross-session pane pick the fleet's foreign rows commit, §5.6) |
| `detach`          |                                             |
| `take-input`      | seize the focused pane's input lease (ADR-0033) |
| `give-input`      | release the focused pane's input lease (ADR-0033) |
| `signal-terminal` | `signal` = `interrupt`\|`freeze`\|`resume`\|`terminate`\|`kill` (ADR-0033) |
| `set-pane`        | `mouse` = `on`\|`off`\|`toggle` — per-pane mouse opt-out (§7, ADR-0048) |
| `plugin-action`   | `plugin`, `action` — run a plugin manifest action (§5.5) |
| `plugin-pane`     | `plugin`, `pane` — open a plugin manifest pane (§5.5) |
| `reload-config`   | re-read the config and apply it in place (§4.3) |

### 5.5 Command palette and pickers

`command-palette` (`C-a :`) opens a filterable overlay listing every
action, each annotated with its currently-bound chord. Rows are grouped
under dim category headers — **Pane**, **Window**, **Session**, **View** —
when the query is empty; as you type, the headers fall away and the
matches are ranked best-first by a scored fuzzy match (contiguous runs,
word-boundary hits, and earliness all raise a row's rank), so typing `sp`
floats `split-pane` to the top. Enter commits the selected row through the
same `run_action` path a keybinding takes.

The rows are a **scroll viewport**, not the whole list: a palette (or
picker) with more rows than fit the box shows a window onto them, always
kept around the selection, and paints a scrollbar in the right border
column whose thumb shows how much list there is and where you are in it.
Navigate with arrows / `C-n` / `C-p` (`j` / `k` too while the query is
empty), `PageUp` / `PageDown` for a screenful, `Home` / `End` for the ends,
or the mouse wheel. Every list overlay shares this — the pickers and the
agent dashboard (§5.6) as much as the palette.

Enabled plugins' manifest `[[actions]]` appear under a trailing
**Plugin** header, one namespaced row per action
(`plugin: <plugin-name>: <action title>`). Committing one runs
`plugin-action { plugin, action }`, which executes the manifest's argv
through the same child-process runtime as `phux config run PLUGIN
ACTION` — spawned off the input loop, so a slow plugin never freezes the
TUI. A failed run (non-zero exit, timeout, or spawn error) pops a
dismissable toast showing the captured output; successes only log. A
manifest action may also declare `keys = "..."` to contribute a
prefix-table binding (see the plugin-manifest block in §4.2); user
config always wins on conflict, and the palette row shows whichever
chord actually ended up bound.

Manifest `[[panes]]` share the same **Plugin** header, one row per
hostable pane (`plugin pane: <plugin-name>: <pane title>`). Committing
one runs `plugin-pane { plugin, pane }`, which opens a real server-side
Terminal running the pane's argv through the same `SPAWN_TERMINAL` verb
`split-pane` / `new-window` use — no plugin-privileged wire surface
(ADR-0017); any consumer could do the same. The spawn's working
directory is the plugin root, and the child sees `PHUX_PLUGIN_ID`,
`PHUX_PLUGIN_PANE_ID`, and `PHUX_PLUGIN_ROOT` on top of the server's
environment (the pane counterpart of the action runtime's identity
variables). The manifest's `placement` routes where it opens:

- `split` — beside the focused pane (side-by-side), like `split-pane`.
- `tab` — a new window named after the pane's `title`.
- `zoomed` — a split whose new pane opens zoomed to fill the window;
  `toggle-zoom` reveals it tiled beside the anchor pane.
- `overlay` — **not hosted yet.** A floating live-terminal overlay is a
  larger chrome surface than the current overlay stack (modal select
  lists and prompts) supports; entries declaring it are skipped with a
  logged warning and never listed. The declaration remains valid
  manifest schema so packages can ship it ahead of the host.

Unlike `[[actions]]`, panes contribute no keybindings today; a user can
still bind one manually with a parameterized action
(`{ action = "plugin-pane", plugin = "...", pane = "..." }`).
Disabled plugins (`enabled = false`) contribute no rows.

The **session picker** (`session-picker`, `C-a s`, alias `C-a a`) lists the
server's other sessions; choosing one re-attaches this client to it
in-process (`switch-session`). A trailing "+ New session" row creates one.

The **window picker** (`window-picker`, `C-a w`) is hierarchical: every
session is a section header with its windows nested beneath it. Choosing a
window in the **current** session switches to it directly
(`select-window { index }`). Other sessions' windows are **one-step
jumps**: the client fetches each peer session's persisted layout right
after attach, so the picker lists their windows (`index:name`, pane
count) too, and choosing one commits `switch-session { name, window }` —
a single Enter re-attaches to that session and selects that window once
its layout loads. A peer session with nothing persisted yet (or one
created after this client attached) falls back to a single "switch to
this session" row; its own picker then lists its windows. The cached
foreign layouts are an attach-time snapshot: if a peer rearranged its
windows since, the jump still switches sessions and the stale window
index degrades to the session's own remembered focus (logged, no bell).

### 5.6 Agent-fleet dashboard

The **agent-fleet dashboard** (`agent-fleet`, `C-a A`) is the one-view
answer to "which of my agents needs me?": a filterable overlay listing
every pane of the attached session, grouped under session headers, each
row carrying

- the agent's **name and kind** from its structured `phux.agent/v1`
  record (ADR-0040) when one is present — declared by an agent or derived
  by the server ([ADR-0046](../../ADR/0046-server-side-agent-state-detection.md),
  so the state glyph below is live for a recognized agent CLI rather than
  permanently `?`) — falling back to the pane's OSC title otherwise (the
  record outranks the title);
- a one-character **state glyph**: `!` blocked, `*` working, `-` idle,
  `.` done, `?` unknown (also used when no record is declared);
- an **attention highlight** — the row's label paints in the theme's
  `attention` slot (§4.4, the same amber as the sidebar marker and the
  status-bar asked hint) when the pane has a pending ADR-0035 question or
  its record declares/derives high attention;
- the pane's **branch or cwd** in the dimmed right column, next to the
  state word (`working - main`), from the same client-local `.git/HEAD`
  read as the sidebar branch line.

Enter focuses the chosen pane: current-session rows commit
`focus-pane { window, pane }` through the single dispatch path (switching
the window and moving its client-local focus in one step). Rows under
**other** sessions are **one-step cross-session pane focus** (phux-jpqd):
each pane of a peer session with a cached persisted layout commits
`switch-session { name, window, pane }`, so a single Enter re-attaches to
that session, selects the window, and focuses that pane — with the peer's
agent glyph and state already shown on the row (blocked, working, idle,
done, or `?`). A peer session with nothing persisted yet (or created after
this client attached) falls back to a single "switch to this session" row
as before. The dashboard grows no wire surface for this (ADR-0030): it
reuses the same lazy per-pane L3 reads the window picker uses (phux-foz.8,
ADR-0018) — the peer's persisted `phux.tui.layout/v1` workspace for the
pane tree, plus a one-shot `GET_METADATA` on each foreign pane's
`phux.agent/v1` record for its identity. Foreign rows therefore carry no
asked flag or branch/cwd — those need a live per-pane subscription, so the
record's declared state is the honest maximum until you attach there. The
`phux agent list` CLI remains the exhaustive cross-session projection.

The dashboard is **live**: while it is open, agent-record changes, asked
events, pane spawns/closes, and layout changes rebuild its rows in place
(push, not poll) without disturbing your query or selection. It shares
the palette's fuzzy filter, `j`/`k` / arrows / `C-n`/`C-p` navigation,
and Esc dismissal. No new theme slots: headers use `section_header`,
secondaries `dim`, hot rows `attention`.

### 5.7 Which-key popup

Press the prefix and hesitate, and a small floating panel lists every
prefix-table continuation — key on the left, action on the right — built
from your live bindings (rebinds included; it is the same config snapshot
the help overlay reads). The numeric window-jump keys collapse into a
single `0-9` row.

The popup is display-only and never captures input:

- **Any key** dismisses it and executes its binding exactly as if the
  popup had never appeared. A continuation typed *before* the delay
  elapses suppresses the popup entirely — it can never eat or delay a
  chord.
- **Esc** dismisses it and cancels the pending prefix (nothing is sent to
  the pane).

Configured under `[keybindings]`:

```toml
[keybindings]
which-key = true          # default; false disables the popup
which-key-delay-ms = 600  # hesitation before it appears
```

### 5.8 Copy-mode

`C-a [` enters copy-mode on the focused pane. Copy-mode is **client-local**:
it is a projection over the pane's own libghostty engine, and nothing about a
selection touches the wire — the client extracts the selected text from its own
`Terminal` and writes it to the *host* clipboard via OSC 52. This is
[ADR-0045](../../ADR/0045-client-side-copy-mode.md) applied on top of
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md);
there is no server round-trip, no selection frame, and no clipboard verb on the
protocol.

Movement and viewport:

- **Arrow keys** move the selection cursor; hold **Shift** to extend the
  selection from its anchor instead of moving both ends.
- An arrow past the top or bottom edge, and **PageUp** / **PageDown**, scroll
  the pane's client-local viewport into mirrored scrollback. Selection is
  bounded by the scrollback the client already holds, not the server's full
  history.

Selection modes — a two-corner rectangle interpreted as one of:

- **Char** (default): linear, text-flow selection — full interior rows, partial
  first and last rows.
- **Line**: whole lines.
- **Rect**: rectangular (block/columnar) selection — the column band on every
  row in the span. **Tab** (in copy-mode) rotates Char → Line → Rect. The
  on-screen highlight and the extracted text are computed from the same
  `SelectionRect`, so a block selection copies exactly the band it highlights.

One-shot grabs resolve against the engine at the cursor and copy-and-exit
immediately (tmux-style):

| Key | Grab                                                              |
|-----|-------------------------------------------------------------------|
| `w` | word under the cursor (`select_word`)                             |
| `v` | whole line under the cursor (`select_line`)                       |
| `V` | line bounded by semantic-prompt (OSC-133) state changes           |
| `A` | all selectable content (`select_all`)                             |
| `]` | the command-output span under the cursor (`select_output`); a no-op when the pane has no OSC-133 zones |

- **Enter** copies the current two-corner selection to the host clipboard and
  exits.
- **Esc** exits copy-mode without copying.

Mouse: a left-button drag inside the pane selects and, on release, copies and
exits; the wheel scrolls the client-local viewport. A click with no drag simply
exits, so a mouse-initiated entry can never trap the keyboard. See §11 for the
scope boundary — phux does not reimplement selection boundaries or a clipboard
format path; it delegates both to libghostty and the host terminal.

Resizing the terminal **keeps** copy-mode open — it is a selection over the
live pane, not a box pinned to the screen, so a resize must not discard a
selection you are still building (the context menu is the opposite case, §7.1).
The selection adopts the pane's new size instead: a shrink pulls both corners
back inside the pane, a grow makes the newly revealed rows and columns
reachable, and a Line-mode selection re-spans to the new width (phux-d26y).

---

## 6. Layout

### 6.1 The tree

A window's layout is a **binary split tree**: each interior node is a
split (horizontal or vertical) with a single `ratio` in `(0, 1)` and
exactly two children; leaves are panes. Three-way and N-way splits
are represented as nested binary splits. See
[ADR-0012](../../ADR/0012-binary-split-tree-layout.md) for the closed
decision behind this shape and the wire form in
[`../spec/L3.md`](../spec/L3.md) §3.2.

```
window: split(vertical, ratio = 0.5)
        ├── pane #0
        └── split(horizontal, ratio = 0.33)
            ├── pane #1
            └── pane #2
```

(The first ratio gives pane #0 the top half of the window; the second
gives pane #1 the left third of the bottom half.)

Tabbed layout nodes are reserved for the v0.2 wire spec (see
[`../spec/CHANGELOG.md`](../spec/CHANGELOG.md)).

The client-side rendering surface for this tree — multi-pane tiling,
borders, focus chrome, input routing to the focused pane, layout
persistence in L3 metadata under `phux.tui.layout/v1`, and the
keybind-action wiring — is settled by
[ADR-0019](../../ADR/0019-tui-multi-pane-rendering.md) and tracked under
the `phux-4li` epic.

### 6.2 Resize behavior

<!-- impl-status: shipped; probe: frozen -->
> **Status:** Viewport-driven reflow ships. Automatic minimum-size freezing
> now also ships (phux-foz.3): proportional re-flow and freezing are
> implemented in the layout walk itself, so paint, reflow
> (`TERMINAL_RESIZE` sizing), and mouse hit-testing all read the same frozen
> tiling.

When the client viewport (or server-aggregated viewport for multi-client
sessions) resizes, split ratios are preserved and dimensions are
redistributed proportionally. A leaf that hits its minimum size
(`min_cols = 2`, `min_rows = 1` for the inner content; chrome is per
client) freezes; remaining space redistributes among non-frozen leaves.
This mirrors tmux's resize behavior.

Below the layout's aggregate minimums (every leaf at its floor plus one
cell per interior divider) freezing disengages and pure proportional
tiling resumes: panes degrade to sub-viable rectangles rather than
disappearing, and the exact-tiling invariant (no gaps, no overlaps)
holds at every viewport size.

### 6.3 Resize commands

<!-- impl-status: shipped; probe: resize-pane -->
> **Status:** Keyboard `resize-pane` actions and mouse divider dragging ship
> (ADR-0048, phux-foz.3). `resize-pane` dispatches through the
> single-dispatch action registry, `C-a H/J/K/L` are the default bindings
> (see §5.3), the command palette offers a resize row, and drag-on-divider
> (§7) commits through the same ratio math.

`resize-pane direction=right amount=5` moves the boundary between the
focused pane and its right neighbor by 5 columns toward the right,
giving the focused pane more width. Negative amounts shrink.

Resize commands modify the relevant interior node's `ratio` (not
absolute sizes). After a subsequent window resize, the new ratio is
preserved.

A resize that would push either side of the boundary below 2 cells on
the resize axis is a bell-no-op (ADR-0019 decision 5). The gate measures
the ratio's *proportional* tiling — what the ratio asks for — not the
frozen tiling of §6.2, so a command cannot silently bank ratio behind a
frozen divider that the layout would snap to on the next viewport grow.
The new layout broadcasts to other attached clients via `SET_METADATA`
(`phux.tui.layout/v1`), like every other layout mutation.

### 6.4 Window sidebar

<!-- impl-status: shipped; probe: toggle-sidebar -->
> **Status:** Shipped (`phux-4h5a`; herdr-shaped by `phux-p4vp`;
> interactive per `phux-fce4`; sectioned + agent-aware per `phux-foz.9`).

`[sidebar]` docks a vertical strip on the left (default) or right edge;
`toggle-sidebar` (`C-a b`) flips it at runtime, and so does clicking the
collapse chevron in the strip's bottom corner. Panes tile into the
remaining content rect, so the strip never overlaps content.

The strip runs the **full height** of the terminal, and the status bar
yields its columns rather than spanning underneath it (`phux-qtw8`): with
the sidebar open, the bar — window tabs included — starts beside the
strip. The three regions tile the viewport without overlap, and a click
in the strip's columns is the strip's, on every row.

The strip is laid out herdr-style in two labelled sections, headed by
muted lowercase headers (the `sidebar_section` theme slot):

**`spaces`** — one fixed **two-row block** per window, top to bottom in
`select-window` index order:

- **Name row.** A status dot (filled + `accent` for the active window,
  hollow + `dim` otherwise, `attention` amber when a pane in the window
  is waiting on a human) followed by the window's bold display label
  (agent record, OSC title, or stored name — same resolution as the
  status-bar tab strip), plus the §8.6 attention `!`.
- **Branch row.** The VCS branch of the window's focused pane, dim and
  nested under the label (`main`, a `wave2/...` branch, or a short
  commit hash for a detached HEAD). Blank when the pane's working
  directory is not inside a git repository.

**`agents`** — one row per agent-running pane: a lifecycle glyph, the
window's stored name, and `state - agent-name` (e.g. `idle - claude`,
`working - merge-queue-w5`), colored by the `agent_idle` /
`agent_working` / `agent_blocked` / `agent_done` theme slots (an
undeclared state renders in `dim`). Per pane, in preference order:

1. **The structured `phux.agent/v1` record** (ADR-0040). The server
   derives and writes this record for a pane it owns
   ([ADR-0046](../../ADR/0046-server-side-agent-state-detection.md)), so
   the four lifecycle states are live for a recognized agent CLI with no
   integration on the agent's side: `working` while it runs, `blocked`
   when it is waiting on a human, `idle` otherwise. An explicit
   `phux agent set` outranks the derivation for whatever fields it
   supplies, so a wrapper or hook that declares its own state still wins.
2. **The OSC-title identity heuristic** — the compatibility path for a
   pane the server did not recognize (an unknown agent, or a platform
   where process introspection is unavailable). The name comes from the
   title token; the state is `blocked` while the pane's §8.6 asked flag
   is up, else `idle`. Screen text is never scanned on the render path.
   Title changes refresh the chrome directly: the client diffs each
   pane's title as content frames apply, so the row appears when the
   agent sets its title and disappears when the shell resets it on exit.

Rows are ordered by **how much they want a human**, not by window index:

    blocked  >  done (unvisited)  >  working  >  done/idle (visited)  >  unknown

"Finished, and you have not looked at it yet" therefore sorts above
"still working" — the whole point of the section is to answer "which of
my agents needs me?" without reading it top to bottom. Ties break by most
recent state change, then by window order. A pane is **seen** once you
focus it; a *new* state landing on a pane you are not looking at marks it
unseen again, so an agent that finishes in a background window rises back
to the top rather than staying quietly settled from an hour ago.

Panes matching neither source produce no row — the section lists agents,
not shells. When no pane matches, the section still renders its header
with a quiet `no agents` empty-state line (dim + italic) rather than
vanishing, so the strip reads as two composed sections; the `spaces`
section shows an equivalent `no spaces` placeholder when there are no
windows. The empty-state lines are inert — never click targets. (A short
strip that cannot fit the gap + header + one row drops the whole agents
section as before.)

Two environment knobs govern the server-side derivation (the client has
no switch of its own; it renders whatever the record says):

| Variable | Effect |
|---|---|
| `PHUX_AGENT_DETECT=0` | Disable detection entirely. Rows fall back to the OSC-title heuristic, exactly as before ADR-0046. |
| `PHUX_AGENT_RULES_DIR=<dir>` | Load agent rule manifests from `<dir>` instead of `$XDG_CONFIG_HOME/phux/agent-rules`. A manifest replaces the built-in of the same `kind`. |

See [`../operations.md`](../operations.md) for what the detector reads and
how a bad manifest surfaces.

Branch inference is **client-local and read-only**: the pane's working
directory (carried by the `ATTACHED` snapshot) is walked up to the
enclosing `.git`, worktree gitfiles (`gitdir: ...`) are resolved, and
`HEAD` is read directly — one cached file read, never a `git`
subprocess, and nothing added to the wire. The cache re-validates on a
short TTL keyed by `HEAD`'s mtime, so a `git switch` shows up on the
next chrome refresh without stat storms.

The strip's last two rows are the bottom-anchored **interactive
affordances** (`phux-fce4`), with the collapse chevron in the bottom
corner cell; window blocks and agent rows are click targets too. Every
sidebar click commits the same `ResolvedAction` a keybinding or palette
row would — one `run_action` dispatch path, no bespoke click semantics:

| Target                      | Committed action                       |
|-----------------------------|----------------------------------------|
| A window block (either row) | `select-window { index }`              |
| An agent row                | `select-window { index }` for the window holding the agent's pane |
| `+ new`                     | `new-window`                           |
| `= menu`                    | `command-palette` (the session/plugin menu; `new-session` lives in its Session group) |
| The collapse chevron        | `toggle-sidebar`                       |

Pointer events over the strip never leak into pane routing: presses on
section headers, blank rows, or the separator column are consumed and
dropped. The same targets stay keyboard-reachable through their actions
(`C-a c`, `C-a :`, `C-a b`, `C-a 0`–`9`).

---

## 7. Mouse

<!-- impl-status: shipped; probe: MouseEvent -->
> **Status:** Shipped (ADR-0048; per-pane opt-out in phux-npb3).
> Click-to-focus, drag-on-divider to resize, and default outer-terminal
> mouse capture are implemented. The client enables its own mouse
> tracking on attach so divider drags work without an inner program
> turning mouse mode on. Opt-outs: the global `mouse = false` config,
> and the per-pane `set-pane mouse off` action described below.

Mouse handling is enabled by default. On attach the client emits DECSET
`?1002h` (button-event tracking) + `?1006h` (SGR coordinates) for the
*outer* terminal and restores them on detach. That capture is what makes
drag-to-resize work in a plain shell: without it the client is deaf to
the pointer over a divider whenever the inner program has no mouse mode.

| Event                    | Action                                |
|--------------------------|---------------------------------------|
| Click in pane            | Focus the pane, then forward to it    |
| Press on a divider       | Grab the boundary for a resize drag   |
| Drag a divider           | Resize the boundary (tracks pointer)  |
| Release                  | Commit the new layout (broadcast L3)  |
| Scroll wheel in pane     | Layered: an inner program that        |
|                          | enabled mouse mode gets the wheel     |
|                          | forwarded; otherwise on the primary   |
|                          | screen the wheel scrolls the pane's   |
|                          | client-local scrollback viewport, and |
|                          | on the alt screen it becomes arrow    |
|                          | keys (xterm alternate scroll, DECSET  |
|                          | 1007 — on by default, apps opt out    |
|                          | with `?1007l`). In copy-mode it       |
|                          | scrolls the focused pane's local      |
|                          | viewport                              |
| Right-click in pane      | Opens the pane context menu (§7.1);   |
|                          | forwarded to the inner program        |
|                          | instead when that program has mouse   |
|                          | tracking on                           |
| Click on status bar row  | A `windows`-widget tab selects that   |
|                          | window (`select-window { index }`,    |
|                          | phux-foz.12); every other cell on the |
|                          | row is consumed as chrome (no-op)     |
| Right-click on the bar   | A tab selects its window and opens    |
|                          | the window menu; elsewhere on the row |
|                          | opens the session menu (§7.1)         |
| Click on a sidebar row   | Select that window (window blocks and |
|                          | agent rows); `+ new` / `= menu` / the |
|                          | collapse chevron run their actions    |
|                          | (§6.4)                                |
| Right-click the sidebar  | A window or agent row selects that    |
|                          | window and opens the window menu;     |
|                          | every other cell opens the session    |
|                          | menu (§7.1)                           |

Only divider cells change meaning. Every event inside a pane's rectangle
is forwarded to that pane with pane-local coordinates, so an inner TUI
(vim, htop) that turns mouse tracking on still receives its mouse events
— the server's per-pane encoder produces empty bytes for a pane whose
inner app has no mouse mode, so forwarding is harmless either way.

**Native selection.** Enabling outer capture suppresses the host
terminal's click-drag text selection inside the phux viewport. Hold
**Shift** to bypass application mouse reporting and use native selection
(a near-universal terminal convention; phux relies on it but does not
enforce it). A host that does not honour Shift-bypass needs
`mouse = false` for easy selection.

**Escape hatches.** `mouse = false` in `[defaults]` skips the DECSET
entirely and reverts to pass-through-only (the client only sees mouse
when an inner program enables it).

Per-pane (phux-npb3): the `set-pane` action with `mouse = "on"`,
`"off"`, or `"toggle"` (bindable, and offered by the command palette as
a toggle) opts the *focused* pane out of client mouse handling without
touching its siblings. The state is client-local and capture follows
focus: while an opted-out pane is focused the client drops its own
mouse-tracking DECSET, so the host terminal's raw handling (native
click-drag selection and friends) returns for that pane; focusing any
opted-in pane re-enables capture and drag-to-resize. While capture is
on (another pane focused), a click on the opted-out pane still focuses
it — that is the mouse path back in — but the client never synthesizes
`INPUT_MOUSE` (or the local wheel viewport scroll) for an opted-out
pane. Nothing crosses the wire; a pane's opt-out ends when it closes.

We do not ship copy-mode mouse drag selection — see §11.

### 7.1 Context menus

<!-- impl-status: shipped; probe: ContextMenu -->
> **Status:** Shipped (ADR-0058).

The right button opens a menu anchored at the pointer, listing the
actions that apply to what you clicked. Three menus, one per target:

| Right-click on            | Menu    | Rows                                |
|---------------------------|---------|-------------------------------------|
| A pane                    | pane    | Split right, Split down, Zoom /     |
|                           |         | Unzoom, Copy mode, All commands…,   |
|                           |         | Close pane                          |
| A status-bar tab or a     | window  | New window, Rename window…, Pick    |
| sidebar window/agent row  |         | window…, All commands…, Close       |
|                           |         | window                              |
| Any other chrome cell     | session | New window, Pick window…, Pick      |
|                           |         | session…, Rename session…, Agent    |
|                           |         | fleet, Toggle sidebar, All          |
|                           |         | commands…, Detach                   |

A menu row commits the same `ResolvedAction` a keybinding would, so it
runs through one dispatch path and nothing is menu-only. Each row shows
the chord bound to it, when there is one.

Right-clicking a window's tab or sidebar row **selects that window
first**, then opens the menu for it — the menu acts on what you pointed
at, not on whatever was active.

**Driving one.** Both idioms work, with no mode to choose:

- Press, drag onto a row, release — the row under the pointer is picked.
- Press and release, move, then click a row — a left or right press on a
  row picks it.

The pointer hovers rows as it moves (the client raises `?1003h`
any-motion reporting for as long as a menu is open, and drops it again
on close). Arrow keys, `j` / `k`, `C-n` / `C-p`, `Home` / `End` move the
selection; `Enter` picks; `Esc`, `q`, or a click outside dismisses. A
click on the menu's border or on a separator does nothing.

**Panes that own the mouse.** An inner program with mouse tracking on
(vim, htop, an agent TUI, anything with its own right-click menu) keeps
every button, so no menu opens over it — the same boundary drag-to-copy
respects. Bind the `context-menu` action to open the pane menu from the
keyboard there, or use the command palette. A pane opted out via
`set-pane mouse off` has no menu either, by the same logic.

The menu never covers the sidebar or the status-bar row: it is clamped
into the pane content rect, flipping left and up at the edges, so a
click on a bottom-docked bar opens the box upward over the panes.

**Resizing closes it.** A menu is pinned to the cell you clicked, against
the viewport that existed then, so a terminal resize invalidates its box
and the client drops it (phux-fsb). Every other overlay — the palette,
the pickers, help, prompts — lays itself out fresh on each paint and
reflows into the new size instead.

---

## 8. Status bar

### 8.1 Architecture: widget-first from day one

The status bar is **rendered entirely client-side**. A GUI client may
ignore it and render its own chrome; the TUI client composes it from
widgets and draws it on one reserved row of the outer terminal — the
bottom row by default, or the top row with `position = "top"`.

Every slot's contents are a list of **widgets**. A widget is a typed
thing that produces styled text. The default config looks short because
a bare string is shorthand for a no-parameters widget:

```toml
[status]
left   = ["session-name"]                               # → [{ kind = "session-name" }]
center = []
right  = [{ kind = "time", format = " %H:%M" }]
position = "bottom"   # or "top"; default "bottom"
```

`position` moves the whole reserved row: with `"top"` the bar draws on
the outer terminal's first row and the panes shift down one row, so
nothing ever underlaps the bar. The sidebar strip is the exception — it
is full-height in both positions, and the bar insets out of its columns
instead (§6.4). Everything else — widgets, styling, refresh — is
identical in both positions.

There are three categories of widgets:

1. **Server facts.** The server already publishes session names, window
   lists, focused pane, cwd (via OSC 7), last command exit (via OSC
   133). These are widget kinds (`session-name`, `windows`, `cwd`,
   `exit`, etc.) backed by data the server pushes anyway.
2. **Client-local widgets.** Things derivable on the client without
   server help: `time`, `mode`, `key-indicator` (last key chord).
3. **`exec` widgets.** The client runs the named program on the
   configured interval and renders its stdout (parsed for SGR if it
   contains ANSI). These run per-client; a clipboard daemon, a battery
   percentage, etc.

```toml
right = [
    { kind = "exec", command = "~/.local/bin/battery", interval = "30s" },
    { kind = "text", value = " | " },
    { kind = "time", format = "%H:%M" },
]
```

### 8.2 Why widget-first

The scoping decision in `CONTRIBUTING.md` is that we will not ship a
status bar *DSL* — no `if/else` mini-language, no format-template
expression engine. The widget system gets us extensibility without
becoming a template interpreter: arbitrary logic lives in `exec`
widgets, which are real programs in real languages, supervised by the
client. The widget contract itself is small and typed.

This shape costs us almost nothing on day one (the default config is
three names in three lists), and means we never have to do an
architectural revision to grow a status bar plugin story.

### 8.3 Built-in widget kinds

| Kind            | Parameters                                                   |
|-----------------|--------------------------------------------------------------|
| `session-name`  | `format?` (default: `"{name}"`; `{name}` is the truncated session name), `prefix?` (literal, prepended), `max-len?` (chars, `> 0`) — **implemented** |
| `time`          | `format` (strftime) — **implemented**                       |
| `windows`       | `active?`/`inactive?` (style tables), `separator?`, `format?` (`{index}`/`{name}`) — **implemented**; tabs are click targets (`select-window`, phux-foz.12) wherever the widget sits (any slot, top or bottom bar) |
| `help-hints`    | prefix-aware help / palette / copy affordances — **implemented** |
| `window`        | `format?` (default: `"{name}"`)                              |
| `pane`          | `format?`                                                    |
| `cwd`           | `format?`, `truncate?` (chars; keeps the path tail), `$HOME` collapses to `~` — **implemented** |
| `exit`          | `format?` (`{code}` placeholder; last command exit code, OSC 133) — **implemented** |
| `host`          | `format?`                                                    |
| `mode`          | `format?` (current input mode)                               |
| `key-indicator` | shows the last key/chord pressed; reserved for v0.2          |
| `text`          | `value` (literal styled text)                                |
| `spacer`        | flexible expanding space; no parameters                      |
| `exec`          | `command` (string via `/bin/sh -c`, or argv array), `interval?` (default `5s`, floor `1s`), `parse-ansi?` (true) — **implemented** |

Every widget kind accepts a `style` table with optional `fg`, `bg`
(color strings: names, `#rrggbb`, or palette indices), and the boolean
attributes `bold`, `dim`, `italic`, `underline`, `reverse`. The registry
applies it uniformly, so no widget can opt out. Precedence: cells the
widget styles itself keep their own style — `windows`' `active`/
`inactive` segments, `exec`'s SGR-parsed output, `help-hints`' dim base
all win — and only cells the widget left plain inherit the widget-level
`style`. A `style` value that is not a table, or a table with an unknown
field, fails the bar build and is flagged by `phux config check`.

Widget options are a **closed surface**: every factory rejects an
option outside its documented set, naming the widget and suggesting the
nearest valid spelling ("unknown option `formt` (did you mean
`format`?)"). `phux config check` runs the same build path over
`[status]`, so a typo'd kind, a typo'd option, or a bad style table
surfaces as a located finding instead of parsing clean and doing
nothing.

The implemented built-ins today are `session-name`, `time`, `windows`,
`help-hints`, `cwd`, `exit`, and `exec` (the others above are design
intent); `windows` takes its `active` and `inactive` segments as such
style tables. Plugin manifests can contribute additional widget entries
via `[[widgets]]` (section 7's manifest contract): each contribution is a
widget table plus a `slot`, appended after the user's own widgets, and a
contribution that fails validation is dropped with a logged warning
rather than degrading the bar.

Data feeds behind the server-fact widgets: `cwd` renders the focused
pane's live directory from `cwd_changed` events (the server queries the
PTY child's kernel cwd at OSC-133 prompt boundaries and on output
settle; the `ATTACHED` snapshot's spawn cwd seeds it), and `exit`
renders `command_finished.exit_code` (the OSC-133 `D`-mark code, so it
requires shell integration). `exec` widgets never run on the render
path: the client runs the command per `interval` as a bounded
`kill_on_drop` child process (10s hard cap) and folds captured stdout —
first line only — into a cached strip the widget renders; a failed or
timed-out run keeps the last good output.

### 8.4 Refresh and ordering

- Server-fact widgets re-render on the relevant server event (window
  rename, focus change, OSC 7/133).
- Client-local widgets with no interval re-render only on event.
  `clock` re-renders every minute by default; `interval` overrides.
- `exec` widgets re-render every `interval`. The client batches
  re-renders to once per frame (max ~60 Hz).
- Slot contents render left-to-right with no implicit separator. Use
  `text` widgets for separators.

### 8.5 What the status bar is not

- Not multi-row. One row — bottom of the outer terminal by default,
  top with `position = "top"` (§8.1). If you need more, dedicate a
  pane.
- Not themable via a styling engine. Per-widget `style` tables only.
- Not server-rendered. Every client owns its chrome. This is what
  enables a future GUI client with native chrome to coexist with the
  TUI client trivially.

### 8.6 Agent attention (the asked chrome)

When an agent in a pane blocks for a human answer, the server emits
`AgentEvent::Asked` on the subscribed event stream
([ADR-0035](../../ADR/0035-agent-asked-event.md); detection sources in
[ADR-0036](../../ADR/0036-agent-asked-detection.md)). The interactive
TUI folds that event into per-pane state (the same fold as the
ADR-0033 `TerminalControl` badge) and renders it on every chrome
surface that names windows, colored by the `attention` theme slot
(§4.4):

- **Window tab marker.** The asking pane's window gets a ` !` suffix on
  its tab, in both the sidebar strip and the status bar's `windows`
  widget — including for a background window, so the question is
  findable from anywhere. (The sidebar marker is themed; the `windows`
  widget marker rides the segment's own style, like the zoom `Z`.)
- **Status-bar hint.** A right-aligned `[ ASK ]` chip on the bar row
  (`[ ASK xN ]` when several panes are asking), sitting left of the
  ADR-0033 supervisory badge when one is up.

**Jump and return.** `C-a q` (`next-attention`) jumps to the next asking
pane in deterministic window order, then depth-first leaf order, wrapping at
the end. The first jump saves the pane you came from; further cycling does
not overwrite it. `C-a Q` (`return-from-attention`) returns there once and
consumes the saved origin. If no pane is asking, no origin was saved, or the
origin closed, the action bells without moving focus. Both actions are
client-local: they send no frame and write no layout metadata or shared focus.
`C-a A` remains the full agent-fleet dashboard (§5.6).

**Clearing rule.** Attention clears when the client forwards key or
paste input to the asking pane — i.e. you focused it and typed
(presumably answering). Merely focusing or clicking the pane does
*not* clear it: looking at a question is not answering it. A repeated
`Asked` for a still-flagged pane changes nothing; the flag re-raises
on the next `Asked` after input cleared it. The flag is client-local
and per-attach — it does not persist across detach/reattach (a
re-emitted `Asked` from the ADR-0036 detector re-raises it).

**Implementation provenance.** The attention channel tracked as
`phux-oih5.15` is already present: the wire `AgentEvent::Asked`, explicit ask
hook, server detector/state, and TUI asked fold shipped in commits `bdb64f6`,
`2e59992`, `23c7bca`, and `28f0b34`. No replacement wire is introduced here.
The shared/directed-focus proposals tracked as `phux-oih5.10` and
`phux-oih5.17` are superseded by accepted ADR-0049: topology may be shared,
but focus authority and advisory attention navigation remain client-local.

### 8.7 Transient notices

Some lifecycle events deserve a moment of visibility but no persistent
chrome. The bar has a single **transient notice slot** for them: a
full-row message (reverse video; warnings additionally bold) that takes
the bar row over from the widgets for about **7 seconds**, then expires
on the bar's existing 1-second refresh tick and the widget row returns.
The slot is **newest-wins** — a fresh notice replaces the current one
and restarts the clock; nothing queues.

Current producers:

- **Input-authority handovers** (ADR-0033). When the *focused* pane's
  input lease moves — another client takes or releases the wheel — an
  info notice calls out the transition (`input: c9 took the wheel`,
  `input: wheel released`). The persistent `WHEEL:*` badge (same
  client-id spelling) keeps showing the steady state; the notice marks
  the moment it changed. The lease state the server re-states at attach
  time is not a transition and raises no notice, and neither do
  handovers on unfocused panes.
- **Degraded federation.** When a hub announces that a satellite became
  unreachable (a spontaneous, uncorrelated `ERROR
  { SATELLITE_UNREACHABLE }`), a warn notice reports it (`federation
  degraded: ...`). This is the in-TUI view of the same state `phux
  status` reports on the CLI.
- **Pane death** (phux-i0e8.2.2). When a pane's process dies and other
  panes survive the layout fold, a warn notice names the dead pane and
  its exit shape: `pane 3: exited 137`, or `pane 3: killed (signal or
  unknown)` when `TERMINAL_CLOSED` carried no exit code (a signal kill).
  Two deaths are deliberately silent: a clean **exit 0** (the user typed
  `exit`; nothing is wrong) and a close **this client itself requested**
  via `kill-pane` / `kill-window` (the kill dispatch marks its targets
  as expected, and the matching close consumes the marker — so a later
  spontaneous death of a reused pane id still notifies).

When the **last** pane dies there is no bar left to notice on: the
client's consumer-owned detach policy (phux-4r1) tears the TUI down.
Since phux-i0e8.2.2 that exit is *explained*: after the alt screen is
gone and the terminal is cooked again, the client prints one line to
stderr — `phux: session ended: the last pane exited 137` (or
`... killed (signal or unknown)`) — so an OOM-killed shell no longer
looks like a phux crash. A clean detach prints nothing, and the process
exit code stays `0` in both cases: the attach succeeded; the ending just
gets words. (Internally the `run_*` attach entry points return an
`AttachEnd` — `Detached` vs `LastPaneClosed { exit_status }` — that the
CLI callers format.)

Precedence and degradation:

- The persistent **error line** (a `[status]` config that failed to
  load, §4.2.1) always outranks the slot: while it holds the row, a
  notice is refused and degrades to a log line.
- **No bar row, no notice.** An empty `[status]` config reserves no
  row, so notices degrade to log lines (`tracing`) instead of painting.
  This is a documented limitation: configure at least one widget to see
  transient notices.
- Notices are client-local and never persist: nothing crosses the wire,
  and a detach/reattach clears the slot.

---

## 9. Hooks

<!-- impl-status: partial; probe: after-new-pane -->
> **Status:** Partially shipped (phux-r82.1). Config parsing for
> `[[hooks.<name>]]` entries ships in `phux-config` (see `schema.rs`),
> and the server-side dispatcher (`phux-server::hooks`) fires a starter
> set of real events: `after-new-pane`, `pane-exit`, `focus-changed`,
> `client-attached`, `client-detached`, and `agent-state-changed`. Enabled plugin manifests'
> `[[events]]` entries whose `on` names one of these events fire through
> the same dispatcher. The remaining hook points in the table below
> (`after-new-session`, `after-new-window`, `after-kill-pane`,
> `output-silenced`, `output-active`) stay design intent — the server
> does not observe those edges yet.

Hooks fire at named events. Each hook in the config is an
array-of-tables (TOML `[[hooks.<name>]]`) of `{ when, action }` pairs.

```toml
[[hooks.after-new-pane]]
when   = { session-startswith = "work" }
action = { kind = "run", command = "echo pane up >> ~/.cache/phux/hooks.log" }

[[hooks.pane-exit]]
when   = { exit-code = 0 }
action = "noop"

[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "say 'pane exited'" }
```

The hook system is intentionally small:

- **Match clauses** (`when = { key = value }`) are exact-string or
  simple glob matches (`"*"`). No regex; no expression language.
- **First match wins** per hook event. Subsequent entries don't fire.
- **Async by default.** Hook actions fire and the server moves on. Sync
  hooks (where the result blocks the trigger) are reserved for v0.2.

Hook points (initial). The context keys are what `when` clauses can
match (and what the hook child receives as `PHUX_*` variables); keys in
parentheses may be absent on a given firing — `exit-code` for a
signal-killed child, `session` when none applies, `agent-name` for an
anonymous agent, `from` on a first sighting. This table mirrors
`phux_config::vocab::hook_context_keys`, which is itself pinned to the
server's event constructors by an agreement test.

| Hook                  | Fires after / on                         | Context keys                                              |
|-----------------------|------------------------------------------|-----------------------------------------------------------|
| `after-new-session`   | session creation                         | design intent                                             |
| `after-new-window`    | window creation                          | design intent                                             |
| `after-new-pane`      | pane creation, before exec               | (`session`), `terminal-id`                                |
| `after-kill-pane`     | pane removed from layout                 | design intent                                             |
| `pane-exit`           | inner process exit                       | (`exit-code`), `terminal-id`                              |
| `client-attached`     | client attach completed                  | `client-id`, `session`                                    |
| `client-detached`     | client detach (any reason)               | `client-id`, (`session`)                                  |
| `focus-changed`       | any client changes focus                 | `client-id`, `terminal-id`                                |
| `agent-state-changed` | a pane's derived agent state changed     | `agent-kind`, (`agent-name`), (`from`), `terminal-id`, `to` |
| `output-silenced`     | configurable silence threshold elapsed   | design intent                                             |
| `output-active`       | first byte after a silence               | design intent                                             |

`phux config check` validates this whole surface — an unknown event
name (including the design-intent rows, which the server does not fire
yet), a `when` key outside the event's context (the `-startswith`
suffix strips off before the lookup), or an action that can never
execute server-side is reported there and warned about again at server
startup, instead of silently never firing.

Server-side execution semantics (the shipped subset):

- **Child processes only.** There is no in-process plugin host. A `run`
  action's `command` may be a string (executed via `/bin/sh -c`) or an
  argv array (executed directly). `noop` matches and does nothing;
  other action kinds (e.g. `message`) are client-side and the server
  dispatcher skips them (the entry still consumes the event under
  first-match-wins).
- **Event context rides environment variables.** Every hook child gets
  `PHUX_EVENT` plus one `PHUX_*` variable per context key:
  `PHUX_TERMINAL_ID`, `PHUX_SESSION`, `PHUX_EXIT_CODE` (absent for
  signal-killed children), `PHUX_CLIENT_ID`. Every hook child also gets
  `PHUX_SOCKET` — the UDS path the firing server listens on — so a bare
  `phux` invocation inside a hook script targets that server even when
  it runs off the default socket path. Plugin event hooks
  additionally get `PHUX_PLUGIN_ID`, `PHUX_PLUGIN_EVENT_ID`, and
  `PHUX_PLUGIN_ROOT`, and run with the plugin root as their working
  directory.
- **Fire-and-forget, bounded.** Events queue onto the dispatcher through
  a non-blocking bounded channel (a full queue drops the event); at most
  a fixed number of hook children run concurrently, each under a timeout
  with kill-on-drop. A slow or wedged hook never blocks the terminal
  actor hot path.

### 9.1 Agent notifications ride `agent-state-changed`

`agent-state-changed` fires when the ADR-0046 detector's *published* state
for a pane actually changes. Its context adds `agent-kind`, `agent-name`
(omitted when the record is anonymous, so a hook child can tell "unnamed"
from "unset"), `from`, and `to` — exported as `PHUX_AGENT_KIND`,
`PHUX_AGENT_NAME`, `PHUX_FROM`, and `PHUX_TO`.

`from` is **absent** on a first sighting. "We have never seen this pane" is
a different fact from "it was idle", and a notifier that conflates them
announces every agent launch as a transition. A withdrawn record (the agent
exited) arrives as `to = "unknown"`.

This is deliberately the *only* notification surface. phux ships no sound
player and no desktop-notification client:

```toml
# Tell me when an agent stops and wants a human.
[[hooks.agent-state-changed]]
when   = { to = "blocked" }
action = { kind = "run", command = "afplay /System/Library/Sounds/Glass.aiff" }

# ... and when one finishes its turn.
[[hooks.agent-state-changed]]
when   = { to = "idle" }
action = { kind = "run", command = "osascript -e 'display notification \"turn done\" with title \"phux\"'" }
```

A built-in notifier would have to grow a config surface for the player, the
sound, the per-state mapping, and the mute switch — reimplementing, badly,
what `osascript`, `notify-send`, `afplay`, and `tput bel` already do. What
the server owes the operator is the *edge*, delivered once, with enough
context to decide. Remember that hooks are **first-match-wins per event**,
so order the `when` clauses most-specific first.

The hook is a true edge in both directions: the detector's own filter models
its emissions rather than the store, so the drain compares against the
recorded state and fires nothing when a republish lands on the state already
there. A notifier that fires on a non-change is a notifier the operator
turns off.

---

## 10. Recording

Two surfaces ship. `phux rec [TARGET] -o PATH` records one pane headlessly:
it subscribes as a pure `ATTACH_TERMINAL` observer, so it neither attaches the
session nor resizes the pane and is safe against a session someone is using.
`phux --rec PATH` records the session you are attached to by teeing the
client's own composited output, so the artifact carries the chrome — tiled
panes, dividers, status bar, sidebar, overlays, cursor — and not just one
pane's bytes.

The output extension picks the format: `.cast` for an [asciinema] cast, `.gif`
for an animated GIF, `.png`/`.apng` for an animated PNG, and a path with no
extension gets `.gif`. GIF and APNG are encoded in-process — no `agg`, no
`vhs`, no `ffmpeg`
— and `phux rec --from FILE.cast -o out.gif` re-renders an existing cast
offline at a different frame rate or idle limit.

The default asciicast version is **2**, and `--cast-version 3` is opt-in. v3
is *not* backward compatible with v2: the header schema changed and event
times became relative intervals, so a v2-only reader that tolerates a v3
header replays a four-minute recording in a fraction of a second. There is no
consumer that reads v3 but not v2.

[`recording.md`](./recording.md) is the full surface — flags, formats, and the
three fidelity limits worth knowing before you record something long.
[ADR-0060](../../ADR/0060-self-contained-session-recording.md) owns the
reasoning.

**Playing one back.** `phux play FILE.cast [TARGET]` creates a **pane whose
PTY is fed from the recording** and prints its Terminal id. It is not a viewer
for your own shell — `asciinema play` is that, and it needs no server. What
this produces is an ordinary pane: attach it, `phux snapshot` it,
`phux resize` it, watch it from an agent, share it with a second client,
`phux kill` it. TARGET says *where* the pane goes (it is created beside it,
splitting that window, default `.`); TARGET is never written to, and no flag
plays into a pane that already has a shell in it.

The pane is resized to the recording's own grid and to each resize the
recording contains, so lines wrap where they wrapped when it was captured;
`--no-fit` opts out. `--speed`, `--idle-limit`, and `--loop` shape the
timeline, and when the recording ends the pane holds its final frame until it
is killed (`--close` ends it instead). Full surface in
[`recording.md`](./recording.md) §6; the reasoning, including why the
shell-level player stays unbuilt, is in
[ADR-0064](../../ADR/0064-playback-as-a-pane.md).

**Superseding the earlier design.** This section previously specified
`phux capture --record TARGET --out FILE.cast` plus a server-side
`PANE_OUTPUT` tee. Neither was ever built and neither is coming: recording is
consumer-side, the verb is `rec`, and `capture` is retired rather than
aliased. The `phux play` that section sketched now exists, but as the pane
above rather than as the shell-level replayer it described.

[asciinema]: https://asciinema.org/

---

## 11. Things we explicitly do not ship

Repeating from `CONTRIBUTING.md` because the design decisions here lean
on these:

- **No embedded scripting language.** No tmux-style `if-shell`, no
  format-template DSL with conditionals. Templates are interpolation
  only.
- **No tmux-style copy-mode reimplementation.** No second parser for
  selection boundaries, no mouse drag selection, and no custom clipboard
  format path. The client may expose a focused-pane copy-mode projection
  for cursor movement, viewport scrolling, highlighting, and literal
  search over mirrored scrollback, then delegate extraction/formatting to
  libghostty and native clipboard behavior.
- **No multi-row status bar, no widgets, no themes-as-config.** The
  status bar is one row. Themes are color slots, not a styling engine.
- **No embedded plugin runtime in core.** Plugin manifests are declarative
  config today. Future runtime surfaces execute argv commands over the
  same CLI/socket contract instead of embedding a scripting language.
- **No homegrown crypto.** Transport is the right layer; SSH and Unix
  socket perms cover it.

---

## 12. Defaults table

The shipped defaults, in one place:

| Setting                       | Default                                  |
|-------------------------------|------------------------------------------|
| Shell                         | `defaults.shell`; unset → `$SHELL`, fallback `/bin/sh` |
| `TERM` advertised to panes    | `xterm-256color` (phux-7vx/phux-0o8; set `defaults.term = "ghostty"` to opt in) |
| History limit per pane        | 50 000 lines                             |
| Backpressure threshold        | 32 unacked frames                        |
| Journal size cap (per pane)   | 10 MiB ring                              |
| Prefix key                    | `C-a`                                    |
| Which-key popup               | on, 600 ms hesitation delay              |
| Pane on PTY exit              | close                                    |
| Mouse                         | on (`defaults.mouse`; `false` = pass-through only, §7) |
| New-pane CWD inheritance      | `inherit-focused` (tmux-shaped)          |
| Spawn-on-attach               | `defaults.shell` (unset = inherit)       |
| Session name template         | `"default"` (supports `${cwd-basename}`) |
| Window-size policy            | `smallest` (shared Terminal geometry, ADR-0027) |
| Status bar                    | `[{ kind = "windows" }]` / `[{ kind = "help-hints" }]` / `["session-name", { kind = "time", format = " %H:%M" }]` |
| Status bar position           | `bottom` (`[status] position`, or `top`) |
| Activity / silence thresholds | activity off; silence 2 min when enabled |
| Resize on attach              | aggregate min bounding box per session   |
| Cursor blink                  | follow inner program request             |

---

## 13. First-time use

A new user, fresh install, no config file:

```sh
$ phux
# spawns server, creates session "default" with one window/one pane
# running $SHELL in $PWD
# attaches the client and renders
# status bar shows "0:shell | C-a ? help | C-a : palette | C-a [ copy | default 21:14"
$ C-a c           # new window
$ C-a d           # detach
$ phux            # re-attach to "default"; full state replayed
```

Discoverability: the default status bar keeps the highest-value prefix
affordances visible without consuming pane space. If the prefix is
rebound, the `help-hints` widget renders the configured prefix.

Beyond that, two client-rendered overlays teach the bindings themselves
(the TUI owns its chrome — nothing here is server-rendered):

- `C-a ?` opens the **help modal**, a centered reference listing every
  prefix-table and global binding, followed by three built-in sections
  covering the driver-owned interactions no keybinding table describes:
  **Copy mode** (the in-overlay selection keys — arrows, `Tab` mode
  cycle, the one-shot grab keys, mouse drag), **Mouse & menus** (the
  right-click context menus and their in-menu navigation, divider
  drag-to-resize, and the sidebar's click targets, labelled with the
  same `+ new` / `= menu` affordance strings the sidebar paints), and
  **Panels & dashboards**, which advertises the window sidebar (off by
  default) and the agent-fleet dashboard with their `toggle-sidebar` /
  `agent-fleet` chords resolved from the active config — a rebind shows
  the rebound chord, an unbound action says `unbound`. The built-in
  rows are sourced from tables colocated with the implementing modules
  and held in lockstep by per-module tests, so the modal cannot
  advertise a gesture that has no handler. A table taller than the
  modal scrolls —
  arrows / `j` / `k` / `C-n` / `C-p` step a row, `PageUp` / `PageDown` a
  screenful, `Home` / `End` jump to the ends, the wheel scrolls a detent,
  and an overflowing table paints a scrollbar in the right border column
  (the window is counted in wrapped display rows, so long action labels
  that fold onto a second row are budgeted for). Esc (or `?` again)
  dismisses it.
- Press `C-a` and *hesitate*, and the **which-key popup** appears after
  `which-key-delay-ms` (default 600 ms), listing the available prefix
  continuations. Any key dismisses it and executes normally; Esc cancels
  the prefix. See §5.7.

---

## 14. Out of scope, but on the radar

These are not in v0.1 but the design accommodates them so they don't
require breaking changes:

- **Resilient remote transport** (zmosh-style UDP/SSP). Hooks into the
  `Transport` abstraction in the wire spec (see
  [`../spec/proto.md`](../spec/proto.md) §4).
- **Native GUI client** (libghostty surface). Talks the same protocol
  as the TUI client — the client's `libghostty_vt::Terminal` already
  parses `PANE_OUTPUT` bytes locally (ADR-0013); a GUI client swaps
  the TUI's `RenderState`-to-VT renderer for a `RenderState`-to-GPU
  renderer and reuses everything else.
- **Multi-user shared sessions.** Today's protocol already supports
  multiple clients per session; ACL and identity will be a future
  authenticated transport addition.
- **Tabbed layouts** (nested tab containers). The wire spec (see
  [`../spec/L3.md`](../spec/L3.md) §3.2) reserves
  the `TABBED` layout node.
- **Image protocols** (sixel, kitty graphics). Under ADR-0013 these
  ride on the `PANE_OUTPUT` byte stream like any other VT sequence;
  per-client gating happens in the server's capability rewriter
  (see [`../spec/proto.md`](../spec/proto.md) §6.2). The `Sixel` / `KittyGraphics` / `Iterm2` capability
  bits already exist; the work is in the rewriter, not the wire
  format.
- **tmux control mode (CC) frontend.** Optional adapter that would let
  a CC-aware terminal (iTerm2 today; Ghostty when 1.4+ binds its
  parser to the GUI) render phux Terminals as native splits of that
  terminal. The native byte-stream protocol (ADR-0013) stays primary
  and strictly more capable; CC is one possible alternative consumer,
  not a roadmap commitment. Per
  [ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md) the
  reference TUI has no protocol-level privilege, so a CC adapter
  picks its tier set (typically L1+L3) the same way the native TUI
  does. The earlier `CC_FRONTEND` capability bit in the wire spec
  (see [`../spec/proto.md`](../spec/proto.md) §6.2)
  is **reclaimed** under ADR-0017; no capability bit is needed.
