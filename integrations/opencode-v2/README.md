# @phux/opencode-v2

Private OpenCode V2 plugin, pinned to the public `@opencode/plugin@2.0.26`
contract. phux owns the terminals; OpenCode owns its session and permissions.

## Build and load

From a phux checkout:

```sh
cd integrations/opencode-v2
bun install --frozen-lockfile
bun run build
```

OpenCode loads bundled `index.js` for server tools and `tui.js` for the terminal
companion, not TypeScript sources. The bundles include phux's shared runtime;
the TUI entrypoint uses the host's `solid-js` peer dependency.
Configure OpenCode V2 with an absolute package directory:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": [
    {
      "package": "/absolute/path/to/phux/integrations/opencode-v2",
      "options": { "socket": "/absolute/path/to/phux.sock" }
    }
  ]
}
```

Omit `socket` to use the phux CLI's environment/default selection. `phux` must
be available on PATH, or set the plugin's `executable` option to its path.
This repository's `opencode.jsonc` already selects the local package.

The terminal companion must also be listed in global `cli.json` (`plugins`),
since the shared OpenCode service cannot know which phux pane is displaying a
session. It claims only the session visible in this TUI and clears the prior
claim on navigation or exit. This is what lets phux-mobile link a pane to its
native OpenCode view by exact `opencode:<session id>` identity:

```json
{ "plugins": ["/absolute/path/to/phux/integrations/opencode-v2"] }
```

The CLI companion does nothing outside a phux pane. The server plugin may
still supply tools globally, but **do not assume its `PHUX_TERMINAL_ID` names
the active TUI pane** when the service is shared. Server-side lifecycle is off
by default to avoid competing claims; `serverLifecycle: true` is only for a
dedicated server permanently bound to one pane without the CLI companion.
Tools need an explicit target unless the service was started inside that pane.

To move the package without the checkout, build it, run `bun pm pack`, and
install the resulting tarball in the destination project with
`bun add /absolute/path/to/phux-opencode-v2-0.1.0.tgz`. Set `plugins[].package`
to the absolute installed `node_modules/@phux/opencode-v2` directory. The package
is private; this is local artifact installation, not a public registry release.

## Tools and safety

The complete shared tool catalog is registered through OpenCode's native tool
transform API, with a native permission action for each tool name:

- Discovery: `phux_list`, `phux_panes`.
- Terminal creation: `phux_create`, `phux_spawn`.
- Shell control: `phux_run`, `phux_send_keys`, `phux_paste`.
- Observation: `phux_snapshot`, `phux_wait`.
- Agent control: `phux_agent_prompt`, `phux_agent_wait`, `phux_resource_wait`.
- Diagnostics: `phux_status`, `phux_runtime_info`.

Use direct `@N` or `host/@N` selectors. Creating a terminal selects it only for
the calling OpenCode session. Target precedence is an explicit tool target,
that session's selection, then `PHUX_TARGET`. Session deletion forgets its
selection. Output is bounded; run and wait operations default to a 30-second
phux deadline plus a 5-second local subprocess allowance. Short calls default
to 10 seconds. Native tool cancellation propagates to the CLI subprocess.

When the server itself is launched in phux, `PHUX_TERMINAL_ID` identifies its pane. Tools
refuse shell input, key injection, paste, and agent prompts into that pane;
reads are allowed. Create a sibling for shell work. Lifecycle metadata stays on
the hosting pane and never follows a selected worker. For a standalone launch,
an explicit `PHUX_TARGET` is the fixed lifecycle identity and initial tool
target; without either identity, no pane is labelled OpenCode.

Permission requests remain in OpenCode. The plugin observes permission events
but never approves them or rewrites host permission policy. Lifecycle records
contain identity and event metadata, not prompt bodies or a forced agent state.
Only one OpenCode session at a time owns the hosting pane's lifecycle stream.
The CLI companion keeps that owner in sync with the visible tab even when the
service is shared or multiple terminal clients show different sessions.

The context hook adds the parent-pane rule and fleet context. Set
`PHUX_CONTEXT_AWARENESS=0` or `contextAwareness: false` to disable fleet context;
the parent-pane rule remains. Options also include `lifecycleTimeoutMs` and
`contextTimeoutMs`.

When a sibling pane is selected, OpenCode's built-in shell runs as `phux run`
on that sibling. If the only pane is the one hosting this agent, the shell
refuses. The plugin does not replace the terminal widget or PTY API, and it
does not tunnel the OpenCode server. Use `phux attach` for a remote view.
Satellite targets work through the phux CLI and its configured local hub socket.

## Verification

```sh
bun run gates
```

The package gate typechecks, builds, then tests. Its packed-artifact regression
loads the tarball outside the checkout and checks registration against the
public SDK. Source tests cover cross-session selection/deletion, lifecycle
identity, parent write protection, and cancellation.
