---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-21
---

# The phux MCP adapter

**TL;DR.** Register `phux mcp` as a stdio server in your MCP host and keep
the local phux server running. Use `phux mcp --schema` for the installed tool
catalog and `phux mcp --skill` for operating guidance. The adapter shares
the CLI's selectors and return shapes except where noted below.

---

## Registering with a host

For Claude Code, the [Claude plugin](./claude.md) already registers MCP and
adds lifecycle hooks. Use the manual registration below for an MCP-only
setup, not in addition to the plugin.

Installing phux puts both release binaries on `PATH` but does not
register MCP with a host. Start phux first so the server is running
(`phux` starts it when needed), then register:

```sh
claude mcp add phux -- phux mcp
```

`phux mcp` does not auto-start the phux server. `phux_new` follows the
CLI create path and can start one; leave the server running for other
tools such as `phux_ls`.

For another MCP host, stdio with no adapter arguments:

```json
{
  "mcpServers": {
    "phux": {
      "command": "phux",
      "args": ["mcp"]
    }
  }
}
```

The adapter connects to the default phux socket. For a non-default
socket, set `PHUX_SOCKET` in the environment the host gives the MCP
process. An individual tool call can instead supply its optional
`socket` argument; that argument takes precedence over `PHUX_SOCKET`.
The adapter does not read credentials: access is the local Unix socket
under the permissions of the user running the host.

Inspect the installed binary without a server, config read, or JSON-RPC
handshake:

```sh
phux mcp --skill
phux mcp --schema
phux mcp --help
```

The reusable MCP skill is also published as `using-phux-mcp`. Install it
with `npx skills add no-phux/skills`. `phux mcp --skill` remains the
version-matched copy compiled into this binary.

`--schema` is the exact `tools/list` descriptor array, including each tool's
`inputSchema` and `outputSchema`. Outputs are extensible objects: ignore
unknown fields. These are standalone modes, not part of the host's server
command. `phux mcp` execs the `phux-mcp` companion; registering `phux-mcp`
directly also works.

### First inventory and snapshot

1. Leave a phux shell session running, then restart or reconnect your MCP
   host after saving its registration.
2. Ask the host to call `phux_ls` and identify the session you created.
3. Ask for `phux_snapshot` of its shell pane, using an explicit target from
   inventory. **Expected:** the tool returns the same terminal text you see
   in phux, without resizing the human view.
4. Only after checking the target, ask the host to run `pwd` in that idle
   shell using `phux_run`. A refusal that the pane is not an available shell
   is a safety check: select a shell rather than forcing input into an agent UI.

If the catalog loads but inventory fails, inspect `phux status` and the
adapter's socket environment; see
[agent connection recovery](../troubleshooting.md#an-agent-or-mcp-host-cannot-see-the-server).
Disconnecting the MCP host closes the adapter, not the phux server or its
running panes. Coordinate input with anyone attached to the same pane.

## What this is

A thin adapter over the same `phux-client` functions the agent CLI uses
([agent CLI guide](./agents.md)). It holds no protocol-level privilege.
Return shapes are the CLI `--json` documents; selectors are the CLI
`TARGET` grammar ([pane selectors](./tui.md#selectors)). Run `phux mcp --schema` for
every current argument. Do not infer required fields from this page.

## Transport and lifecycle

JSON-RPC 2.0 over the MCP stdio transport: newline-delimited JSON, one
message per line. The JSON-RPC is hand-rolled over `serde_json`. The MCP
protocol version is `2025-06-18`, including structured tool results.
Initialization also accepts `2024-11-05` and `2025-03-26` for older hosts,
which can read the equivalent JSON text content block.

| Method | Reply |
|---|---|
| `initialize` | `protocolVersion` (requested supported revision, otherwise latest), `capabilities` (`{ "tools": {} }`), `serverInfo` (`name` = `"phux"`, `version` = the crate version) |
| `notifications/initialized` | none |
| `notifications/cancelled` | none; aborts the in-flight `requestId` and returns error `-32800` for that original id |
| `tools/list` | the tool catalog |
| `tools/call` | dispatch by tool name |
| `ping` | empty result |

A malformed line yields a JSON-RPC parse error with a null id; an
unknown method on a request yields method-not-found (an unknown
notification is ignored). Tool calls run as independently abortable
tasks. Replies retain their original ids when they complete out of
order. stdin EOF aborts and drains every pending task; dropping a
CLI-backed task triggers `kill_on_drop` child cleanup.

## Target resolution

Prefer `resource_id: "@7"` (or `host/@7`) for tools that expose `target`.
It is mutually exclusive with `target` and refuses names, tags, or focus
selectors. IDs come from inventory or the additive `resource_id` on resource
results, including spawn/new receipts; they belong to one server incarnation.
Existing `target` selectors remain compatible with the CLI grammar. Exact
IDs bypass inventory resolution for snapshot, input, wait, upload, ask, and
resource reads; name-based resolution is client-side. `=` is unsupported: an MCP
request has no attached-client focus history. Use `.` or an explicit
target.

**Checkout behavior:** the source adapter **resolves `%name`**. In-process tools parse it as
`Selector::Agent` and resolve it to the named agent's Terminal (exactly
one match, or a refusal). CLI-backed session tools pass the string
through: `phux_agent_session_close`, `phux_agent_emit`, and
`phux_agent_log` therefore hit the AgentSession, while Terminal-facet
tools hit the parent pane. Two live sessions sharing a name refuse.

`phux_snapshot` and `phux_wait` make `target` optional (default
focused/last session). `phux_watch` may omit it to collect server-wide events
and `agent_state` changes for every local Terminal. `phux_send_keys`,
`phux_paste`, `phux_run`, `phux_ask`, `phux_kill`, `phux_signal`,
`phux_tag`, and the spatial tools require an explicit target. Spatial
selectors must each resolve to exactly one local same-session pane.

Socket precedence: explicit `socket` argument, then `PHUX_SOCKET`, then
the daemon default (`$XDG_RUNTIME_DIR/phux/phux.sock`, falling back to
`/tmp/phux-$UID/phux.sock`).

## Catalog

`phux mcp --schema | jq -r '.[].name'` lists the compiled catalog.

The generated [CLI/MCP parity reference](../reference/parity.md) maps each
tool to its CLI verb, execution path, and annotations; an automated gate checks
the mapping. In-process tools call the same `phux-client` function as the CLI
and return the same document. Other tools (`phux_run`, `phux_new`,
`phux_launch`, `phux_workspace`, diagnostics, and agent verbs whose documents
the CLI assembles) execute the CLI with argv, never a shell. They parse the
canonical JSON, cap stdout/stderr at 1 MiB / 64 KiB, and kill the child on
cancellation or deadline.
Every strict schema caps each string at 4096 bytes and arrays at 64 entries and
sets `additionalProperties: false`. A `phux_snapshot` with `tail` or
`unwrap` is refused over 1 MiB of document, the bound the subprocess path
had. Every result has an object-valued `structuredContent`, also encoded in
`content[0].text` for older hosts. CLI error documents keep their stable
`error.code`, receipt, remedy, and exit status. Other failures have
`{schema_version: 1, error: {code: "tool_error", message}}`; branch on
`isError` and the code, not message prose.

Additional tool contracts:

- **`phux_wait`** returns `{ "outcome": "met"|"timed_out", "polls": N }`,
  not the CLI's `ScreenState` document. Set `timeout_secs` to bound it;
  omitting that argument can wait indefinitely. It exposes `until` /
  `idle_ms` / `timeout_secs`; it does not expose `--regex`, `--tail`, or
  `--output-only`.
- **`phux_run`** bounds `timeout_secs` to `1..=3600` so a tool call
  cannot wait forever. Timeout is a tool error from the CLI's exit 125;
  there is no `outcome: "timed_out"` JSON from the CLI either.
- **`phux_watch`** is a bounded one-shot (`max_events` and/or
  `timeout_secs`). The result envelope is versioned even though CLI
  `phux watch --json` is unmarked NDJSON. A host that wants a live stream
  shells out. With no `target`, it includes agent-state changes for every
  local Terminal through the same enumerate-and-follow stream as
  `phux agent wait --any`.
- **`phux_paste`** is one paste event. A paste inserts without
  submitting; follow with `phux_send_keys` sending `Enter`. A dropped
  untrusted payload still reports `sent: true`.
- **`phux_detach`** sends `DETACH_CLIENTS` through the same library call
  as `phux detach`, and reports the count the server acked (`phux detach`
  has no `--json`). There is deliberately **no `phux_attach`**: a live ANSI
  stream has no bounded request/response shape for a `tools/call` result.
- **`phux_status`**: a stopped server is an answer, not an error. Branch
  on `running`. A null `pid` is a peer-credential gap, not "no server".
- **`phux_doctor`**: a failing check is an answer; branch on `ok` and
  each check's `status`. Check names are not unique — read every
  `server-health` row. `warn` is not `pass`. There is no repair tool and
  no log-reading tool.
- **`phux_whoami`**: executes `phux whoami --json`. An older server is
  refused (`server_too_old`), not guessed. No `remote` argument.
- **`phux_agent_wait`**: exit 124 returns as `satisfied: false`, not a
  tool failure. Edge-triggered and always bounded. Set `any: true` to wait
  across every local agent; `any` and `target` are mutually exclusive.
- **`phux_agent_log`**: no `follow` argument. A following read is a
  stream, and this adapter has no streaming result shape.
- **Agent session tools** (`phux_agent_session_open` / `close`,
  `phux_agent_emit`, `phux_agent_log`): on a server without
  `resource_kinds` in `phux_status`'s `features`, each returns
  `unsupported_server`. The adapter adds no producer of its own.
- **`phux_resource_wait`** is always bounded: `timeout_secs` is required
  (`1..=3600`). `outcome: "timed_out"` and `outcome: "gone"` are results,
  not tool errors; branch on `outcome`. Pass the returned `cursor` back as
  `after` to resume without missing an exit; `evidence_lost: true` says
  the journal had already evicted part of that range, so a `gone` may
  hide an exit. A direct `@N` target is used
  as given, so a resource that already exited can still be named.
  `phux_resource_show` and `phux_resource_methods` return the same
  documents as the CLI's `resource show|methods --json`.
- **`phux_spawn`** takes `retain_secs` (keep the pane inspectable after
  exit) and `idempotency_key` (32 hex digits; a retry answers the first
  pane with `replayed: true`); **`phux_new`** takes `idempotency_key`.
  Each is refused with `unsupported_server` when the server lacks
  `retain_on_exit` / `spawn_idempotency`, never silently ignored.
- **`phux_watch`** event items carry `seq`, `ts_ms`, and `actor` on a
  server with the event journal. `cwd_changed`, `terminal_control`,
  `journal_gap`, and `source_gap` are named (they used to arrive as
  `unknown`); the envelope stays `schema_version` 2.

**Annotations.** Every tool carries `annotations.readOnlyHint` and
`annotations.destructiveHint`, derived from the kind catalog rather than
set by hand. A tool is read-only only when every method it can send is
read-only by the catalog's conservative rule (a denied or unclassified
row counts as a write). It is destructive when any of those methods is
marked `dangerous` in the catalog (it can end a process, eject a client,
stop the server, or release a held action; ADR-0128) or needs the `INPUT`
verb (keystrokes in a live PTY can run anything); `CREATE` and `BIND`
writes are marked as writes but not destructive. The hints are advice
for a host's confirmation UI; authorization still happens at the server.

**Deliberate exclusions.** No MCP `take` / `give`: an input lease ends with
the short-lived connection. No headless focus tool: focus is client-local.
No `attach`. `server`, `stdio-bridge`, and `upgrade` are
interactive/daemon/operator lifecycles; `pair` and satellite registry
mutation handle credentials; plugin installation and config editing
mutate local trust. Those stay outside the model-facing set.

`confirm: true` is one check derived from the same catalog mark:
`phux_kill` and `phux_detach` always require it, `phux_signal` requires it
for interrupt/terminate/kill (`freeze` and `resume` are the reversible
brake), and `phux_approve` requires it to approve (a denial releases
nothing). Before `phux_kill`, a caller must display the resolved target and
obtain affirmative authorization under its operating policy. Missing
confirmation returns `error.code: "confirmation_required"` with
`approval.kind`, the wire `effect`, and an exact `approval.retry` tool call.
A noninteractive controller may authorize that exact effect and retry with
`confirm: true`; the returned recipe is not itself permission or a grant.

**Approvals (ADR-0128).** When this server holds an agent's kill, signal, or
forced detach for approval (its workload grant spells `?signal`), the tool
call normally waits for the decision. Noninteractive callers should submit
with `background: true` and a nonzero 32-hex-digit `idempotency_key`. This
returns `{schema_version: 1, operation_id, state: "pending", status_tool:
"phux_operation_status"}` immediately, keeping the requester connected.
Observe with `phux_operation_status` using that same key: `completed` carries
`result`; `failed` carries a structured `error` document. `pending` provides
an `approval_check` call to `phux_approvals` and the canonical `request`.
Listing requires `inventory` and `observe` on Global approval records: delegate
that check to an authorized controller if the requester is refused.
A pending operation is not
necessarily held; correlate the listed method, subjects, and requester
before requesting a decision. `phux_approve` decides an approval by `id`
with `decision: approve|deny`. It still needs un-held `signal` on the held
subject: the requesting workload cannot approve itself, and `confirm`
never bypasses this server check. Hosts may apply policy or delegate to a
separate authorized approver; the adapter never auto-approves.

**Mutation retries.** Every mutating tool accepts `idempotency_key`. Same
key, tool, and arguments join the in-flight invocation or replay its exact
result (including failure); a mismatch returns `idempotency_conflict`.
`background` changes observation only and is excluded from that comparison.
The bounded adapter ledger holds 128 keys without eviction and refuses new
keys with `idempotency_capacity` rather than risking duplicate execution.
This guarantee is adapter-lifetime only, not a durable transaction: it is
lost on disconnect/restart. The existing new/spawn/kill/signal keys are
also forwarded to the server, retaining their server-backed guarantees and
feature checks. Other tools' keys are not safe across adapter restarts.
Cancellation ends observation of a keyed call, not the invocation. Adapter
exit cancels unfinished workers, but effects may already have happened:
reconcile state before retrying. Serialize writes to the same pane even
when using different keys.

No tool in the orchestration sequence moves a human's local focus,
stores remote credentials, grants a persistent input lease, or schedules
future work. Serialize topology writes.

## A `tools/call` example

Request (one line on stdin):

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"phux_run","arguments":{"resource_id":"@7","command":"cargo test"}}}
```

Success:

```json
{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\"command\":\"cargo test\",\"exit_code\":0,\"output\":\"...\",\"duration_ms\":8123,\"truncated\":false}"}],"structuredContent":{"command":"cargo test","exit_code":0,"output":"...","duration_ms":8123,"truncated":false},"isError":false}}
```

The result also contains `structuredContent` with that same JSON object;
agents do not need to parse the text block. Tool failures (no target, no
server, malformed arguments) return a structured error document in JSON-RPC
`result` with `isError: true`.
Protocol failures (parse error, unknown method, missing params) return
JSON-RPC `error`.
