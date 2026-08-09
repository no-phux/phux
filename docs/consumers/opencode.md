---
audience: humans, agents, consumers, contributors
stability: evolving
last-reviewed: 2026-08-01
---

# OpenCode integration

**TL;DR.** `@phux/opencode` adds six bounded terminal tools and cache-preserving
fleet awareness while an external local phux server owns the terminals. Targets
resolve from an explicit argument, the latest created pane, then `PHUX_TARGET`.
The plugin uses public OpenCode hooks; it does not embed a TUI, paste, or connect
to remote phux transports.

---

## Requirements

The package requires Node.js 20 or newer, OpenCode, a compatible external
`phux` executable, and a running local phux server. It does not bundle or start
phux. The current adapter expects the documented `phux 0.1.0` CLI shapes:

```sh
phux --version
```

The package is built against the public root API of `@opencode-ai/plugin`
1.18.1. Both OpenCode and this integration are pre-1.0 surfaces, so validate a
new OpenCode version before rolling it out broadly.

## Install and load

OpenCode documents two plugin-loading paths: npm package names in
`opencode.json`, and JavaScript or TypeScript modules discovered under
`.opencode/plugins/` or `~/.config/opencode/plugins/`. Use the package-name
form for a registry release:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["@phux/opencode"]
}
```

OpenCode installs npm plugins at startup. Do not also add a local shim for the
same package; local and npm plugins are separate load sources and would create
two plugin instances.

### Load from a checkout

Build the integration, install it as a dependency of the project's OpenCode
config directory, and expose it through OpenCode's documented local-plugin
directory:

```sh
cd /absolute/path/to/phux/integrations/opencode
npm ci
npm run build

cd /absolute/path/to/your/project
mkdir -p .opencode/plugins
npm install --prefix .opencode --save-exact /absolute/path/to/phux/integrations/opencode
cat > .opencode/plugins/phux.js <<'EOF'
export { default as PhuxPlugin } from "@phux/opencode"
EOF
```

The named re-export gives OpenCode one local plugin function. The project does
not need a `plugin` entry in `opencode.json` for an automatically discovered
local file.

### Load a packed artifact

Packing tests the same files that a registry release contains without claiming
that a version is published:

```sh
cd /absolute/path/to/phux/integrations/opencode
npm ci
npm pack --pack-destination /tmp

cd /absolute/path/to/your/project
mkdir -p .opencode/plugins
npm install --prefix .opencode --save-exact /tmp/phux-opencode-0.1.0.tgz
cat > .opencode/plugins/phux.js <<'EOF'
export { default as PhuxPlugin } from "@phux/opencode"
EOF
```

The packed runtime is standalone JavaScript plus declarations and the package
README. It still requires the external `phux` executable.

## Runtime configuration

The normal configuration boundary is the environment inherited by OpenCode:

```sh
PHUX_SOCKET=/absolute/path/to/phux.sock \
PHUX_TARGET=@42 \
opencode
```

`PHUX_SOCKET` chooses a non-default local Unix socket. `PHUX_TARGET` is an
optional initial target and is read when the plugin instance starts.
`PHUX_TERMINAL_ID`, normally inherited automatically inside phux, identifies
OpenCode's own pane in fleet context. Set `PHUX_CONTEXT_AWARENESS=0` to disable
that context. Restart OpenCode after changing these variables.

The public OpenCode plugin config type also accepts options alongside an npm
package entry:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": [
    ["@phux/opencode", {
      "executable": "/absolute/path/to/phux",
      "socket": "/absolute/path/to/phux.sock",
      "lifecycleTimeoutMs": 1000,
      "contextAwareness": true,
      "contextTimeoutMs": 1000
    }]
  ]
}
```

`executable` and `socket` override the CLI path and socket for that plugin
instance. `lifecycleTimeoutMs` bounds each best-effort lifecycle subprocess.
`contextAwareness` overrides the environment opt-out, and `contextTimeoutMs`
bounds each fleet refresh. Both timeouts default to 1000 ms. These options apply
to a configured npm entry. An automatically discovered local shim should use
`PATH`, `PHUX_SOCKET`, `PHUX_TARGET`, and `PHUX_CONTEXT_AWARENESS` instead.

## Automatic fleet context

For each new OpenCode user message, the plugin reads the public phux agent
inventory. The first observation is a synthetic text part appended to that new
message. Later changes append sequenced deltas; unchanged state adds nothing.
The plugin never puts live values in `experimental.chat.system.transform`, so
the static system prompt and tool definitions remain an exact provider-cache
prefix.

A checkpoint carries OpenCode's inherited Terminal id, the selected target, and
up to 64 sorted pane records: canonical Terminal/session/window identity,
agent label and kind, lifecycle state, attention, and cwd. Context is capped at
8 KiB and reports omitted panes. Values are marked as untrusted observations.
Screen rows, scrollback, titles, detector evidence, explanations, tool output,
and credentials are excluded; terminal content remains an explicit tool read.

The `experimental.session.compacting` hook gives OpenCode's compactor a current
canonical checkpoint. A successful `session.compacted` event forces another
checkpoint onto the next synthetic continue or user message. A missing server
emits one edge-filtered `unavailable` checkpoint and does not fail the user
turn. Awareness is current at new-message boundaries rather than continuously
inside one uninterrupted model/tool loop. OpenCode and this plugin are pre-1.0,
so the runtime smoke guards these experimental hooks. The shared rationale is
[ADR-0067](../../ADR/0067-cache-preserving-agent-fleet-context.md).

## The six tools

| Tool | OpenCode-facing behavior |
|---|---|
| `phux_list` | Lists sessions without changing phux focus. |
| `phux_create` | Creates a named session without attaching and selects its seed pane for this plugin instance. Its optional `command` is argv. |
| `phux_snapshot` | Reads a bounded screen projection without attaching or resizing. |
| `phux_send_keys` | Sends named or literal key items. It is not paste. |
| `phux_run` | Runs one shell command string through the phux sentinel and returns its result. |
| `phux_wait` | Waits for visible text, idleness, or indefinitely when all conditions and deadlines are omitted. |

The headless CLI owns selector syntax, command semantics, JSON shapes, and exit
codes. Use the [agent CLI guide](./agents.md) for that contract rather than
inferring a second CLI from these tool descriptions.

`until` and `idle_ms` are mutually exclusive. `timeout_seconds` is the phux
operation deadline; `local_timeout_ms` is the adapter subprocess deadline.
Short operations default the local deadline to 10 seconds. `phux_run` and
`phux_wait` have no implicit local deadline, preserving their documented
indefinite forms. OpenCode's tool abort signal is passed to every subprocess.
Snapshot, run, and wait output sent to the model is limited to the newest 200
lines and 12 KiB, with an explicit notice when the adapter or phux truncated
it.

## Target selection and concurrency

Every targeted tool resolves its pane in this exact order:

1. the tool's explicit `target` argument;
2. the seed pane selected by the latest successful `phux_create` in this
   plugin instance;
3. `PHUX_TARGET` captured at plugin startup.

The tool fails when all three are absent. It never silently uses phux focus.
An explicit target overrides both selected and environment targets.

Selection belongs to the plugin instance, not to an OpenCode session. Two
concurrent OpenCode sessions using the same instance therefore share one
mutable selected target, and concurrent creates are last-completion-wins. Use
explicit targets when sessions or tool calls can overlap.

The plugin does not lock the PTY, serialize terminal tools, reserve a prompt,
or make `snapshot` followed by input transactional. A human attach, another
agent, and `phux_send_keys` or `phux_run` can interleave. Lifecycle metadata is
queued to avoid overlapping metadata writes, but it is status, not an input
lock. Coordinate writers and prefer discrete `phux_run` calls where possible.

## Lifecycle metadata and gaps

Lifecycle reporting is best effort and uses only the public OpenCode server
plugin surface.

| Public signal | Behavior |
|---|---|
| `session.status` with `busy` | Declares identity for the current target, if not already declared there. |
| `session.status` with `idle` | Same; a turn boundary declares nothing new and writes nothing. |
| `session.idle` | Same. |
| successful `phux_create` | Declares identity for that tool's public OpenCode session. |
| `session.deleted` | Ownership-checks and clears that session's declaration. |
| plugin `dispose` | Best-effort ownership-checks and clears declarations known to this instance. |

Records use `name=opencode`, `kind=opencode`, and owner
`opencode:<public OpenCode session id>` — **identity only, never a `state`**. A
declared `state` outranks the server's own derivation for the record's whole
lifetime ([`../spec/L3.md`](../spec/L3.md) §3.7,
[ADR-0046](../../ADR/0046-server-side-agent-state-detection.md) point 8), so
reporting one would stand the shipped `rules/opencode.toml` detector down on
every pane running this plugin. The server derives `working` and `blocked` from
that manifest instead.

The record is written once per session and target, not once per event. A
whole-record write carries `state: "unknown"`, so rewriting identity at a turn
boundary would clobber the server's derivation and publish a
`working -> unknown` edge that `phux agent wait` reads as the agent departing.

Before clearing, the plugin reads the current declaration and requires its name,
kind, and owner to still match. It therefore preserves metadata replaced by
another owner.

Retry status, `session.created`, `session.error`, and unrelated events do not
invent transitions. OpenCode 1.18.1 has no documented event that distinguishes
a plugin reload from final disposal, so reload preservation is not claimed.
A process crash, `SIGKILL`, or other forced termination cannot run disposal.
Metadata failures and local deadlines do not fail terminal tools.

## Shared CLI boundary and other adapters

The source reuses the host-independent `PhuxCli` adapter maintained with the
[Pi integration](./pi.md). The OpenCode build bundles that adapter, its schema
validation, and the public OpenCode tool helper into the artifact. The packed
runtime has no dependency on `@phux/pi` or `@opencode-ai/plugin`; it still
executes the external phux CLI. This shared implementation boundary does not
make Pi target persistence, commands, or lifecycle behavior part of the
OpenCode contract.

Use [Pi](./pi.md) when Pi-native target persistence and human commands are the
needed host surface. Use [phux-mcp](./mcp.md) when a client speaks MCP over
stdio and needs that adapter's broader catalog. Those guides own their own
contracts; this page does not duplicate them.

## Human attach and current safety boundaries

To join a session, construct argv for a separate real terminal, for example:

```json
["phux", "attach", "--socket", "/absolute/path/to/phux.sock", "work"]
```

Treat this as argv, not a shell string to `eval`. The OpenCode plugin does not
execute attach, open a nested terminal, or navigate the human client. A human
attach is a live writer, not a read-only monitor, so coordinate it with agent
input.

Current boundaries are explicit:

- There is no TUI embedding and no dependency on OpenCode TUI internals.
- There is no WASM build; this is a Node adapter around an external native
  process.
- There is no paste tool. `phux_send_keys` sends key items and must not be
  presented as clipboard or bracketed-paste support.
- There is no remote pairing or remote transport configuration. The plugin
  accepts a local Unix socket, not QUIC, WebSocket, bearer-token, certificate,
  or pairing arguments.
- Never place pairing tokens, certificate material, or other remote
  credentials in tool arguments, targets, lifecycle records, attach guidance,
  or smoke output.

Package-development commands and the opt-in real smoke live in
[`../../integrations/opencode/README.md`](../../integrations/opencode/README.md).
