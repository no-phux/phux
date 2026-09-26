# @phux/opencode-v2

OpenCode V2 plugin. phux owns the PTY. OpenCode owns the session. This package
is the join, not a tunnel.

## The seam

```text
phux Terminal                 the only PTY
  └── AgentSession            provider=opencode, native_id=<OpenCode session id>
        this plugin emits prompt / tool / ask / stop
        the transcript and permissions stay in OpenCode
```

If `opencode` is the process in a pane, that pane is the TUI. Typing into it
injects keystrokes into the UI. Shell work belongs on a sibling Terminal.
`phux_run` and `phux_send_keys` refuse `PHUX_TERMINAL_ID`. `phux_snapshot` and
`phux_wait` may read it. `phux_create` selects the sibling.

OpenCode's built-in terminal and `/api/pty` are a second PTY. This plugin does
not call them and does not retarget the TUI widget. There is no plugin hook
for that. Leave the widget unused.

A remote view is `phux attach`, including `host/@N` through a hub. This plugin
does not dial QUIC or WebSocket and does not forward OpenCode's HTTP API. Point
`PHUX_SOCKET` at a local hub socket if targets should be `host/@N`. The plugin
still speaks the phux CLI, not a second transport.

## What it registers

| Surface | Behavior |
|---|---|
| `phux_list`, `phux_create`, `phux_snapshot`, `phux_send_keys`, `phux_run`, `phux_wait` | Same six tools as the V1 adapter, plus the parent-pane write refusal. |
| `session` `context` hook | Stable parent-pane rule, then the cache-preserving fleet suffix. |
| tool `execute.before` / `execute.after` | `tool_start` / `tool_end` on the AgentSession. |
| `session.status`, `session.idle`, `permission.asked`, `session.deleted` | Producer records. Identity only, never a declared `state`. |

Prompt text stays off the agent stream. A `prompt` record carries a length. A
tool record carries a name.

## Load

OpenCode imports `index.js`, not the TypeScript source. `bun run build` refreshes it.

```sh
cd integrations/opencode-v2
bun install
bun run build
```

This checkout's `opencode.jsonc` loads `./integrations/opencode-v2`. From another project:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": [
    {
      "package": "/absolute/path/to/phux/integrations/opencode-v2",
      "options": {
        "socket": "/absolute/path/to/phux.sock"
      }
    }
  ]
}
```

Run it inside a phux pane so `PHUX_TERMINAL_ID` is set, or set `PHUX_TARGET` to
a sibling before the first write. `PHUX_CONTEXT_AWARENESS=0` disables the fleet
suffix. The parent-pane rule stays.

Options match the V1 adapter: `executable`, `socket`, `lifecycleTimeoutMs`,
`contextAwareness`, `contextTimeoutMs`.

## Not in this cut

- Replacing OpenCode's `shell` tool execute. The descriptions tell the model
  not to use it. Swapping the executor is a later change.
- A TUI panel that attaches as a viewer. Writes would still go through the
  tools, under the input lease.
- Publishing. The package is private until the V2 contract is the one we ship.

## Check

```sh
cd integrations/opencode-v2
bun install
bun run gates
```
