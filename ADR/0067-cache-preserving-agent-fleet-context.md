---
audience: contributors
stability: stable
last-reviewed: 2026-08-01
---

# 0067 — Cache-preserving agent fleet context

**TL;DR.** Agent-host integrations project phux fleet metadata into model
context as bounded, sequenced tail messages. The first observation is a
checkpoint; later changes are deltas and unchanged state emits nothing.
Compaction receives or is followed by a fresh checkpoint. Static system prompts
and tool definitions never carry live fleet values.

Status: Accepted
Date: 2026-08-01

## Context

phux already derives agent identity, lifecycle, and attention for every live
Terminal. Pi and OpenCode expose tools that can query that projection, but the
model does not know it is inside phux, which pane is itself, or whether peer
agents became idle or blocked until it chooses to query. Tool-only discovery is
too late for orchestration.

Putting a current fleet snapshot in the base system prompt solves awareness but
moves volatile data to the front of every provider request. Prefix caches then
stop at the first changed byte, repeatedly charging for otherwise stable
instructions, context files, and tool definitions.

## Decision

1. Pi and OpenCode refresh the public `phux agent list --json` projection at
   each new model-facing user turn. They do not subscribe, modify the wire, or
   inject terminal output.
2. Each host session owns a sequence. Its first observation is a full
   `checkpoint`. A changed projection appends a `delta` with `base_seq`; an
   unchanged projection appends nothing. Latest sequence wins.
3. Context is added only at the request tail: Pi appends a hidden custom
   message after the user message; OpenCode appends a synthetic text part to
   that new user message. Live values never rewrite the static system prompt or
   tool schemas.
4. A checkpoint replaces the delta chain after eight deltas, after branch/tree
   movement, and after compaction. OpenCode also supplies a canonical checkpoint
   to its compaction hook. Pi forces one on the first turn after compaction.
5. The projection is bounded to 8 KiB and at most 64 sorted panes. It carries
   canonical Terminal, session and window identity, agent label/kind,
   lifecycle state, attention, cwd, this host's inherited Terminal id, and its
   selected target. It reports an omitted count when bounded.
6. Screen rows, scrollback, titles, detector evidence, explanations, prompts,
   tool output, and credentials are never automatic context. Snapshot and event
   tools remain the explicit path for terminal content.
7. Every context message labels values as untrusted observations, not
   instructions. Control characters are removed and strings are length-bounded
   before JSON encoding.
8. Refresh failure is best effort. One bounded `unavailable` checkpoint is
   emitted and identical failures are edge-filtered. A recovery emits a fresh
   checkpoint. `PHUX_CONTEXT_AWARENESS=0` disables the feature.

This is a consumer behavior only. It adds no frame, metadata key, or protocol
version.

## Why

Exact-prefix caches reward append-only conversations. A persistent tail message
becomes part of that conversation, so later requests reuse everything through
it and pay only for subsequent turns and deltas. Edge filtering also prevents
idle fleets from consuming tokens merely because another turn began.

A checkpoint/delta stream gives the model both freshness and an explicit stale
state rule. Compaction is already a context discontinuity; establishing a new
checkpoint there bounds reconstruction without introducing an additional
steady-state cache break.

## Tradeoffs

- Awareness is current at user-turn boundaries, not continuously during one
  uninterrupted provider/tool loop. Tools still perform fresh reads before
  consequential actions.
- Each turn pays for one bounded local subprocess even when no context is
  emitted. The one-second local timeout prevents a missing server from blocking
  the host indefinitely.
- Cwd and names are useful orchestration metadata but may contain sensitive or
  adversarial text. They are local, bounded, structured, and explicitly
  untrusted; deployments that do not want them in model context disable the
  feature.
- Provider cache implementations differ. This design preserves an exact stable
  prefix but cannot force a provider to cache it.
- OpenCode's compaction and message hooks are experimental pre-1.0 APIs, so its
  package smoke remains a required compatibility gate.

## Alternatives

**Rewrite a live system-prompt block.** Rejected because one state transition
invalidates the reusable prefix containing static instructions and tools.

**Append an ephemeral current snapshot before every provider call.** Rejected:
when the snapshot is not persisted, the next request diverges where the prior
snapshot used to be and loses the rest of that turn's prefix.

**Require the model to call `phux_panes`.** Retained for explicit fresh detail,
but rejected as the only awareness path because orchestration cannot react to
state it does not know to query.

**Inject pane screens automatically.** Rejected for token cost, prompt-injection
risk, and disclosure. Fleet metadata says where attention is needed; snapshot
remains an explicit action.
