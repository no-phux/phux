# Bootload measurement methodology and receipts

Appendix to [Where to cache an agent's context bootload](./context-bootload-cache.md),
Beads `phux-hhb7z`, 23 October 2026. Repository examined: `ac3870eb`.

## Setup and definition of cold

The probes ran on the Mac development host, from this linked worktree, before
the research document was written. Each was a new CLI process and conversation;
neither resumed or forked the parent conversation. Installed packages, filesystem
caches, credentials and provider infrastructure were not flushed. Thus **cold
means a fresh worker/session, not a cold OS or a guaranteed empty provider
cache**. The primary observations nevertheless both reported zero cache reads.

| Observation | OpenCode | pi |
| --- | --- | --- |
| Installed CLI | `opencode v2.1.3` | `pi 1.4.0` |
| Model recorded by harness | `openai/gpt-6.1-sol`, variant `default` | `openai-codex/gpt-6.1-sol`, configured thinking `high` |
| Input tokens, first model response | 14,976 | 11,445 |
| Cache-read / cache-write tokens, first response | 0 / 0 | 0 / 0 |
| Output tokens, first response | 36 | 70 |
| Process start → first tool call | 3.137 s | 10.415 s |
| Process start → first tool completion event received | 3.645 s | 11.559 s |
| First operation | `read` of repository `AGENTS.md` | `fabric_exec` calling `pi.read` on repository `AGENTS.md` |
| Process start → CLI exit | 35.837 s | 74.599 s |

Input is the harness's first-response input counter, not an estimate from file
length, and not the sum of every subsequent bootload turn. The first request
precedes the explicit `bd prime` tool result. Later requests therefore contain
more context. Provider counters and model settings are not identical across
harnesses; no billing comparison is inferred from these numbers.

A Python wrapper started a monotonic clock immediately before `Popen` and
timestamped each stdout JSON line on arrival, while retaining stderr separately.
For pi, first-tool latency is the arrival of `tool_execution_start`. OpenCode's
JSON CLI exposes completed `tool_use` parts, so the first-call time is recovered
from the first part's `state.time.start`: subtract
`(event.timestamp - state.time.start) / 1000` from that line's monotonic arrival
time. This is a harness-recorded call boundary, not a packet-level inference
trace; completion-event arrival is reported separately to expose the distinction.
The timing receipt, without prompt contents, is:

```json
{"worker":"opencode","first_tool_event_arrival_s":3.644814375,"event_timestamp_ms":1792737430096,"tool_start_ms":1792737429588,"derived_first_call_s":3.136814375}
{"worker":"pi","first_tool_execution_start_arrival_s":10.414870333}
```

The OpenCode session database was inspected read-only, scoped to the scratch
session, to confirm model and usage because the CLI omitted its final
`step_finish` event. First-response evidence was also present in stdout:

```json
{"tokens":{"input":14976,"output":36,"reasoning":0,"cache":{"read":0,"write":0}}}
```

Pi's first assistant `message_end` reported:

```json
{"input":11445,"output":70,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":11515}
```

There is already reuse within these sessions: OpenCode's second request reported
1,971 ordinary input tokens plus 14,848 cache-read tokens; pi's second reported
1,613 ordinary input plus 11,392 cache-read tokens. Those are observed harness
counters, not evidence that two independent workers share a provider cache.

## Isolation, configuration, and limitations

Both workers inherited the user's instruction files and installed extensions,
but all inherited `PHUX_*` variables were removed. The wrapper then set
`PHUX_PROFILE=bootload-research` and `PHUX_SOCKET` to an **absent socket in a
scratch directory**. In particular, no production pane id, target, websocket
credential or mail address was inherited. No phux server was started and neither
probe was authorized to contact another socket. Fleet context was deliberately
unavailable, not a measurement of a populated fleet.

OpenCode used `--standalone`, a fresh session in its normal credential-bearing
database, and an explicit model. Temporary `OPENCODE_CONFIG_CONTENT` disabled
the `phux-ops` MCP server, denied `integration.use` for `mcp:*`, and denied
`plugin:github:phall1/blackbird*`. No global config was edited. Pi used
`--no-mcp` and an explicit scratch session directory. This disables pi's built-in
MCP loader, **not every extension-owned discovery path**: Fabric still attempted
`kit-dev-mcp`, whose stderr reported missing Python dependency `tqdm`. The worker
completed with exit 0; that failure and installed extension overhead remain in
the pi timing. The absent phux socket remained the safety boundary.

The installed OpenCode phux plugin was pinned to `78341ce7`; installed pi used
`@phux/pi` 0.8.0. These were measurements of the user's installed harness setup
against this checkout, not rebuilt integrations from the examined revision.
The project `.pi/settings.json` points to `../integrations/pi`; the global
package arbitration in [activation.ts](../../integrations/pi/src/activation.ts)
prefers the globally configured phux package. This distinction matters for
reproducing the experiment.

The common worker prompt was:

> This is a bounded read-only cold-boot measurement, not implementation work.
> Your first tool call must read the repository AGENTS.md. Then read CLAUDE.md,
> inspect the locally applicable hooks and skill catalog (do not read all skill
> bodies), and run bd prime. Fleet context is intentionally unavailable:
> PHUX_SOCKET points to a nonexistent research socket; do not contact any other
> phux socket, server, Blackbird service or fleet. Do not claim/close/update any
> issue, delegate, write files, run builds, or run git mutations. Once bootload
> is read, reply BOOTLOAD_DONE with the sources loaded and stop. This is the
> entire task.

This **deliberately forces a reread**. It measures that requested workflow, not
the spontaneous probability of rereads or the necessity of every read. The
agents inspected some other-harness settings too; pi's completion extension
added a verification continuation. Full CLI runtime is consequently not pure
preparation time. Both emitted `BOOTLOAD_DONE`; their tool traces, not that
assertion alone, define what was loaded. The primary OpenCode worker recovered
the truncated beginning of `bd prime`; the initial exploratory worker described
below only read a clipped version and is not a full-bootload baseline.

Pi's emitted initial system message already contained 13,809 UTF-8 bytes of
project instructions and 12,587 bytes of skill catalog. Thus a compiled prompt
cache would not by itself prevent the explicitly requested reread of information
already in the request. A separate local `bd prime` subprocess took **1.783 s**
and produced **80,658 bytes**, including 87 memories. This is one additional
command sample, not a token count or an isolated component of the first-tool
latency. `AGENTS.md` was 6,340 bytes; `CLAUDE.md` was 11 bytes (`@AGENTS.md`).

Two exploratory OpenCode runs explain why the final probe retained the normal
credential store:

- A new scratch `OPENCODE_DB` with no explicit model selected
  `opencode/big-pickle`: 15,834 ordinary input + 384 cache-read tokens,
  first call 3.440 s, exit after 50.014 s. Excluded from the primary comparison.
- An explicit `openai/gpt-6.1-sol` against a new scratch database failed with
  `provider.no-route: Model unavailable`, exit 1 after 0.861 s. Reusing the normal
  credential-bearing database with a **new standalone conversation**, rather
  than modifying credentials or model configuration, produced the primary run.

## Reproduction and evidence receipt

Use the prompt above as `PROMPT`, run from the repository worktree, and sanitize
the child environment as described above. The OpenCode-only temporary config
was equivalent to:

```json
{
  "mcp": {"servers": {"phux-ops": {"type": "local", "command": ["phux-ops-mcp"], "disabled": true}}},
  "policies": [
    {"action": "integration.use", "resource": "mcp:*", "effect": "deny"},
    {"action": "integration.use", "resource": "plugin:github:phall1/blackbird*", "effect": "deny"}
  ]
}
```

Pass that JSON through the **child's** `OPENCODE_CONFIG_CONTENT`; do not write it
over user settings. In the wrapper, environment sanitization was
`not k.startswith(("PHUX_", "OPENCODE_SESSION", "BLACKBIRD_"))`, followed by the
two scratch phux values above. Core invocations in that sanitized environment
were:

```sh
opencode run --model openai/gpt-6.1-sol --standalone --format json \
  --title 'Scratch cold bootload measurement' "$PROMPT"
pi --mode json --print --approve --no-mcp \
  --session-dir "$SCRATCH/pi-sessions" "$PROMPT"
```

The pi invocation inherited `defaultProvider: "openai-codex"`,
`defaultModel: "gpt-6.1-sol"` and `defaultThinkingLevel: "high"` from global
settings. For reproduction, pin them with
`--model openai-codex/gpt-6.1-sol --thinking high`. Thinking level is a latency
confounder; this single run does not isolate its causal contribution.

`--approve` trusts project-local pi resources for this invocation, without
changing the user's trust configuration. Use the normal credential store, no
`--continue`, and no existing session id. Keep package caches intact if comparing
to these observations. Capture line arrival times, not just `/usr/bin/time` at
exit. Read first OpenCode `step_finish.part.tokens` and first pi assistant
`message_end.message.usage`; never substitute total conversation tokens.

Local raw transcripts remain outside Git because they contain user instruction
and hook contents. Scratch root on this host:
`/private/var/folders/6k/_cy0x5fn1c53gx23j8s3_mnm0000gn/T/opencode/context-bootload-cache`.
This is a local receipt, not a promised permanent artifact store.

| Artifact | Session | SHA-256 of raw stdout JSONL |
| --- | --- | --- |
| `opencode-auth.jsonl` | `ses_e9885c2ebffe52x8vZqidPUIz7` | `eb03c8e1d6c8635b7ee79639a65d822981cf7ee8132fdbfd48c62e25dfa0eb6b` |
| `pi.jsonl` | `01a16777-4391-77eb-b338-ffd5d0744178` | `d21bdf5c6e42ebe791027e768ea12fdff9e893a4a3e1d2cc002533b2b21d6d7c` |

The directory also contains the measurement wrapper, argv/cwd receipts, stderr,
monotonic `*-arrival.jsonl` files, and `*-result.json` exit records. No raw prompt,
credential store or runtime implementation is included in this PR.
