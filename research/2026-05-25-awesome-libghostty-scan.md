---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-05-28
---

# Competitive scan: awesome-libghostty (2026-05-25)

**TL;DR.** Survey of libghostty-adjacent projects on the
awesome-libghostty list as of 2026-05-25. Identifies what phux should
study, what's safely orthogonal, and what (if anything) invalidates
the current ADRs. Research artifact, not a decision; follow-ups are
filed as bd issues under [phux-a97](../docs/adr/README.md).

**Source**: <https://github.com/Uzaaft/awesome-libghostty> as of 2026-05-25.
**Scope**: Identify projects on the awesome-libghostty list that overlap phux's
design bets, decide what to steal, and confirm whether anything invalidates the
SPEC or ADRs.

## TL;DR

- **Closest neighbor**: [`psyclyx/vanish`](https://github.com/psyclyx/vanish) — same substrate
  (libghostty-vt), same problem class (multiplexer + persistence + multi-client).
  Picks raw VT bytes for its native wire and is single-session-per-daemon, so
  phux's distinctive bets ([ADR-0002](../docs/adr/0002-diff-based-protocol.md) cell
  diffs on every transport, [ADR-0003](../docs/adr/0003-server-process-model.md)
  forest-per-daemon, [ADR-0006](../docs/adr/0006-input-mirrors-libghostty.md)
  structured input) remain differentiated.
- **Threats to spec or ADRs**: none.
- **Steal candidates**:
  1. vanish's primary/viewer role model with takeover — a small `RolePolicy`
     addition to `ATTACH`/`COMMAND` would buy real agent-watching UX cheaply.
  2. Mux's product vocabulary (workspaces, git-divergence view, agent status
     sidebar, mode prompts, opportunistic compaction) — concrete hints for the
     `AGENT_HOOKS` `ServerFeature` bit.
  3. `coder/ghostty-web` as a *renderer* (not a parser) for a future browser
     phux-client that consumes `PANE_DIFF` streams.

## Method

Pulled the README from `Uzaaft/awesome-libghostty` and triaged ~70 entries.
Six were judged adjacent enough to warrant a README read; one (`coder/ghostty-web`)
was read because it's load-bearing for `coder/mux`. Everything else is a UI
shell, a language binding, or an Apple-platform terminal app and is out of
scope for a *protocol-substrate* comparison.

For each project the scan captured: what it does, the wire/IPC format,
overlap with phux on the four headline bets (cell-diff wire,
satellite/remote, swarm, predictive echo), and whether there's something
to steal or something that invalidates our design.

## Adjacent projects

### 1. `psyclyx/vanish` (Zig) — closest neighbor

- **What**: Terminal session multiplexer on libghostty-vt. Single primary +
  N viewers per session, web access included.
- **Architecture**: Per-session daemon. Two distinct wire formats:
  - Native client ↔ session: binary protocol over Unix socket, type `0x82`
    `Output` payload = "VT-encoded terminal output" — raw VT bytes on the
    wire. Plus a `Full` (`0x83`) full-state snapshot on connect. Resize,
    takeover, kick messages exist. No `FrameAck`, no `Diff` op, no per-pane
    refresh cap.
  - Browser client ↔ HTTP server: server-side VT-to-HTML rendering with
    cell-level delta streaming over SSE. Structurally close to phux's diff
    bet, but only on the web path.
- **Overlap**: Multiplexer-on-libghostty-vt — yes. Per-session daemon (we
  are per-user daemon, see [ADR-0003](../docs/adr/0003-server-process-model.md)).
  Cell-diff wire — only on the web transport, not the primary one.
  Satellite/swarm/predictive-echo — no.
- **Steal**:
  1. `vthtml.zig` (cell-delta-over-SSE) is a working prototype of one
     reduction of the diff protocol (cells → positioned HTML spans). When
     a browser client lands post-v0.1, study the renderer shape, not the
     wire format.
  2. The primary/viewer role model with takeover is a clean exclusivity
     primitive phux does not currently spec. SPEC §11 `ATTACH` and
     `COMMAND` could grow a `RolePolicy` for the agent-watching-agent
     use case. Filed as a follow-up (see below).
- **Invalidates?** No. Vanish chose VT-bytes for the native path (the path
  [ADR-0002](../docs/adr/0002-diff-based-protocol.md) rejects) and is
  single-session-per-daemon. Different shape, overlapping libraries.

### 2. `neurosnap/zmx` (Zig)

- **What**: Single-session shell persistence. abduco-with-state-replay.
  README is explicit that it does *not* provide windows, tabs, or splits.
- **Architecture**: Per-session daemon (one PTY per daemon), Unix socket
  IPC, libghostty-vt for state replay on reattach. Wire carries VT bytes;
  the daemon ships output bytes to clients and the in-daemon `Terminal`
  handles state replay.
- **Overlap**: Persistence + libghostty-vt — yes. Cell-diff wire — no.
  Satellite/swarm — no. Predictive echo — no.
- **Steal**: Already covered by
  [ADR-0005](../docs/adr/0005-relationship-to-zmx-and-zmosh.md). We've already
  picked up the non-exhaustive tag-enum pattern.
- **Notable absent feature in zmx that phux has**: scrollback survives
  detach because phux ships diffs and snapshots, not VT byte replay.

### 3. `seruman/hauntty` (Go + WASM)

- **What**: Go daemon for session persistence; uses ghostty-vt compiled to
  WASM to track terminal state. The author's README is honest about the
  scope (learning project; recommends tmux / zellij / shpool / zmx
  instead).
- **Architecture**: Go daemon, Unix socket, WASM bridge to libghostty-vt.
  README doesn't spec a wire format in detail; vocabulary ("dump session
  screen contents", "restore from saved state") indicates VT-bytes-on-the-
  wire with snapshot-based reattach. No diff vocabulary.
- **Overlap**: Persistence + libghostty-vt — yes. Cell-diff wire — no.
  Satellite/swarm — no. Predictive echo — no.
- **Steal**: One useful piece of intel — libghostty-vt → WASM is a real,
  shipping path. Bookmark for any future non-Rust client.
- **Invalidates?** No.

### 4. `Yukaii/ykmx` (Zig)

- **What**: Experimental multiplexer in Zig with tiling layouts, tabs,
  popups, plugin host. Designed to run *inside* zmx for persistence
  (`zmx attach dev ./zig-out/bin/ykmx`).
- **Architecture**: Single-process TUI multiplexer (tmux-shaped, Zig).
  Plugin protocol is NDJSON over a Bun process. No network wire — runs
  in one TTY. Composes with zmx for persistence rather than implementing
  it itself.
- **Overlap**: Multiplexer scope (windows/tabs/layouts) — yes. Cell-diff
  wire — no, it's a TTY app. Satellite/swarm — no. Predictive echo — no.
- **Steal**: Nothing. The "compose with zmx for persistence" inversion is
  the opposite of phux's "one server owns persistence + multiplexing"
  stance ([ADR-0003](../docs/adr/0003-server-process-model.md)); useful
  datapoint, not a model to copy. Explicitly **don't** copy the NDJSON
  plugin protocol — SPEC §11.1's typed-command stance is the opposite
  direction and should hold.
- **Invalidates?** No.

### 5. `neurosnap/footty` (Zig)

- **What**: Foot's Wayland UI with libghostty's VT rendering. Repo's
  README is foot's upstream README — no footty-specific docs.
- **Architecture**: Wayland terminal emulator. No network protocol. No
  multiplexer.
- **Overlap**: Zero direct overlap with phux. The only shared substrate is
  libghostty-vt itself.
- **Steal**: Nothing for phux's protocol. Useful as another existence
  proof that libghostty-vt can be embedded in non-Ghostty hosts —
  reassuring for [ADR-0004](../docs/adr/0004-libghostty-vt-as-grid.md).
- **Invalidates?** No.

### 6. `coder/mux` — only real swarm competitor

- **What**: Desktop & browser app for parallel agentic development.
  Isolated workspaces (local, git worktrees, SSH), multi-model agent
  runner (Sonnet / Opus / GPT / Grok / Ollama), VS Code extension,
  costs / compaction UI.
- **Architecture**: README is a product page, not a tech doc. From
  context: Electron-style desktop app wrapping its own agent loop
  ("custom agent loop … core UX inspired by Claude Code"). The terminal
  rendering uses `ghostty-web` (WASM libghostty + xterm.js-compatible API
  in browser / Electron). So the "terminal" surface is per-workspace,
  in-process WASM, not a remote multiplexer protocol.
- **Overlap**: Worktree-based parallel agentic dev — direct overlap.
  Remote execution via SSH — overlap with the satellite story in
  [ADR-0007](../docs/adr/0007-mosh-class-transport-and-satellites.md).
  Cell-diff wire across a network — no, they terminate the terminal at
  the renderer in-process. Predictive echo — N/A (no remote terminal
  protocol).
- **Steal**: The product surface vocabulary — "isolated workspaces",
  "git divergence view", "agent status sidebar", "opportunistic
  compaction", "mode prompts". These are user-facing concepts that hint
  at command-plane primitives phux's spec should expose; we have
  `AGENT_HOOKS` as a `ServerFeature` bit but no concrete shape yet.
- **Invalidates?** No. Mux solved an agent-orchestration UX while phux is
  solving the terminal-multiplexer-protocol substrate. They could
  literally ship on top of phux. **Worth a positioning ADR** (filed as a
  follow-up) so a future contributor doesn't conflate the two.

### 7. `coder/ghostty-web`

- **What**: libghostty (Ghostty's VT) compiled to WASM, wrapped in an
  xterm.js-compatible API for browsers. Built for `coder/mux`.
- **Architecture**: WASM VT in the browser, roughly 400 KB. App-level
  wire format is whatever you feed `term.write(bytes)` — i.e. raw VT
  bytes, the xterm.js contract. README's example uses a websocket
  carrying raw VT both directions.
- **Overlap**: Uses libghostty-vt — yes. Wire format — raw VT bytes,
  exactly what [ADR-0002](../docs/adr/0002-diff-based-protocol.md) rejects.
- **Steal**: Bookmark for a future browser phux-client — we'd use
  ghostty-web's `Terminal` as a *rendering surface* receiving phux
  `PANE_DIFF` events, not as a VT parser. Same trick vanish does in its
  SSE web client.
- **Invalidates?** No. Confirms the xterm.js-compat ecosystem is locked
  to the raw-VT contract; even Coder's polished entrant didn't try to
  break out — they sit at the renderer layer, not the protocol layer.
  Our rejection of xterm.js wire-compat keeps us out of that gravity
  well.

## Out of scope (and why)

- **Language bindings** (`libghostty-rs`, `go-libghostty`, `libghostty-cpp`,
  `libghostty-dart`, `ghosttpy-vt`, `ghostty_ex`, the various FFI
  wrappers): substrate for phux and its peers, not competitors.
- **Native terminal emulators** (`conterm`, `macterm`, `fantastty`,
  `Dotty`, `hollow`, `tildaz`, `Umbra`, `gostty`, `forgetty`, `Husk`,
  `Ghostling`, `phantty`, etc.): single-process terminal UIs.
  No network protocol; outside the multiplexer/protocol niche.
- **SSH / Mosh clients** (`Echo`, `Spectty`, `Geistty`, `Quay`, `remux`,
  `RootShell`, `VVTerm`, `NeoShell`, `Sshotty`): mobile/desktop SSH UIs
  that happen to render with libghostty. Mosh-compat is explicitly
  out of scope per [ADR-0007](../docs/adr/0007-mosh-class-transport-and-satellites.md).
- **Editor embeds** (`emacs-libgterm`, `Ghostel`, `obsidian-ghostty-terminal`,
  `onyx-shell`, `vscode-bootty`, `shade`, `jupyterlab-ghostty-terminal`):
  terminal-in-editor; orthogonal.
- **Agent-orchestration UIs that are not multiplexers** (`Ghostree`, `Forge`,
  `cmux`, `Factory Floor`, `Supacode`, `Mux0`, `taskers`, `Aizen`,
  `paulatty`, `moai-studio`, `agtmux-term`, `AiyuTerm`, `in0`, `Zentty`,
  `TheCommander`, `moss`, `blink`, `limpid`, `con-terminal`, `frep`,
  `YEN`, `witty`, `Mori`, `Muxy`): Mux's category. They are competitors
  to a future phux *product*, not to the phux *protocol*. They all
  appear to terminate terminals in-process (most via ghostty-web or
  native libghostty).
- **Web/embed renderers** (`Restty`, `browstty`, `electron-libghostty`,
  `RemoteTTYs`, `webterm`): orthogonal — rendering layer, not protocol.
- **Utilities** (`evp`, `termscope`, `headless-terminal`, `term2html`,
  `reed`, `Trolley`, `vterm-mcp`): single-purpose tools.

## Follow-ups

Filed (or to file) as bd issues:

1. **`RolePolicy` on `ATTACH` / `COMMAND`** — spec a primary/viewer
   exclusivity model with takeover semantics, modeled on vanish. Targets
   the agent-watching-agent use case. SPEC §11 surface.
2. **Positioning ADR (phux vs Mux)** — short ADR explaining that phux is
   a protocol substrate, not an orchestration app, and that products like
   Mux could ship on top of it. Prevents future scope confusion when the
   swarm story matures.
