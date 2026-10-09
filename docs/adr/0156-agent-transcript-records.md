---
audience: contributors
stability: stable
last-reviewed: 2026-10-09
---

# 0156 — Agent transcripts ride provider_raw as phux.transcript/v1

**TL;DR.** First-party integrations append the agent's conversation to its
AgentSession stream as `provider_raw` records whose `data` follows one
provider-neutral convention, `phux.transcript/v1`: one bounded entry per
record, replaced by a later entry with the same id. No wire, frame, codec, or
record type changes. For the Pi and Claude integrations phux ships, records
carrying what the pane shows are on by default; tool output is opt-in with
`PHUX_AGENT_TRANSCRIPT=full`, and `PHUX_AGENT_TRANSCRIPT=0` turns them off.

Status: Accepted
Date: 2026-10-09

## Context

Phones should render a native transcript of an agent running in a pane while
the pane's TUI stays the real session. Scraping the TUI's screen for that is
the failure ADR-0046 and ADR-0103 retired for state. The harness already
states every turn: Pi's extension bus carries messages, streaming updates,
and tool executions; Claude's hooks carry the prompt, each tool call, and the
transcript path. ADR-0103 gave those producers an ordered, replayable stream
and an opaque record type, `provider_raw`, but defaulted it closed and left
its payload unspecified, so each consumer would have to learn each provider.

## Decision

1. **The convention.** A transcript record is `type: "provider_raw"` with
   `data = {"provider": str, "schema": "phux.transcript/v1", "entry": E}`:

   ```text
   E = { "id": str, "role": "user"|"assistant"|"thinking"|"tool"|"system",
         "text": str, "truncated": bool, "final": bool,
         "tool"?: { "name": str, "call_id": str, "summary": str,
                    "status": "running"|"ok"|"error", "output": str } }
   ```

   `tool` is present exactly when `role` is `tool`; all of its members are
   always present. `output` is empty while running and, by default, always
   (decision 4). An entry whose `id`
   repeats an earlier one replaces it. `final: false` is a streaming partial
   that a later entry with the same `id` replaces. Consumers recognize the
   convention by the `schema` word and ignore unknown members; a different
   schema word is a different convention.
2. **Bounds.** The serialized `data` is at most 16,128 bytes (16 KiB less a
   256-byte envelope reserve), so the retained line with server-stamped
   `seq` and `ts_ms` stays within the codec's 16 KiB record. Producers strip
   terminal escape sequences and control characters other than newline and
   tab. `text` is cut to fit, head kept, on a character boundary, with
   `truncated: true`. `tool.summary` is one line of at most 512 characters;
   `tool.output`, when carried, keeps at most its last 4 KiB. Ids and names
   are at most 256 bytes. A partial is sent only when its text grew and at least 250 ms
   passed since the previous partial of that id, never after its final. The
   per-session ring stays `defaults.agent-log-bytes` (4 MiB): partials and
   finals are ordinary records under ADR-0103's retention.
3. **Producers.** Pi: each user message (final); the assistant reply as
   throttled partials, then final; visible thinking, final only, when a
   thinking block ends or the message ends; each tool call as one entry keyed
   by its call id, `running` with an argument summary at start, `ok` or
   `error` at end. Claude: `UserPromptSubmit` yields a `user` entry;
   `PostToolUse` and `PostToolUseFailure` a `tool` entry keyed by
   `tool_use_id`, `ok` or `error`;
   `Stop` the turn's last reply as a final `assistant` entry, from
   `last_assistant_message` or the tail of `transcript_path`. The Claude
   wrappers call the hidden `phux agent hook-transcript`, which prints the
   `data` object, and feed it to `agent emit --data -` on stdin; Pi's
   emitter sends every record's data the same way. Conversation text never
   reaches a command line, where any local user could read it from the
   process table. A summary is the command, path, pattern, URL, or query a
   call names, else its scalar arguments with content-bearing keys (file
   contents, edit bodies) left out. Shared bounds live in
   `phux_client_core::session::agent_transcript` and in
   `@phux/integration-runtime/transcript`, with
   `AgentEventRecord::transcript_entry()` as the Rust reader.
4. **Privacy default, amending ADR-0103.** For the Pi and Claude
   integrations phux ships, transcript records are on by default and carry
   what the pane shows: prompts, replies, visible thinking, and for each
   tool call its name, one-line summary, and status, with `output` empty.
   `PHUX_AGENT_TRANSCRIPT=full` adds tool output (file contents, command
   output); `PHUX_AGENT_TRANSCRIPT=0` turns transcripts off; any other
   value, or none, is the default.
   `PHUX_AGENT_EMIT_RAW=1` keeps its meaning: it alone opts the whole raw
   provider payload in, as a separate `provider_raw` record. Typed records
   keep ADR-0103's rule: `prompt` carries a length and tool records a name.

## Why

**The pane already shows it.** The default transcript is what the pane
displays: the prompt, the reply, and a line per tool call. The AgentSession is
a child of that Terminal, read through the same socket and the same ADR-0098
verbs, so defaulting it closed hides nothing from anyone who can attach; it
only forces every phone user to find an environment variable first. Tool
output is different in kind: a file read or a command's output is often
collapsed or never drawn, and a `.env` read would otherwise land in the
4 MiB ring for any socket client, so it is opt-in with `full`. The raw
payload carries even more the screen never shows and stays behind
`PHUX_AGENT_EMIT_RAW`.

**One convention, many providers.** A phone reads `role`, `text`, and
`tool`, never a provider's hook schema. A new harness integration maps its
own events once, in its adapter, and every frontend renders it unchanged.

**No wire change.** `provider_raw` is already in the closed v1 type set and
the server never interprets its `data` (L1 §4.8), so the convention needs no
codec revision, no capability bit, and no server release. A server that
predates this ADR retains and replays these records unchanged.

**Replace by id, not append.** Streaming text is many records with one
meaning. Keying them lets a consumer keep one row per entry and a late
observer reconstruct the current transcript from the bootstrap alone.

## Tradeoffs

- Partials spend ring capacity. The 250 ms grow-only throttle bounds a
  reply at four records a second, and the 4 MiB ring still evicts oldest
  first, so a long session's early transcript ages out of the replay.
- A transcript is a bounded window, not an archive: a long reply or tool
  output is cut, and a consumer sees `truncated` rather than the rest.
- By default a phone sees that a tool ran and on what, not what it
  returned; seeing results needs `full` in the agent's environment.
- Claude yields only the turn's last reply. Assistant text between tool
  calls in one turn is not emitted, and the prompt id is time-based because
  the hook carries none.
- Both a Rust and a TypeScript implementation of the bounds exist, one per
  producer runtime. Their tests pin the same caps; a change to one is a
  change to both.

## Alternatives

**A new record type or codec revision.** Rejected: a `transcript` type
would need `AgentEventsJsonlV2` and a server release for a payload the
server never reads; `provider_raw` already carries opaque provider events.

**Keep ADR-0103's closed default and ask users to opt in.** Rejected for
first-party integrations: it protects nothing the pane does not already
show, and makes the phone transcript a hidden feature.

**Forward the raw provider payload and parse it on the phone.** Rejected:
every frontend would learn every provider's event schema, and the raw
payload carries data the screen never shows.

**Scrape the pane's screen on the phone.** Rejected for the reasons
ADR-0046 and ADR-0103 give: it recovers, unreliably, what the harness states.
