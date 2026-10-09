---
audience: contributors
stability: evolving
last-reviewed: 2026-10-09
---

# 0158 — Pane clipboard writes are a client-side, policy-gated effect

**TL;DR.** An OSC 52 clipboard write from a program in a pane is honoured by
the attached client that is showing that pane, using its own libghostty
replica's `on_clipboard_write` hook. It never touches the wire. The write
fires only from live output, never from bootstrap or replay. A per-client
`clipboard.write` policy (`allow`, `ask`, `deny`) gates it. Clipboard reads
stay unsupported.

Status: Proposed
Date: 2026-10-09

## Context

Programs set the clipboard with OSC 52 (`ESC ] 52 ; c ; <base64> BEL`). Vim's
`"+y` over SSH, tmux `set-clipboard`, and `pbcopy` shims all depend on it.
Ghostty honours it with `clipboard-write = allow` and `clipboard-read = ask`.
phux drops it everywhere:

- the server's engine installs no clipboard hook;
- `TERMINAL_EVENT`, which would have carried a clipboard fact, is retired
  ([L1 §3.3](../spec/L1.md));
- client replicas install `on_bell` and nothing else;
- `docs/consumers/cockpit.md` calls a remote pane writing the local clipboard
  a trust boundary that needs an ADR.

A program in a pane therefore cannot copy, even though copying *from* a pane
is client-local copy-mode ([ADR-0045](./0045-client-side-copy-mode.md)).

The bytes already reach every client. `RESOURCE_OUTPUT` forwards the pane's
VT stream ([ADR-0013](./0013-libghostty-bytes-on-wire.md)), and each client
runs a libghostty replica over it. libghostty-vt exposes
`Terminal::on_clipboard_write`, which phux does not register. The bell already
takes this shape: the replica's `on_bell` sets a pending flag, and the client
consumes it.

## Decision

1. **The client acts, from its own replica.** Each client registers
   `on_clipboard_write` on its replicas and treats a write like a bell: a
   pending effect the client consumes. No frame, event, or `PROTOCOL_VERSION`
   change is needed. The server keeps ignoring OSC 52.
2. **Live output only.** A write is honoured only from `RESOURCE_OUTPUT`
   applied after the replica's bootstrap completes. Snapshot decode, history
   pulls, synthesized-VT bootstrap, and reconnect replay never fire it, so
   re-attaching never replays an old copy.
3. **One client writes: the one focused on that pane.** Writes from other
   panes are dropped. A session mirrored to a phone and a laptop changes the
   clipboard only on the device the user is looking at.
4. **Policy, per client: `clipboard.write = "allow" | "ask" | "deny"`.**
   - The default is `allow`, matching Ghostty and tmux `set-clipboard`.
   - `ask` shows the TUI's confirm modal (the same pattern as paste
     protection) or the GUI equivalent, and names the source pane and host.
   - `deny` drops the write and logs one line, without the payload
     ([ADR-0028](./0028-runtime-log-control.md)).
5. **Bounded.** A decoded payload over 1 MiB is dropped. Targets `c`, `p`, and
   `s` all mean the system clipboard. A host without a primary selection has
   no other choice, and Ghostty maps them the same way where it has one.
6. **Delivery is the client's.**
   - The TUI re-emits OSC 52 to its outer terminal, the path copy-mode already
     uses. The outer terminal's own clipboard permission still applies.
   - Cockpit, desktop, and web write the platform pasteboard.
   - FFI hosts (mobile) receive a callback and decide.
7. **Reads stay unsupported.** libghostty-vt ignores `52;?`, and answering a
   read would hand clipboard contents to a remote program. A future read
   policy needs its own ADR.

## Why

Acting in the client follows [ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md):
the replica already parses these bytes. A server-side detector plus a new
event would duplicate that parse and add wire surface for something only a
client can act on, since only the client has a clipboard. It would also have
to choose a recipient, which the client already knows: whoever is focused.

The trust question is the one Ghostty and tmux already answer. The user chose
to run the program, OSC 52 can only overwrite the clipboard and never read
it, and `ask` or `deny` cover anyone who disagrees. A remote host already
receives everything the user types, so letting it place text on the clipboard
is a smaller grant than the one SSH makes. Gating on focus and live output
removes the two surprises that are specific to phux: replay and mirroring.

## Tradeoffs

- With `allow`, a malicious program on a remote host can replace the
  clipboard, and the user may then paste it somewhere. The TUI's paste
  protection catches multi-line pastes into an unbracketed pane, but not a
  one-line swap. `ask` is the mitigation, and it is not the default.
- A background pane, such as an agent running `pbcopy`, cannot set the
  clipboard until it is focused. Its write is dropped, not queued.
- Each client implements delivery. Policy parsing can be shared through
  `phux-config`.
- The TUI relies on its outer terminal supporting OSC 52. Where it does not,
  the write is silently lost, as copy-mode's copy already is.

## Alternatives

**Server-side detection plus an `EVENT`.** The server would register the hook
and journal a `clipboard_write` event. This was rejected: it duplicates the
client's parse, puts clipboard contents in a resumable journal, and still
needs the focus rule on the client.

**Every attached client writes.** This is simpler, but one copy in a session
viewed on three devices would overwrite three clipboards.

**Default `ask`.** This is safer against a hostile remote, but it is the
opposite of Ghostty and tmux and would interrupt ordinary `"+y` use. It
remains the documented hardening choice.

**Keep dropping it.** This keeps the current boundary, but leaves the most
common remote-copy path broken while phux otherwise follows Ghostty.
