---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-09-12
---

# Protocol 101: a complete session walkthrough

**TL;DR.** One terminal session from HELLO to detach: what each step does,
the wire frames it sends, and why the design sits where it does. Read it
before the catalogs. Wire bodies here are illustrative shapes, not byte
layouts; the normative encoding lives in the specs each step links.

---

## The big picture

A client connects, negotiates, attaches to a terminal (or creates one),
receives VT bytes as the PTY emits them, sends structured input back, and
detaches. The wire is asymmetric: the server sends opaque terminal **bytes**;
the client sends **structured** events. Both ends run libghostty, so neither
re-encodes terminal state into a second model. The product picture is
[CONCEPTS.md](../CONCEPTS.md); this page is one session on the wire.

Protocol version for this walkthrough is `0.9.0`. HELLO admits `major.minor`
`0.9`; a `0.8` or `0.7` peer is rejected before session state.

The spine is one terminal. Another resource kind (`AgentSession`) uses the
same attach path; see the appendix, not the numbered steps.

---

## Step 1: HELLO negotiation

**What happens:** the client connects over a Unix socket (or SSH stdin/stdout),
sends the exact protocol version it speaks, and advertises its capabilities.
Every top-level body field is tagged; `client_caps` itself is the required
positional sub-record defined by [proto.md §6.2](./proto.md).

```
Client sends (frame type 0x01):
  HELLO {
    client_name: "phux-tui",       // field 1
    protocol_major: 0,             // field 2
    protocol_minor: 9,             // field 3
    protocol_patch: 0,             // field 4
    client_caps: {                 // field 5
      color: TrueColor,
      layers: 0x05,                // L1 + L3; L2 remains reserved
      images: 0x00,
      kbd_protocols: 0x03,         // kitty + modifyOtherKeys
      hyperlinks: true,
      output_mode: Raw,            // synthesized-profile preference only
      default_colors: None,
      bootstrap_profiles: 0x0e,    // synth raw/state-sync + native-v2 offer
      native_codecs: 1 << 2,       // exact LibghosttyCheckpointV2
      native_features: 0x0000000f, // all four required native features
      max_chunk_bytes: 262144,
      max_history_page_bytes: 1048576,
    }
  }

Server replies (frame type 0x80):
  HELLO_OK {
    protocol_major: 0,             // field 1
    protocol_minor: 9,             // field 2
    protocol_patch: 0,             // field 3
    server_caps: {                 // field 4
      layers: 0x05,
      features: ...,               // includes RESOURCE_KINDS; see proto.md §6.2
    },
    server_id: "phux-server-abc123", // field 5, opaque incarnation bytes
    selected_profile: NativeState { // field 6; current native tag is 3
      codec: LibghosttyCheckpointV2,
      features: 0x0000000f,
    },
    max_chunk_bytes: 262144,        // field 7, negotiated minimum
    max_history_page_bytes: 1048576, // field 8, negotiated minimum
  }
```

**Wire shape:** see [proto.md §6.1](./proto.md). Key pieces:

- `major.minor` must equal `0.9`; any other minor is rejected before session
  state. Patch differences are compatible, and the server returns its current
  patch.
- `layers` is intersected once for the connection. `0x01` is L1 only and
  `0x05` is L1+L3; L2's `0x02` bit remains reserved and unmounted (see
  [L2.md](./L2.md)).
- `BootstrapCapabilities::new()` offers only the two synthesized compatibility
  profiles. Native is explicit opt-in after a successful engine probe: both
  peers must share the exact checkpoint-v2 codec and all four required features
  (`CONTINUATION`, `READY_BOUNDARY`, `HISTORY_PAGES`, and
  `BOUNDED_HISTORY_CONTROL`). The current native offer bit is `0x08`; the
  incomplete legacy `0x01` offer is permanently retired.
- The server selects one advertised profile and the per-axis minimum of the
  nonzero byte bounds. If native is unusable it may select only a synthesized
  profile advertised by both peers; with no shared profile it sends
  `CODEC_UNAVAILABLE`. There is no fallback after `HELLO_OK`.
- Color/image/keyboard/hyperlink rewriting applies only to synthesized
  compatibility profiles. Native checkpoint, history, cursor, and live PTY
  bytes remain opaque and byte-identical.

**Why it matters:** negotiation happens once and fixes the contract for the
whole connection — version, capabilities, and which tiers the two sides will
use.

---

## Step 2: Attach to a terminal

<!-- impl-status: spec-only; probe: RolePolicy -->
> **Status: spec-only —** `role_policy` on `ATTACH` / `ATTACH_RESOURCE`. It is
> not encoded today and the reference server keeps no role state, so a client
> that omits it gets the same unconstrained subscription as one that would
> ask for `PRIMARY`. Do not send it. [L1.md §8.1](./L1.md) has the contract
> roles satisfy when they land. The frame below is the shape that is on the
> wire.

**What happens:** after HELLO, the client picks a terminal to watch. It can
attach to an existing terminal or create one.

```
Client sends (frame type 0x02):
  ATTACH {
    attach_id: 23,                 // field 5, client-chosen, echoed
    target: CREATE_IF_MISSING {    // field 1
      name: "scratch",             // L3 name key, resolved client-side
      command: None,               // use the server's default shell
      cwd: None,                   // use the server's default cwd
    },
    viewport: { cols: 120, rows: 40 },  // field 2
    request_scrollback: false,     // field 3
    scrollback_limit_lines: 0,     // field 4
  }

Server replies (frame type 0x81):
  ATTACHED {
    attach_id: 23,
    snapshot: SubstrateSnapshot { terminals: [...], collections: [], metadata_keys: [] },
    initial_client_id: ClientId(7),
  }
```

**Wire shape:** [L1.md §8](./L1.md) defines `ATTACH` and its `AttachTarget`
union. `attach_id = 23` is client-chosen and echoed by both `ATTACHED` and
`ATTACH_READY`, preventing concurrent attach replies from crossing. The target
is a tagged union — `BY_TERMINAL_ID` to attach to one running terminal,
`CREATE_IF_MISSING` to spawn one if absent, and others. `viewport` carries the
client's drawable size so the server can size the terminal. `ATTACHED` is
metadata only: it carries a `SubstrateSnapshot` of the tier-visible state and
the client's `initial_client_id`. It carries no terminal content yet — that
arrives next.

**Why it matters:** this is where the client says "I want to see and control
this terminal." The server then allocates a subscription and begins the replay
sequence.

---

## Step 3: Receive the negotiated bootstrap

**What happens:** for each terminal, the server sends the HELLO_OK-selected
profile as a generation-scoped opaque stream:

```
Server sends BOOTSTRAP_BEGIN (0x93):
  { terminal_id: LOCAL(42), stream_id: 9, bootstrap_id: 4,
    codec: Native(LibghosttyCheckpointV2), cols: 120, rows: 40,
    output_mode: Raw, base_seq: 1 }
Server sends BOOTSTRAP_CHUNK (0x94):
  { terminal_id: LOCAL(42), stream_id: 9, bootstrap_id: 4,
    chunk_seq: 0, payload: opaque_bytes }
Server sends BOOTSTRAP_READY (0x95):
  { terminal_id: LOCAL(42), stream_id: 9, bootstrap_id: 4,
    history_cursor: optional_opaque_bytes }
Server sends ATTACH_READY (0x83):
  { attach_id: 23 }
```

**Wire shape:** [L1.md §4](./L1.md) defines the exact fields. Native bytes are
an exact libghostty checkpoint and are never parsed or rewritten by phux.
Chunks may split engine records. The client decodes into staging and publishes
atomically at matching READY. History, when requested, is pulled in bounded
pages afterward and never blocks live output or ATTACH_READY.

**Why it matters:** READY gives the client authenticated active state before
history without pausing the PTY or adding a bootstrap-ACK round trip.

---

## Step 4: Stream terminal output

**What happens:** output continues in the same stream/generation, beginning
exactly one sequence after BEGIN's inclusive cut:

```
Server sends RESOURCE_OUTPUT (0x90):
  {
    terminal_id: ResourceId::LOCAL(42),
    stream_id: StreamId(9),
    bootstrap_id: BootstrapId(4),
    seq: 2,
    bytes: b"ls\r\n"
  }

A moment later:
  RESOURCE_OUTPUT {
    terminal_id: ResourceId::LOCAL(42),
    stream_id: StreamId(9),
    bootstrap_id: BootstrapId(4),
    seq: 3,
    bytes: b"Documents\r\nDownloads\r\n..."
  }
```

**Wire shape:** NativeState and SynthesizedVtRaw carry no live ACK.
SynthesizedVtStateSync alone may send cumulative `FRAME_ACK` with the same
terminal, stream, and bootstrap ids after applying the transition. A sequence
gap or stale generation is repaired by `BOOTSTRAP_TOMBSTONE` plus a fresh cut,
never by guessing a base.

**Why it matters:** raw native output remains byte-identical, and explicit
generation/watermark identity prevents stale bytes from corrupting a replica.

---

## Step 5: Handle client input

**What happens:** the client sends a keystroke as a structured event. The
server hands it to its libghostty encoder, which produces terminal-mode-aware
VT bytes, and writes them to the PTY.

```
User presses Ctrl+C.

Client sends (frame type 0x10):
  INPUT_KEY {
    terminal_id: ResourceId::LOCAL(42),
    event: {
      action: PRESS,
      key: KEY_C,
      mods: { ctrl: true },
      text: None,                  // C0 controls are derived by the encoder
    }
  }

The server looks up ResourceId(42), refreshes its key encoder against
that terminal's current modes, encodes, and writes the bytes to the PTY.
The process receives SIGINT or the byte, depending on terminal mode.

A moment later, the process exits and the prompt returns:

Server sends (frame type 0x90):
  RESOURCE_OUTPUT {
    terminal_id: ResourceId::LOCAL(42),
    seq: 4,
    bytes: b"^C\r\n$ "
  }
```

**Wire shape:** [input.md](./input.md) defines the input family —
`INPUT_KEY`, `INPUT_MOUSE`, `INPUT_PASTE`, `INPUT_FOCUS`. (`INPUT_RAW` is
reserved and spec-only; do not send it.) Each live frame carries a
`terminal_id` and a structured event. The server's libghostty-backed encoder
converts the event to mode-aware VT bytes and writes to the PTY; encoder
configuration never crosses the wire.

**Why it matters:** the seam is the protocol. The client never produces VT
bytes; the server never sees encoder options. Each side owns one half.

---

## Step 6: Terminal-originated signals

<!-- impl-status: spec-only; probe: TYPE_TERMINAL_EVENT -->
> **Status: spec-only —** `TERMINAL_EVENT` (`0xB1`). The live byte stream
> already carries the same OSC sequences inside `RESOURCE_OUTPUT`, so a
> consumer reads title and cwd from its own engine. `BELL` (`0xB0`) below
> is shipped.

**What happens:** a BEL in the PTY is forwarded as a structured frame. OSC
title and cwd sequences travel in `RESOURCE_OUTPUT` today; a later
`TERMINAL_EVENT` frame would surface them as fields.

```
Process rings the bell (BEL):

Server sends (frame type 0xB0):
  BELL {
    terminal_id: ResourceId::LOCAL(42),
  }
```

**Wire shape:** [L1.md §3.2](./L1.md) defines `BELL`. [L1.md §3.3](./L1.md)
defines the spec-only `TERMINAL_EVENT` union.

**Why it matters:** `BELL` is a side-channel the byte stream does not
preserve as a frame of its own. Structured OSC events, when they land,
decouple a consumer from parsing those sequences itself.

---

## Step 7: Detach

**What happens:** the user quits or switches clients. The client sends
`DETACH`; the server acknowledges and closes the transport.

```
Client sends (frame type 0x03):
  DETACH { }

Server replies (frame type 0x82):
  DETACHED {
    reason: REQUESTED,
    message: "detach acknowledged"
  }

The server closes the transport (UDS or SSH pipe).
The terminal keeps running on the server.
Another client can attach to it later.
```

**Wire shape:** [proto.md §7.2](./proto.md). `reason` is an enum:
`REQUESTED` (clean client detach), `SERVER_SHUTDOWN`, `SESSION_KILLED` (a
legacy name retained for wire compat), `REPLACED` (another client
deliberately took over an exclusive attach; spec-only until roles land),
`PROTOCOL_ERROR`, and `INTERNAL_ERROR`.

**Why it matters:** detach is clean and does not kill the terminal. The next
client receives a fresh profile-selected bootstrap, then may pull retained
history incrementally.

---

## Putting it together

The complete sequence as a timeline:

```
Client                              Server                    Terminal (PTY)
  |                                   |                           |
  |------- HELLO ------>              |                           |
  |                   <------- HELLO_OK                           |
  |                                   |                           |
  |------- ATTACH ------>             |                           |
  |                   <------ ATTACHED |                           |
  |              <----- BOOTSTRAP_BEGIN (base_seq 1)              |
  |              <----- BOOTSTRAP_CHUNK (opaque checkpoint)       |
  |              <----- BOOTSTRAP_READY                            |
  |              <----- ATTACH_READY                               |
  |              <----- RESOURCE_OUTPUT (seq 2) -------- shell prompt
  |              user types "ls\n" ----->                         |
  |------- INPUT_KEY ------>          |------- write VT bytes --->
  |                                   |                      <---- echo "ls"
  |              <----- RESOURCE_OUTPUT (seq 3) -------- ls output
  |                                   |                           |
  |              <----- RESOURCE_OUTPUT (seq 4) -------- prompt   |
  |                                   |                           |
  |------- DETACH ------>             |                           |
  |                   <------ DETACHED |                           |
  |                                   | (terminal stays alive)    |
  X                                   |                           |
```

After detach, the terminal keeps running. Its PTY is still open. Another
client can attach and continue.

---

## Next steps

This walkthrough covers the happy path. For details:

- **Version negotiation and capabilities:** [proto.md §6](./proto.md)
- **Full frame catalog and encoding:** [proto.md §7](./proto.md)
- **Terminal state, snapshots, and flow control:** [L1.md](./L1.md)
- **All input event types:** [input.md](./input.md)
- **Metadata, session names, and grouping conventions:** [L3.md](./L3.md)
- **Encoding primitives (varints, strings, tagged unions):** [appendix-encoding.md](./appendix-encoding.md)

For the conceptual picture, read [CONCEPTS.md](../CONCEPTS.md).
[ADR-0013](../adr/0013-libghostty-bytes-on-wire.md) is the bytes-on-wire
decision this walkthrough assumes.

A second resource kind — a producer-fed event log bound to a terminal —
uses the same attach path. The appendix sketches it; [L1.md §1.1](./L1.md)
is the contract.

---

## Appendix: another kind (`AgentSession`)

<!-- impl-status: shipped; probe: ResourceKind,RESOURCE_KINDS -->
> **Status: shipped.** The reference server advertises `RESOURCE_KINDS` and
> serves `AgentSession` bound to a Terminal parent. [L1.md §1.1, §1.2,
> §4.8, and §5.5](./L1.md) carry the contract.

An agent harness running inside a terminal can ask for a second resource of
a different kind, bound to that terminal as its parent, and append JSON-line
records to it. A third party reads them by attaching to the new resource
exactly as it would attach to a terminal.

Gate on the feature bit first: `HELLO_OK.server_caps.features` must contain
`RESOURCE_KINDS`; an older server skips the new spawn fields and spawns a
plain terminal.

```
Client sends (frame type 0x22):
  SPAWN_RESOURCE {
    request_id: 7,                 // field 1
    group: GroupId(1),             // field 2; must equal the parent's Group
    kind: AGENT_SESSION,           // field 11
    parent: ResourceId::LOCAL(42), // field 12
    provider: "claude",            // field 13
    native_id: "c0ffee-…",         // field 14
  }                                // fields 3–6, 8, 10 are absent: no process, no window, no grid

Server replies (frame type 0xA2):
  RESOURCE_SPAWNED { request_id: 7, result: OK(ResourceId::LOCAL(43)) }
```

Resource 43 is not a pane. Append records with `APPEND_RESOURCE_OUTPUT`
(command tag `0x1a`); attach with `ATTACH_RESOURCE`. Bootstrap uses
`AgentEventsJsonlV1`, zero geometry, and no `FRAME_ACK`. Closing the parent
closes the child in the same lock; the child never closes the parent.

`phux agent session open|close`, `phux agent emit`, and `phux agent log`
are the reference CLI for this kind. `%name` resolves an AgentSession. The
pane detector (`phux agent show` / `explain`, OSC title) is a different
surface.
