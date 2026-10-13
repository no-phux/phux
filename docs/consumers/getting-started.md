---
audience: humans, agents
stability: evolving
last-reviewed: 2026-10-13
---

# Get started with a coding agent

**TL;DR.** Connect your agent host to a running local phux server and verify
a read before allowing input. The host owns its model, credentials,
permissions, and conversation. Terminal tools do not require a lifecycle
integration.

## Choose your route

Skills teach an agent how to operate phux; tools give it a connection to your
server. Installing a skill alone does not register tools or start a terminal.
If your host already exposes native `phux_*` tools, use those with
`using-phux-tools`. For terminal control over MCP use `using-phux-mcp`; for
CLI-only hosts use `using-phux`. Install the product skills with:

```sh
npx skills add no-phux/skills
```

When the installed CLI differs from the online guide, `phux --skill` prints
its version-matched operating instructions. Use a normal shell tool for an
independent command; use phux when an interactive program or terminal state
needs to persist across steps.

| You use | Start here | What you get |
|---|---|---|
| Claude Code | [Claude plugin first run](./claude.md#first-shared-terminal-walkthrough) | Native MCP tools and identity/attention hooks. An optional launch shim has a separate job. |
| Grok | [Grok plugin first run](./grok.md#first-shared-terminal-walkthrough) | The same MCP tools and lifecycle hooks for Grok Build and the `grok` bot. |
| Pi | [pi + phux in 60 seconds](./pi-in-60-seconds.md), then [Pi first run](./pi.md#first-shared-terminal-walkthrough) | A pane chooser, saved targets, terminal tools, and fleet context. |
| Oh My Pi | [Native OMP setup](./omp.md#install-and-load) | Native terminal tools, branch-local targets, and lifecycle reporting for its hosting pane. |
| OpenCode V2 | [OpenCode checkout setup](./opencode-v2.md#first-shared-terminal-walkthrough) | A source-loaded plugin that works in sibling terminals. Not a published package. |
| Another MCP-capable host | [MCP registration and first read](./mcp.md#registering-with-a-host) | Stdio tools from the installed, version-matched adapter. |
| A shell script or custom tool | [First script below](#first-script) | The same terminals through CLI commands and versioned JSON. |
| Your own harness | [Harness author guide](./harness.md) | Emit lifecycle records; this is integration development, not agent setup. |

## Let your agent read the docs

Give your agent https://docs.phux.sh/llms.txt to find the guide for your task.
Every published documentation page also has a `.md` URL, such as
https://docs.phux.sh/consumers/getting-started.md. Use **Copy page for agent**
in the documentation header for the complete page, without navigation chrome.

For searchable documentation tools, connect the hosted **docs MCP**:

```sh
# Claude Code
claude mcp add --transport http phux-docs https://phux.sh/mcp

# Codex
codex mcp add phux-docs --url https://phux.sh/mcp
```

Other Streamable HTTP clients use `https://phux.sh/mcp` as the server URL.
The tools are `search_docs`, `get_page`, `get_install_command`, and
`latest_release`. No phux account or local server is needed.

This is a read-only documentation service. It cannot see your terminals,
send keystrokes, or install software for you. To control a terminal, follow
your host’s route above or register the separate
[local terminal MCP adapter](./mcp.md#registering-with-a-host).

## Before connecting

1. [Install phux](../INSTALL.md), then complete the [local quickstart](../QUICKSTART.md).
   You should be able to detach and reattach before adding another tool.
2. Install and authenticate your chosen agent host separately. The host-specific
   guide lists its package and minimum requirements; phux does not provide model access.
3. Leave a phux session running at an idle shell prompt. Keep the agent's own
   interactive UI in a different pane or terminal from the shell it will drive.
4. From the environment that will launch the host, run:

   ```sh
   phux --version
   phux status
   phux ls
   ```

   **Expected:** a version, a running server, and the session you just created.
   If the session is missing, compare the host's socket/profile with the terminal
   where phux works; see [wrong-server recovery](../troubleshooting.md#the-server-is-running-but-my-session-is-missing).

Terminal tools and AgentSession lifecycle records are different capabilities.
An older server may support the former without the latter. For capability
checks and safe fallback, use the [agent CLI applicability section](./agents.md#this-tree-older-releases-two-agent-surfaces).
Do not upgrade to `next` merely to complete a basic snapshot.

## First script

With the CLI installed and an idle shell pane running, inventory and read
it from a second terminal:

```sh
phux ls --json
phux snapshot .
```

For this first run, `.` means the focused pane. If anyone may change focus,
replace it in every command with the exact `@N` pane selector from inventory.
Confirm the snapshot is the shell you intend to control, then:

```sh
phux send-keys . "printf '%s\n' agent-ready | tr a-z A-Z" Enter
phux wait --until "AGENT-READY" --timeout 10 .
phux snapshot --json --scrollback 50 .
```

**Expected:** `AGENT-READY` appears in the human view and the snapshot, and the
wait succeeds. The uppercase marker is not in the typed command, so its echo
alone cannot satisfy the wait. On timeout, read the pane again; do not blindly
send the command twice. For a shell command's output and exit code, use
[`phux run` and its shell-safety rules](./agents.md#2-the-loop).

The [full agent CLI guide](./agents.md) owns selectors, paste versus submit,
timeouts, JSON, supervision, and destructive-action rules. `phux --skill`
prints the operating guide matched to your installed binary.

## Share control deliberately

Human and agent keystrokes can interleave. Agree who is typing before
using an existing pane, and inspect the target again after a handoff.

To leave the human view without stopping work, press `Ctrl-A`, release both
keys, then `d`. Quit the agent normally when you want to end its conversation;
detaching alone does not stop it. Disconnecting an MCP host does not kill the
phux server or its panes. Do not use `phux kill` as a generic disconnect button.

## Recover and continue

- **No tools:** check the chosen host's integration installation and registration,
  then restart that host so it loads the package or MCP configuration.
- **Tools exist but cannot connect:** follow [agent connection recovery](../troubleshooting.md#an-agent-or-mcp-host-cannot-see-the-server).
- **Wrong or stale target:** inventory again and choose a current pane; never
  replace a rejected target with `.` just to make a write succeed.
- **Remote work:** first [enroll and attach the remote host](../remote-access.md).
  Then follow the integration's documented socket/remote limits; a local adapter
  is not automatically a network client.
