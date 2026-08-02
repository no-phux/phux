---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-06-06
---

# Protocol 101: a complete session walkthrough

**TL;DR.** One phux session traced end to end, from HELLO to detach: what each step does, the wire frames it sends, and why the design lands where it does. Read it before the reference specs; it is the narrative spine the per-tier docs assume you have already seen. Wire bodies here are illustrative shapes, not byte layouts; the normative encoding lives in the specs each step links.

---

## The big picture

A phux session, in one breath: a client connects to a server, negotiates capabilities, attaches to a terminal (or creates one), receives a stream of VT bytes as the PTY emits them, sends keypresses and mouse events back, and eventually detaches. The flow is asymmetric on purpose. The server sends opaque terminal **bytes**; the client sends **structured** input events. Both ends run the same terminal engine (libghostty), so neither side re-encodes terminal state into a second model — the bytes go straight onto the wire and are parsed once on each end.

Protocol version for this walkthrough is `0.3.0`.

---

## Step 1: HELLO negotiation

**What happens:** the client connects over a Unix socket (or SSH stdin/stdout), declares the versions it speaks, and advertises its capabilities.

```
Client sends (frame type 0x01):
  HELLO {
    versions: [{ min: 0.3.0, max: 0.3.0 }],
    client_caps: {
      layers: 0x01,              // L1 only
      color: TrueColor,
      kbd_protocols: 0x03,       // kitty + modifyOtherKeys
      mouse_protocols: 0x01,     // standard mouse
      hyperlinks: true,
      output_mode: Raw,          // byte-faithful PTY broadcast
    }
  }

Server replies (frame type 0x80):
  HELLO_OK {
    version: 0.3.0,
    server_caps: {
      layers: 0x05,              // L1 + L3 implemented (no L2 tier)
      features: 0x01,            // REATTACH_REPLAY enabled
      max_message_size: 16777216,
    },
    server_id: "phux-server-abc123"
  }
```

**Wire shape:** see [proto.md §6.1](./proto.md). Key pieces:

- `versions` lists the semantic version ranges the client accepts; the server selects the highest version that lies in some range and that it also supports, then echoes it back.
- `layers` is a bitset: `0x01` is L1 only, `0x05` is L1+L3. The negotiated tier set is the intersection of the two `layers` fields; an agent declares L1, a TUI declares L1+L3. The L2 bit (`0x02`) is reserved but unused — there is no collection tier (see [L2.md](./L2.md)).
- The capability fields (color, keyboard, mouse, hyperlinks) tell the server how to downsample the outbound byte stream. If the client advertises `Indexed256`, the server rewrites truecolor SGR codes to their 256-color equivalents before forwarding.

`HELLO.client_caps` carries a legacy `rendering` field (`Diff` vs. `VtReplay`); it is deprecated and ignored. With `TERMINAL_OUTPUT` carrying VT bytes, every client renders by local libghostty parse, so there is no structured-diff alternative to select. See [proto.md §6.2](./proto.md).

**Why it matters:** negotiation happens once and fixes the contract for the whole connection — version, capabilities, and which tiers the two sides will use.

---

## Step 2: Attach to a terminal

**What happens:** after HELLO, the client picks a terminal to watch. It can attach to an existing terminal or create one, and it declares the role it wants on that terminal.

```
Client sends (frame type 0x02):
  ATTACH {
    target: CREATE_IF_MISSING {
      name: "scratch",           // L3 name key, resolved client-side
      command: None,             // use the server's default shell
      cwd: None,                 // use the server's default cwd
    },
    viewport: { cols: 120, rows: 40 },
    request_scrollback: false,
    scrollback_limit_lines: 0,
    role_policy: {
      requested_role: PRIMARY,
      takeover: NEVER,
    },
  }

Server replies (frame type 0x81):
  ATTACHED {
    snapshot: SubstrateSnapshot { terminals: [...], collections: [], metadata_keys: [] },
    initial_client_id: ClientId(7),
  }
```

**Wire shape:** [L1.md §state replay](./L1.md) defines `ATTACH`, its `AttachTarget` union, and `RolePolicy`. The target is a tagged union — `BY_TERMINAL_ID` to attach to one running terminal, `CREATE_IF_MISSING` to spawn one if absent, and others. `viewport` carries the client's drawable size so the server can size the terminal; `role_policy` chooses `PRIMARY` (input-capable) or `VIEWER` (watch-only). `ATTACHED` is metadata only: it carries a `SubstrateSnapshot` of the tier-visible state and the client's `initial_client_id`. It carries no terminal content yet — that arrives next.

**Why it matters:** this is where the client says "I want to see and control this terminal," and which role it claims. The server then allocates a subscription and begins the replay sequence.

<!-- impl-status: spec-only; probe: RolePolicy -->
> **Status: spec-only —** the `role_policy` block in the frame above. It is
> not encoded on `ATTACH` today and the reference server keeps no role state,
> so a client that omits it gets the same unconstrained subscription as one
> that asks for `PRIMARY`. Send the rest of the frame as shown;
> [L1.md §8.1](./L1.md) has the contract roles will satisfy when they land.

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
Server sends TERMINAL_OUTPUT (0x90):
  {
    terminal_id: TerminalId::LOCAL(42),
    stream_id: StreamId(9),
    bootstrap_id: BootstrapId(4),
    seq: 2,
    bytes: b"ls\r\n"
  }

A moment later:
  TERMINAL_OUTPUT {
    terminal_id: TerminalId::LOCAL(42),
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

**What happens:** the client sends a keystroke as a structured event. The server hands it to its libghostty encoder, which produces terminal-mode-aware VT bytes, and writes them to the PTY.

```
User presses Ctrl+C.

Client sends (frame type 0x10):
  INPUT_KEY {
    terminal_id: TerminalId::LOCAL(42),
    event: {
      action: PRESS,
      key: KEY_C,
      mods: { ctrl: true },
      text: None,                  // C0 controls are derived by the encoder
    }
  }

The server looks up TerminalId(42), refreshes its key encoder against
that terminal's current modes, encodes, and writes the bytes to the PTY.
The process receives SIGINT or the byte, depending on terminal mode.

A moment later, the process exits and the prompt returns:

Server sends (frame type 0x90):
  TERMINAL_OUTPUT {
    terminal_id: TerminalId::LOCAL(42),
    seq: 4,
    bytes: b"^C\r\n$ "
  }
```

**Wire shape:** [input.md](./input.md) defines the input family — `INPUT_KEY`, `INPUT_MOUSE`, `INPUT_PASTE`, `INPUT_FOCUS`, `INPUT_RAW`. Each carries a `terminal_id` and a structured event. The server's libghostty-backed encoder converts the event to mode-aware VT bytes and writes to the PTY; encoder configuration never crosses the wire. Sending input as structured data (rather than VT bytes) is what lets phux transport modifier-rich chords, the kitty keyboard protocol, IME composition, and pixel-precise mouse events end to end.

**Why it matters:** the seam is the protocol. The client never produces VT bytes; the server never sees encoder options. Each side owns one half.

---

## Step 6: Other terminal-originated events

**What happens:** the running process may emit control sequences that the server's engine parses and the server surfaces as structured events, rather than re-emitting raw escapes.

```
Process sets the window title via OSC 0:

Server sends (frame type 0xB1):
  TERMINAL_EVENT {
    terminal_id: TerminalId::LOCAL(42),
    event: TITLE { title: "my-project — vim" }
  }

Process rings the bell (BEL):

Server sends (frame type 0xB0):
  BELL {
    terminal_id: TerminalId::LOCAL(42),
  }

Process reports its working directory via OSC 7:

Server sends (frame type 0xB1):
  TERMINAL_EVENT {
    terminal_id: TerminalId::LOCAL(42),
    event: CURRENT_DIR { uri: "file:///Users/alice/workspace" }
  }
```

**Wire shape:** [L1.md §1.2–1.3](./L1.md) define `BELL` and `TERMINAL_EVENT`. The server's engine parses the OSC sequence once and forwards a structured field, so a consumer reads "the current directory" without parsing escape sequences itself. These frames are `spec-only` today; the live byte stream already carries the same OSC sequences inside `TERMINAL_OUTPUT`, so a consumer can also read title and cwd from its own engine.

**Why it matters:** structured terminal events decouple a consumer from OSC parsing. An agent sees title and cwd as fields.

---

## Step 7: Detach

**What happens:** the user quits or switches clients. The client sends `DETACH`; the server acknowledges and closes the transport.

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

**Wire shape:** [proto.md §7.2](./proto.md). `reason` is an enum: `REQUESTED` (clean client detach), `SERVER_SHUTDOWN`, `SESSION_KILLED` (a legacy name retained for wire compat), `REPLACED` (another client deliberately took over an exclusive attach), `PROTOCOL_ERROR`, and `INTERNAL_ERROR`.

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
  |              <----- TERMINAL_OUTPUT (seq 2) -------- shell prompt
  |              user types "ls\n" ----->                         |
  |------- INPUT_KEY ------>          |------- write VT bytes --->
  |                                   |                      <---- echo "ls"
  |              <----- TERMINAL_OUTPUT (seq 3) -------- ls output
  |                                   |                           |
  |              <----- TERMINAL_OUTPUT (seq 4) -------- prompt   |
  |------- FRAME_ACK ------>          |                           |
  |                                   |                           |
  |------- DETACH ------>             |                           |
  |                   <------ DETACHED |                           |
  |                                   | (terminal stays alive)    |
  X                                   |                           |
```

After detach, the terminal keeps running. Its PTY is still open. Another client can attach and continue.

---

## Next steps

This walkthrough covers the happy path. For details:

- **Version negotiation and capabilities:** [proto.md §6](./proto.md)
- **Full frame catalog and encoding:** [proto.md §7](./proto.md)
- **Terminal state, snapshots, and flow control:** [L1.md](./L1.md)
- **All input event types:** [input.md](./input.md)
- **Metadata, session names, and grouping conventions:** [L3.md](./L3.md)
- **Encoding primitives (varints, strings, tagged unions):** [appendix-encoding.md](./appendix-encoding.md)

For the conceptual picture, read [CONCEPTS.md](../CONCEPTS.md) and the ADRs that shape the design:

- [ADR-0013: libghostty bytes on the wire](../../ADR/0013-libghostty-bytes-on-wire.md)
- [ADR-0016: terminal ID as wire primary](../../ADR/0016-terminal-id-as-wire-primary.md)
- [ADR-0030: engine-delegated wire and projection consumers](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)
</content>
</invoke>
