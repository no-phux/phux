---
audience: contributors
stability: stable
last-reviewed: 2026-09-09
---

# 0103 - Agent session resource and producer-fed streams

**TL;DR.** `AgentSession` is the second resource kind: a provider, an opaque
native id, a derived state, and a Terminal parent. Its output stream is
producer-fed: the harness shim appends `AgentEventsJsonlV1` records through
`APPEND_RESOURCE_OUTPUT`, and the server stamps sequence and time, retains a
bounded ring, replays it as the bootstrap, and derives working, blocked, and
done from record types. Screen scraping and hook reports become fallbacks
below the stream.

Status: Proposed
Date: 2026-09-09

## Context

The server learns what an agent is doing by reading its screen
([ADR-0046](./0046-server-side-agent-state-detection.md)) and, since
[ADR-0085](./0085-hook-sourced-agent-state.md), by accepting one state word
per hook. Both recover from the outside facts the harness states plainly: a
prompt was submitted, a tool started, a permission is pending, the turn ended.
The detector is some 6,500 lines of manifests, fixtures, and hysteresis that
guess those facts back, and a restyled prompt box silently breaks it. Neither
path leaves a log: a late observer gets the current `phux.agent/v1` record
([ADR-0040](./0040-agent-identity-metadata.md)) and nothing before it.

[ADR-0102](./0102-resources-the-server-serves-kinds.md) makes the server serve
kinds with an ordered opaque stream each. An agent session is the obvious
second kind, with one difference: no process writes its stream. Something
has to append.

## Decision

1. **The facet.** `AgentSession` carries `provider: String` (for example
   `claude`), `native_id: Option<String>` (an opaque provider session id under
   [ADR-0068](./0068-native-agent-session-restore.md)'s native-id bounds), and
   a derived `state`. It always names a Terminal parent
   ([ADR-0104](./0104-parent-bindings-are-l1-lifecycle.md)). In
   `SPAWN_RESOURCE` it is `kind = 1` with field 12 `parent`, field 13
   `provider`, and field 14 `native_id`; the decoder requires 12 and 13 and
   forbids the Terminal-only fields 3 to 6 and 10. `SpawnError` gains
   `UnsupportedKind`, `ParentNotFound`, and `ParentKindMismatch`.
2. **The codec is `AgentEventsJsonlV1`**, a new `BootstrapCodec` tag (0 and 2
   are never reused). One UTF-8 JSON object per line, at most 16 KiB:
   `{"seq":u64,"ts_ms":u64,"type":<str>,"data":{...}}`. `seq` and `ts_ms`
   are assigned by the server on append; a producer-supplied value is
   ignored. The v1 `type` set is closed: `session_start`, `prompt`,
   `tool_start`, `tool_end`, `notification`, `ask`, `stop`, `session_end`,
   `state`, `provider_raw`. An unknown `type` is `RECORD_INVALID`.
3. **The stream is producer-fed.** New command `APPEND_RESOURCE_OUTPUT {
   resource_id, bytes }`, tag `0x1a`, at most 64 KiB per call, answers
   `COMMAND_RESULT` Ok or Error with `WRONG_RESOURCE_KIND`, `NOT_PRODUCER`,
   `RECORD_INVALID`, or `OVERFLOW`. A Terminal refuses it. The producer holds
   the ADR-0098 `Input` verb on the resource (every owner-socket client does).
   `OVERFLOW` is a call over its bound or a full outbound queue; retention
   eviction is never an error. `ServerFeature::RESOURCE_KINDS = 0x4000`
   advertises kind and command.
4. **Retention and replay.** The server keeps a bounded record ring per
   session, ceiling `defaults.agent-log-bytes`, default 4 MiB, in the
   [ADR-0094](./0094-explicit-per-pane-scrollback-byte-ceiling.md) pattern.
   An `AgentSession` stream always selects the raw profile with this codec:
   `BOOTSTRAP_BEGIN.codec` names it, chunks carry the retained records, live
   `RESOURCE_OUTPUT.bytes` carries whole records, `FRAME_ACK` is forbidden.
5. **The server derives state from the stream.** `prompt` and `tool_start`
   mean working; `ask`, and `notification` whose kind is `permission` or
   `elicitation`, mean blocked; `stop` means done; `session_end` retracts.
   The evidence enters the existing arbiter at a new rank, `Stream`, above
   `Hook`; precedence is Stream, Hook, Process, Screen. While a live child
   exists, screen derivation runs only for idle and departure; `idle` stays
   detector-owned, as ADR-0085 decided.
6. **`REPORT_AGENT_STATE` stays as the fallback.** With a live child its
   handler appends a synthesized `state` record; otherwise ADR-0085 applies.
7. **The Claude shim is the first producer.** Every `--phux-hook` arm reads
   the hook's stdin JSON. `SessionStart` runs `phux agent session open
   @$PHUX_TERMINAL_ID --provider claude --native-id <session_id>` and emits
   `session_start`; `UserPromptSubmit` emits `prompt`; `PreToolUse` and
   `PostToolUse` (new registrations) emit `tool_start` and `tool_end` with
   `tool_name` only, never `tool_input`; `PermissionRequest` and
   `Notification` emit `ask` and `notification` and still run `phux ask`;
   `Stop` emits `stop`; `SessionEnd` emits `session_end` and closes the
   session. A `prompt` carries its length only; `provider_raw` needs
   `PHUX_AGENT_EMIT_RAW=1`. Without `RESOURCE_KINDS` on the server the shim
   keeps its `report-state` and `agent set/clear` calls.
8. **Consumers.** `phux agent session open|close`, `phux agent emit TARGET
   --type T [--data JSON|-]`, and `phux agent log TARGET [--follow] [--json]
   [--tail N]`; `agent show` and `agent list` add `session`; MCP gains
   `phux_agent_log` and `phux_agent_emit`. `%name`
   ([ADR-0075](./0075-agent-name-addressing.md)) resolves `AgentSession`
   resources, which gives that ADR its production caller.

This amends ADR-0040 (when a session exists, `phux.agent/v1` is a projection
of its stream), ADR-0046 and ADR-0085 (the detector is a fallback and the hook
path is a producer). It does not touch
[ADR-0092](./0092-durable-work-coordinator-authority.md): the stream is live
and bounded, not durable evidence.

## Why

**Recover nothing the harness states.** Every state the detector derives from
pixels is a fact the harness emits at a known hook. Reading the hook's own
JSON costs a few lines per arm; recovering it from the screen cost a subsystem
that fails silently when a vendor restyles. The detector remains for agents no
shim covers, and idle, which no hook can prove, stays with it.

**A stream, not a state word.** ADR-0085 sends one word per edge and a late
observer learns nothing. An ordered, replayable log gives `phux agent log` an
event transcript, `agent wait` a sequence to resume from, and the TUI a state
with evidence behind it. The ADR-0070 bootstrap shape already fits: cut at a
sequence, replay, go live.

**Opaque records under a codec name, not a frame family.** ADR-0030's rule is
that the wire carries bytes under a negotiated codec and consumers project.
JSONL records with a closed `type` set are that: the server parses them only
to validate and derive state, and a new type is a codec revision, not a
frame. The server owns `seq` and `ts_ms` because producers are short-lived
hook processes that can race; server assignment keeps the log ordered.

**Privacy defaults closed.** Prompt text and tool input are the user's work,
not lifecycle; the stream carries lengths and names by default.

## Tradeoffs

- **Not built.** Kind, codec, command, ring, arbiter rank, and shim arms are
  all program work; nothing here ships today.
- **The server parses one codec it serves** to validate and derive state, an
  opacity exception confined to this codec as ADR-0046 confined its to a key.
- **The stream is only as good as its producer.** A harness with no shim
  gets the detector only; a shim that dies mid-turn leaves the last derived
  state until screen evidence for idle or departure corrects it.
- **`agent log` is a bounded window**, not a transcript; new types need a
  v2 codec. Both are deliberate.

## Alternatives

**Keep screen scraping primary.** Rejected: 6,500 lines to recover,
unreliably, what the harness states; kept as the fallback.

**Make the agent session the coordinator's `WorkSession`.** Rejected: that is
durable identity with its own endpoint (ADR-0092); this is a live L1 stream.

**A typed frame family for agent events.** Rejected on ADR-0030's principle:
structured taxonomies on the wire drift and tax every consumer.

**Record full transcripts.** Rejected for v1 on privacy and size;
`provider_raw` is the opt-in.
