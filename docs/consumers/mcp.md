---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-07-15
---

# The phux MCP adapter

**TL;DR.** This doc covers what is MCP-specific in `phux-mcp`: the 22
JSON-RPC stdio tools spanning inspection, execution, session lifecycle,
agent identity, existing-pane layout, and bounded plugin/workspace operations;
the stdio transport and lifecycle; target resolution; and the `tools/call`
envelope.
The structured shapes the tools return and the selector grammar are the
shared agent surface and live in their owning docs; this file links them. The
canonical orchestration loop and safety boundaries live in
[`phux-agent-cli/SKILL.md`](../../examples/skills/phux-agent-cli/SKILL.md).

---

## Registering with a host

Installing phux puts `phux-mcp` on `PATH`, but does not register it with an
MCP host. Start phux first so the server is running (`phux` starts it when
needed), then register the stdio adapter with Claude Code:

```sh
claude mcp add phux -- phux-mcp
```

`phux-mcp` does not auto-start the phux server, and neither does the
`phux_new` tool. Leave the server running while the host calls tools such as
`phux_ls`.

For another MCP host, select its stdio transport and use `phux-mcp` with no
arguments. Hosts that use the common MCP server configuration shape can use:

```json
{
  "mcpServers": {
    "phux": {
      "command": "phux-mcp"
    }
  }
}
```

The adapter connects to the default phux socket. For a non-default socket, set
`PHUX_SOCKET` in the environment the host gives the MCP process:

```json
{
  "mcpServers": {
    "phux": {
      "command": "phux-mcp",
      "env": {
        "PHUX_SOCKET": "/absolute/path/to/phux.sock"
      }
    }
  }
}
```

An individual tool call can instead supply its optional `socket` argument;
that argument takes precedence over `PHUX_SOCKET`. The adapter does not read
credentials or require credentials in its host configuration: access is to
the local Unix socket under the permissions of the user running the host.

## 0. What this is, what this isn't

This is the MCP adapter only. `phux-mcp` has no separate core: each tool
is a thin wrapper over the same `phux-client` functions the agent CLI
verbs use ([`agents.md`](./agents.md)). Like every consumer it holds no
protocol-level privilege
([ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md)); the
structured agent surface is a local projection over the shared engine,
exposed through the CLI and its versioned JSON schema, not a wire service
([ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)).

Two things this file does not restate, by the "one fact, one home" rule:

- The structured return shapes — `ScreenState`, `RunResult`,
  `SessionListJson` — are owned by [`agents.md`](./agents.md) §4. Each
  tool below names the shape and links there.
- The selector grammar is owned by [`tui.md`](./tui.md) §3. §2 below
  links it.

---

## 1. Transport and lifecycle

`phux-mcp` speaks **JSON-RPC 2.0 over the MCP stdio transport**:
newline-delimited JSON, one message per line on stdin and stdout. The
JSON-RPC is hand-rolled over `serde_json` — no framework dependency.

The MCP protocol version is pinned to the `2024-11-05` revision. Newer
MCP revisions are additive; the pin is bumped when the adapter adopts
one.

The methods:

| Method | Reply |
|---|---|
| `initialize` | `protocolVersion`, `capabilities` (`{ "tools": {} }`), and `serverInfo` (`name` = `"phux"`, `version` = the crate version) |
| `notifications/initialized` | none (it is a notification) |
| `notifications/cancelled` | none; aborts the in-flight `requestId` and returns error `-32800` for that original id |
| `tools/list` | the tool catalog (see §3) |
| `tools/call` | dispatch by tool name (see §3, §4) |
| `ping` | an empty result (keepalive) |

Robustness: a malformed line yields a JSON-RPC parse error with a null
id; an unknown method on a request yields a method-not-found error (an
unknown notification, having no id, is silently ignored). Tool calls run as
independently abortable tasks while the server keeps reading requests. Replies are
serialized through the transport loop and retain their original ids even when
they complete out of order. `notifications/cancelled` aborts the matching task;
stdin EOF aborts and drains every pending task. Dropping a CLI-backed task
therefore triggers the subprocess adapter's `kill_on_drop` child cleanup.

---

## 2. How a tool resolves a target

Every targeted tool (`phux_snapshot`, `phux_send_keys`, `phux_paste`,
`phux_run`, `phux_wait`, `phux_kill`, `phux_watch`, `phux_ask`, `phux_launch`,
`phux_spawn`, `phux_signal`, `phux_tag`, and the three pane-layout tools)
takes a `target` selector string in the **same grammar as the CLI's
`TARGET`**, whose table and examples live in
[`tui.md`](./tui.md) §3. In one line, the forms are: `.` (current), `=`
(last), `name` (session), `name:N` / `name:tag` (window), `name:N.M`
(pane), `@N` (local opaque id), `host/@N` (satellite terminal), and `#tag`
(tag set, where the tool permits a set).

Resolution is **client-side**, exactly as the CLI resolves it
([ADR-0021](../../ADR/0021-control-plane-commands.md)): the adapter
fetches a state snapshot, expands the selector to candidate
`TerminalId`s, then narrows to a single pane — the focused pane if it is
among the candidates, else the first in snapshot order. This is the same
`pick_target_pane` tiebreak the CLI uses. The server never parses a
selector. `=` is explicitly unsupported here because an MCP request has no
attached-client focus history; callers must use `.` or an explicit target.

`target` optionality differs per tool:

- `phux_snapshot` and `phux_wait` make `target` **optional**; when absent
  they default to the focused/last session (`Selector::Last`). `phux_watch`
  also permits omission to collect server-wide events.
- `phux_send_keys`, `phux_paste`, `phux_run`, `phux_ask`, `phux_kill`,
  `phux_signal`, `phux_tag`, and the spatial tools require explicit targets. Spatial selectors
  must each resolve to exactly one local same-session pane rather than applying
  the focused-pane tiebreak.
- `phux_launch` and `phux_spawn` use optional `target` only for explicit local
  placement. `phux_agent` uses it for pane-specific actions.

Server-facing tools also take an optional `socket` string naming the
Unix-domain socket to connect to. Precedence: an explicit `socket`
argument, then the `PHUX_SOCKET` environment variable, then the daemon
default (`$XDG_RUNTIME_DIR/phux/phux.sock`, falling back to
`/tmp/phux-$UID/phux.sock` — the segment is `$UID`, then `$USER`, then
the literal `default`).

---

## 3. The tool catalog

Twenty-two tools, returned verbatim by `tools/list`. Each `inputSchema` is a
JSON Schema `object`. Tools that take no required argument (e.g.
`phux_ls`) work with no `arguments` at all. The return shapes are the
shared agent shapes owned by [`agents.md`](./agents.md) §4; each tool
names its shape and links there.

### 3.0 Reading the catalog without a session

`phux-mcp --schema` prints the same array `tools/list` returns, as a
standalone pretty-printed JSON document, and exits:

```sh
phux-mcp --schema | jq -r '.[].name'
phux-mcp --schema > phux-tools.json
```

It needs neither a running phux server nor a JSON-RPC handshake: the
catalog is a compile-time constant of the binary, so the tool surface is
readable before anything is wired up. Useful for pinning the surface in a
test, diffing it across releases, or generating a client.

The flag lives on `phux-mcp` rather than as `phux api schema` because this
binary already owns the schemas. Exposing them from the main `phux` binary
would mean either duplicating them — after which they drift — or linking
the whole MCP stack into every `phux ls`.

### 3.1 `phux_ls`

Lists phux sessions on the running server. No target.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `socket` | string | no | Override the UDS path (see §2). |

Result: the canonical versioned `phux ls --json` document:
`{ "schema_version": 3, "sessions": [ { "name", "windows", "attached",
"attached_clients" } ],
"terminals": [...], "unreachable": [...] }`, sorted by name. MCP executes and
parses that CLI surface, so one parser works for both — including
`unreachable`, which is empty exactly when the listing covers the whole fleet
([`agents.md`](./agents.md) §4.1). A non-empty `unreachable` makes `sessions`
and `terminals` a lower bound, not an inventory.

### 3.2 `phux_snapshot`

Captures a pane as structured screen data. Side-effect-free: it does not
attach or resize.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | no | Selector (see §2). Defaults to focused. |
| `scrollback` | number | no | Tri-state — see below. |
| `cells` | boolean | no | When true, include per-cell OSC-133 marks and styles. Default `false`. |
| `socket` | string | no | Override the UDS path (see §2). |

`scrollback` is **tri-state**: **absent** captures the viewport only;
**`0`** captures all retained history; **`N`** captures the most-recent
`N` rows.

Result: a serialized `ScreenState` — the same struct `phux snapshot`
emits, with `cells` populated only when `cells` is true. The field
catalog (schema version, `cursor`, `lines`, `scrollback`, the sparse
`cells` array) is owned by [`agents.md`](./agents.md) §4.2.

### 3.3 `phux_send_keys`

Routes input to the resolved pane by id. No attach, no resize.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | yes | Selector (see §2). |
| `keys` | array of string | yes | Keys to send; must be non-empty. |
| `socket` | string | no | Override the UDS path (see §2). |

Each entry in `keys` is a named key (`Enter`, `Tab`, `C-c`, ...) or a
literal string, tmux-style.

Result: `{ "sent": true, "pane": "<pane>" }`. `pane` is the canonical
direct selector (`@N` or `host/@N`) and can be passed back as `target`.

### 3.4 `phux_run`

Runs a command in the resolved pane and reports its result. Assumes a
POSIX shell.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | yes | Selector (see §2). |
| `command` | string | yes | The command line to run. |
| `timeout_secs` | number | no | Default `600`; `0` waits indefinitely. |
| `socket` | string | no | Override the UDS path (see §2). |

Result on completion: a serialized `RunResult`
(`{ command, exit_code, output, duration_ms, truncated }`), shape owned
by [`agents.md`](./agents.md) §4.3.

MCP executes the canonical `phux run --json` command. Completion returns the
same `RunResult`; timeout emits no JSON and becomes an MCP tool error from the
CLI's exit `125`, matching the CLI contract. MCP additionally bounds
`timeout_secs` to `1..=3600` so a tool call cannot wait forever.

### 3.5 `phux_wait`

Polls the resolved pane until a condition holds.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | no | Selector (see §2). Defaults to focused. |
| `until` | string | no | Succeed once a visible line contains this substring. |
| `idle_ms` | number | no | Succeed once the screen holds still this long. |
| `timeout_secs` | number | no | Give up after this many seconds. The API default is unbounded; orchestration callers must provide a finite value. |
| `socket` | string | no | Override the UDS path (see §2). |

Condition precedence: `until` wins when present (succeed on a substring
match); otherwise the tool settles on idle, using `idle_ms` or the
default dwell when `idle_ms` is absent.

Result: `{ "outcome": "met" | "timed_out", "polls": N }`.

### 3.6 `phux_new`

Creates a named session without attaching through canonical
`phux new --json`; the CLI may start the local server when needed.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | Name for the new session. A name already in use is rejected. |
| `command` | array | no | Initial command (argv) for the seed pane. Omit or pass `[]` for the server's default shell. |
| `cwd` | string | no | Working directory for the seed pane. |
| `socket` | string | no | Override the UDS path (see §2). |

Result: the new session's name and seed pane id.

### 3.7 `phux_kill`

Tears down the Terminal(s) a selector resolves to — a whole session, a
window, a pane, or `@id` — in one atomic `KILL_TERMINALS`.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | yes | Selector (§2). Resolves to its full id set. |
| `confirm` | boolean (`true`) | yes | Explicit destructive-operation confirmation; false or absent is rejected before subprocess execution. |
| `socket` | string | no | Override the UDS path (see §2). |

Result: `{ "schema_version": 1, "killed": true, "target": "..." }` after the
canonical CLI exits successfully. A clean server disconnect after reaping its
last session is already treated as success by that CLI path.

### 3.8 `phux_watch`

The push half of the agent surface — tagged lifecycle/activity events
(`command_started`/`finished`, `title_changed`, `bell`,
`pane_spawned`/`closed`, `dirty`, `idle`, `asked`) — exposed as a **bounded
one-shot** tool. MCP `tools/call` is request/response while the underlying
stream is long-lived, so the tool collects events until a bound is reached,
then returns the batch; an MCP host that wants a truly live stream still
shells out to `phux watch --json`.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | no | Pane selector to watch. Omit for server-wide events. |
| `max_events` | number | no | Return after collecting this many events. |
| `timeout_secs` | number | no | Return after this many seconds. Canonical orchestration always supplies this and/or `max_events`; without either the call blocks until the server exits. |
| `socket` | string | no | Override the UDS path (see §2). |

Result: `{ events: [ { event, terminal?, ...payload } ], count: N }`, the
same per-event JSON shape as `phux watch --json`, including `asked` payloads
with `id`, `question`, `suggestions`, and nullable `elapsed_seconds`. It is an
accelerator of `phux_wait`'s poll floor, not a replacement: `phux_wait` is
still the way to block on a specific screen condition.

### 3.9 `phux_plugin_action`

Executes one action declared by an enabled configured plugin manifest.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `plugin_id` | string | yes | Configured plugin id. |
| `action_id` | string | yes | Plugin-local action id. |
| `timeout_secs` | number | no | Give up after this many seconds. Omit to wait indefinitely. |
| `cwd` | string | no | Override cwd. Relative paths resolve under the plugin root. |
| `config` | string | no | Override `config.toml` path; defaults to the normal phux config path. |

Result: the same `schema_version = 1` action result as
`phux config run --json`: `plugin_id`, `action_id`, `command`, `cwd`,
`outcome`, `exit_code`, `stdout`, `stderr`, and `duration_ms`. The runtime
executes argv directly from the plugin root; there is no hidden shell
expansion.

### 3.10 `phux_ask`

Reports that an agent in a pane is asking for human input. This is the
MCP twin of `phux ask`: it emits the same `asked` event that
`phux_watch` / `phux watch --json` observe, without writing a sentinel to
the target PTY.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | yes | Selector (see §2). |
| `id` | string | yes | Stable question id for answer correlation. |
| `question` | string | yes | Human-facing question text. |
| `suggestions` | array of string | no | Suggested answers in display order. |
| `elapsed_seconds` | number | no | Seconds the agent has already been waiting. |
| `socket` | string | no | Override the UDS path (see §2). |

Result: `{ event: "asked", terminal: "@N", id, question, suggestions,
elapsed_seconds }`, matching the CLI's `phux ask --json` projection. This is
advisory attention, not focus authority: present the payload to the human and
point them to TUI `C-a q` (next ask) / `C-a Q` (return); do not synthesize those
keys from MCP.

### 3.11 `phux_plugin_workspace`

Lists configured plugin workspace profiles. This is the workspace
composition/read half of the plugin surface: it returns the manifest-level
agents, actions, events, and pane roles that describe an agent bench. It
does not create panes by itself; agents compose the returned profile with
the existing `phux_new`, `phux_send_keys`, `phux_run`, `phux_wait`, and
`phux_plugin_action` tools.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `plugin_id` | string | no | Optional configured plugin id filter. |
| `workspace_id` | string | no | Optional plugin-local workspace id filter. |
| `config` | string | no | Override `config.toml` path; defaults to the normal phux config path. |

Result: `{ workspaces, count }`, where each item contains
`plugin_id`, `plugin_name`, `enabled`, and the serialized plugin
`workspace` profile. A filtered miss is an MCP tool error.

### 3.12 `phux_paste`

Pastes a payload into the resolved pane as one paste event. No attach, no
resize. The server picks the delivery form from the pane's live terminal
state: with bracketed paste (DEC mode 2004) on, the payload arrives wrapped
in `ESC[200~` / `ESC[201~` as a single block; otherwise the raw bytes are
delivered as if typed. A paste inserts without submitting — paste-aware
programs buffer the block until a real Enter, so follow with
`phux_send_keys` sending `Enter` to run it. Prefer this over
`phux_send_keys` for multiline or indented text, which per-character typing
would corrupt through the target's auto-indent. The CLI contract (trust
default, `--untrusted` gate) is owned by [`agents.md`](./agents.md) §2.

| Param | Type | Required | Meaning |
|---|---|---|---|
| `target` | string | yes | Selector (see §2). |
| `text` | string | yes | The payload to paste, verbatim (newlines included). |
| `untrusted` | boolean | no | Mark the payload untrusted; the pane's untrusted-paste policy (reject by default) may silently drop an unsafe payload. Default `false` — the caller vouches for content it composed. |
| `socket` | string | no | Override the UDS path (see §2). |

Result: `{ "sent": true, "pane": "<pane>", "untrusted": <bool> }`. `pane`
is the canonical direct selector (`@N` or `host/@N`). A dropped untrusted
payload still reports `sent: true` — the route was accepted; the drop is
the pane's policy.

### 3.13–3.22 Orchestration parity tools

The remaining ten strict-schema tools execute the canonical `phux` CLI with
argv (never a shell), parse its JSON or small documented text shape, cap each
string at 4096 bytes and arrays at 64 entries, cap stdout/stderr at 1 MiB/64
KiB, and kill the child on cancellation or deadline.

| Tool | CLI mapping | Required safety/shape notes |
|---|---|---|
| `phux_launch` | `phux launch --json` | Integration or `list: true`; optional exact local `target`, split, ratio, cwd, and bounded extra argv. |
| `phux_spawn` | `phux spawn --json` | Optional explicit placement (`target`, split, ratio) or satellite; target and satellite conflict. Command is argv, not shell text. |
| `phux_signal` | `phux signal` | Explicit target and signal; `interrupt`, `terminate`, and `kill` require `confirm: true`. |
| `phux_tag` | `phux tag` | `ls`/`add`/`rm`; returns a versioned projection of the CLI's tab-separated confirmation. |
| `phux_rename` | `phux rename` | Explicit current and new session names. |
| `phux_agent` | `phux agent` | `list`/`show`/`explain` use canonical JSON; `set`/`clear` parse the confirmed agent record. |
| `phux_insert_pane` | `phux insert-pane --json` | Existing pane only; no implicit spawn and no focus operation. |
| `phux_move_pane` | `phux move-pane --json` | Exact local same-session panes, bounded ratio. |
| `phux_swap_pane` | `phux swap-pane --json` | Exact local same-session panes; preserves client-local focus. |
| `phux_workspace` | `phux workspace` | `inspect`, `save`, or `restore`; bounded local paths and canonical JSON where the CLI provides it. |

Every schema in this parity table sets `additionalProperties: false`, and
handlers enforce that again before side effects. Before `phux_kill`, a caller
must display the resolved target and obtain explicit human confirmation; the
tool does not add an implicit confirmation protocol. `phux_signal` enforces
`confirm: true` for interrupt/terminate/kill in addition to that human step.
There are deliberately no MCP `take`/`give` tools:
the CLI lease belongs to the short-lived subprocess connection, so advertising
a persistent lease would be dishonest. There is no headless focus tool because
focus is client-local. `attach`, `server`, `stdio-bridge`, and `upgrade` are
interactive/daemon/operator lifecycles; `pair` and satellite registry mutation
handle credentials; plugin installation and config editing mutate local trust
configuration. Those remain intentionally outside the model-facing tool set.

### Composing the tools safely

The canonical sequence is `phux_ls` → `phux_new` → placed `phux_launch` or
`phux_spawn` → optional exact spatial edits → `phux_run`/`phux_send_keys` →
bounded `phux_wait` plus bounded `phux_watch` → surface `phux_ask` events →
re-read state. Serialize topology writes because layout metadata is
last-write-wins. No tool in this sequence moves a human's local focus, stores
remote credentials, grants a persistent input lease, or schedules future work.

---

## 4. A worked `tools/call` example

A `phux_run` against an explicit pane, target `work:1.0`:

Request (one line on stdin):

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"phux_run","arguments":{"target":"work:1.0","command":"cargo test"}}}
```

Success response:

```json
{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\n  \"command\": \"cargo test\",\n  \"exit_code\": 0,\n  \"output\": \"...\",\n  \"duration_ms\": 8123,\n  \"truncated\": false\n}"}],"isError":false}}
```

The result is a **single text content block**. A structured result is
pretty-printed JSON (here, the serialized `RunResult`); a bare error
string is shown verbatim.

A **tool** failure — no such target, no running server, a malformed
argument — is a *successful* JSON-RPC response carrying `isError: true`,
never a JSON-RPC error and never a crash. Contrast this with
**protocol-level** errors (a parse error, an unknown method, missing
`tools/call` params), which *are* JSON-RPC `error` responses.

A second example, `phux_snapshot` of a pane by opaque id with scrollback
and per-cell data:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"phux_snapshot","arguments":{"target":"@42","scrollback":200,"cells":true}}}
```

This returns the last 200 scrollback rows plus the viewport, with the
sparse per-cell `cells` array populated.

---

## 5. Relationship to the CLI

The MCP tools are name-for-name adapters over the CLI agent surface. The base
set maps `phux_ls`, `snapshot`, `send_keys`, `run`, `wait`, `new`, `kill`,
`watch`, and `ask` to their hyphenated CLI verbs. The parity table in §3 maps
launch/spawn/signal/tag/rename/agent/layout/workspace name-for-name.
`phux_plugin_action` maps to `phux config run`; `phux_plugin_workspace` reads
the same plugin manifest workspace profile. CLI-subprocess tools consume the
canonical JSON directly; in-process tools reuse the same `phux-client` or
`phux-plugin` implementation as the CLI.

Per [ADR-0022](../../ADR/0022-tool-for-agents.md), the CLI and its JSON
schema are the stable agent contract; the wire underneath stays additive
and versioned, and MCP is one thin adapter over it among several.
