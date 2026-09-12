---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# The phux MCP adapter

**TL;DR.** What is MCP-specific in `phux mcp`: stdio JSON-RPC,
registration, socket selection, and the tools the adapter deliberately
omits. The installed catalog is `phux mcp --schema`; the operating guide
is `phux mcp --skill`. Return shapes and selectors are the shared agent
surface.

---

## Registering with a host

Installing phux puts both release binaries on `PATH` but does not
register MCP with a host. Start phux first so the server is running
(`phux` starts it when needed), then register:

```sh
claude mcp add phux -- phux mcp
```

`phux mcp` does not auto-start the phux server, and neither does
`phux_new`. Leave the server running while the host calls tools such as
`phux_ls`.

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

`--skill` is the compiled operating guide. `--schema` is the exact MCP
Tool descriptor array returned by live `tools/list`, including each
`inputSchema`; it is not a catalog of tool *output* schemas. These are
standalone modes and do not belong in the host's normal server command.
`phux --capabilities --json` reports whether this companion is
discoverable beside phux or on `PATH`.

The launcher replaces itself with `phux-mcp`, preserving stdio, signals,
and exit status. Direct `phux-mcp` registrations remain valid.

## What this is

A thin adapter over the same `phux-client` functions the agent CLI uses
([`agents.md`](./agents.md)). It holds no protocol-level privilege.
Return shapes are the CLI `--json` documents; selectors are the CLI
`TARGET` grammar ([`tui.md`](./tui.md#selectors)). Run `phux mcp --schema` for
every current argument. Do not infer required fields from this page.

## Transport and lifecycle

JSON-RPC 2.0 over the MCP stdio transport: newline-delimited JSON, one
message per line. The JSON-RPC is hand-rolled over `serde_json`. The MCP
protocol version is pinned to `2024-11-05`.

| Method | Reply |
|---|---|
| `initialize` | `protocolVersion`, `capabilities` (`{ "tools": {} }`), `serverInfo` (`name` = `"phux"`, `version` = the crate version) |
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

Every targeted tool takes a `target` selector string in the same grammar
as the CLI. Resolution is client-side. `=` is unsupported: an MCP
request has no attached-client focus history. Use `.` or an explicit
target.

This tree's adapter **resolves `%name`**. In-process tools parse it as
`Selector::Agent` and resolve it to the named agent's Terminal (exactly
one match, or a refusal). CLI-backed session tools pass the string
through: `phux_agent_session_close`, `phux_agent_emit`, and
`phux_agent_log` therefore hit the AgentSession, while Terminal-facet
tools hit the parent pane. Two live sessions sharing a name refuse.

`phux_snapshot` and `phux_wait` make `target` optional (default
focused/last session). `phux_watch` may omit it to collect server-wide
events (no `agent_state` items in that case). `phux_send_keys`,
`phux_paste`, `phux_run`, `phux_ask`, `phux_kill`, `phux_signal`,
`phux_tag`, and the spatial tools require an explicit target. Spatial
selectors must each resolve to exactly one local same-session pane.

Socket precedence: explicit `socket` argument, then `PHUX_SOCKET`, then
the daemon default (`$XDG_RUNTIME_DIR/phux/phux.sock`, falling back to
`/tmp/phux-$UID/phux.sock`).

## Catalog

`phux mcp --schema` prints the same array `tools/list` returns, as a
standalone pretty-printed JSON document, and exits. It needs neither a
running phux server nor a JSON-RPC handshake: the catalog is compiled
into the binary.

```sh
phux mcp --schema | jq -r '.[].name'
```

The flag lives on the MCP companion rather than `phux api schema` so
the schemas cannot drift and the main binary does not link the MCP
stack.

Name-for-name mapping onto the CLI. CLI-subprocess tools execute argv
(never a shell), parse the canonical JSON, cap each string at 4096 bytes
and arrays at 64 entries, cap stdout/stderr at 1 MiB / 64 KiB, and kill
the child on cancellation or deadline. Every strict schema sets
`additionalProperties: false`. In-process tools reuse `phux-client`
directly.

Contract facts `--schema` descriptions do not collect:

- **`phux_wait`** is a bounded `{ "outcome": "met"|"timed_out", "polls": N }`
  gate, not the CLI's `ScreenState` document. It exposes `until` /
  `idle_ms` / `timeout_secs`; it does not expose `--regex`, `--tail`, or
  `--output-only`.
- **`phux_run`** bounds `timeout_secs` to `1..=3600` so a tool call
  cannot wait forever. Timeout is a tool error from the CLI's exit 125;
  there is no `outcome: "timed_out"` JSON from the CLI either.
- **`phux_watch`** is a bounded one-shot (`max_events` and/or
  `timeout_secs`). The result envelope is versioned even though CLI
  `phux watch --json` is unmarked NDJSON. A host that wants a live stream
  shells out.
- **`phux_paste`** is one paste event. A paste inserts without
  submitting; follow with `phux_send_keys` sending `Enter`. A dropped
  untrusted payload still reports `sent: true`.
- **`phux_detach`** talks `DETACH_CLIENTS` in-process (`phux detach` has
  no `--json`). There is deliberately **no `phux_attach`**: a live ANSI
  stream has no request/response shape for the one-text-content-block
  `tools/call` envelope.
- **`phux_status`**: a stopped server is an answer, not an error. Branch
  on `running`. A null `pid` is a peer-credential gap, not "no server".
- **`phux_doctor`**: a failing check is an answer; branch on `ok` and
  each check's `status`. Check names are not unique — read every
  `server-health` row. `warn` is not `pass`. There is no repair tool and
  no log-reading tool.
- **`phux_whoami`**: executes `phux whoami --json`. An older server is
  refused (`server_too_old`), not guessed. No `remote` argument.
- **`phux_agent_wait`**: exit 124 returns as `satisfied: false`, not a
  tool failure. Edge-triggered and always bounded.
- **`phux_agent_log`**: no `follow` argument. A following read is a
  stream, and this adapter has no streaming result shape.
- **Agent session tools** (`phux_agent_session_open` / `close`,
  `phux_agent_emit`, `phux_agent_log`): on a server without
  `resource_kinds` in `phux_status`'s `features`, each returns
  `unsupported_server`. The adapter adds no producer of its own.

**Deliberate exclusions.** No MCP `take` / `give`: the CLI lease belongs
to the short-lived subprocess connection, so advertising a persistent
lease would be dishonest. No headless focus tool: focus is client-local.
No `attach`. `server`, `stdio-bridge`, and `upgrade` are
interactive/daemon/operator lifecycles; `pair` and satellite registry
mutation handle credentials; plugin installation and config editing
mutate local trust. Those stay outside the model-facing set.

`phux_kill` and `phux_detach` require `confirm: true`. `phux_signal`
requires it for interrupt/terminate/kill. Before `phux_kill`, a caller
must display the resolved target and obtain explicit human confirmation.

No tool in the orchestration sequence moves a human's local focus,
stores remote credentials, grants a persistent input lease, or schedules
future work. Serialize topology writes.

## A `tools/call` example

Request (one line on stdin):

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"phux_run","arguments":{"target":"work:1.0","command":"cargo test"}}}
```

Success:

```json
{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\n  \"command\": \"cargo test\",\n  \"exit_code\": 0,\n  \"output\": \"...\",\n  \"duration_ms\": 8123,\n  \"truncated\": false\n}"}],"isError":false}}
```

The result is a **single text content block**. A tool failure — no such
target, no running server, a malformed argument — is a *successful*
JSON-RPC response carrying `isError: true`, never a JSON-RPC error and
never a crash. Protocol-level errors (parse, unknown method, missing
params) *are* JSON-RPC `error` responses.
