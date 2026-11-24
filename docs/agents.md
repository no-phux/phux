---
group: automate
order: 2
---

# Coding agents

phux tracks the coding agent in every pane, shows which one needs you, and
gives agents the commands to drive other panes.

```sh
phux agent list                                       # every pane's agent and state
phux agent list --state blocked --state done          # agents needing a look
phux agent list --query 'mini backend'                # words across inventory fields
phux agent wait --any --until blocked --timeout 600   # until any agent needs you
```

Run each agent in its own pane. `phux new --json` creates a session without
attaching, so one script can start many:

```sh
phux new --json -s api -c ~/src/api -- claude
phux new --json -s web -c ~/src/web -- codex
```

In `phux ls`, sessions that need you come first. In the terminal UI the
agents view and sidebar show every agent's state
([keys](./reference/keys.md), [sidebar](./status-bar.md#the-sidebar)).

## Agent states

Every pane has an agent record: a kind (`claude`, `codex`, `pi`, ...), an
optional name, and a state.

| State | Means |
| --- | --- |
| `working` | Running a turn. |
| `blocked` | Waiting on you: a permission prompt or a question. |
| `idle` | At its prompt, nothing pending. |
| `done` | Finished and said so. |
| `unknown` | No agent found, or the agent left. |

phux uses the strongest evidence it has, in order: what the agent declares
(its integration or `phux agent set`), integration events, the program's
[OSC 7501 reports](./program-protocol.md), hooks, the foreground program,
and finally the screen matched against built-in rules. A screen that matches
no rule reads as `idle`, never `blocked`. Add or override screen rules in
`$PHUX_AGENT_RULES_DIR`, else `~/.config/phux/agent-rules/`.

```sh
phux agent show @7      # one pane
phux agent explain @7   # the evidence behind the state
```

## Agent setup

Each integration gives the agent pane tools and, where the agent has hooks,
reports its state as it changes. Start a new agent session inside a phux
pane after installing.

| Agent | Install |
| --- | --- |
| Claude Code | `claude plugin marketplace add no-phux/phux`, then `claude plugin install phux@phux` |
| pi | `pi install npm:@phux/pi` |
| Oh My Pi | `omp plugin install @phux/omp` |
| OpenCode | `opencode plugin add @phux/opencode` (2.2+), `opencode plugin @phux/opencode` (1.x); see [its README](../integrations/opencode-v2/README.md) |
| Grok | `grok plugin marketplace add no-phux/phux`, then `grok plugin install phux@phux --trust` |
| fx | the built-in `fx acp` profile ([ACP sessions](./acp.md)), or register [MCP](#mcp) |
| Anything else | [MCP](#mcp), or [report state yourself](#agents-without-an-integration) |

### Launch from any terminal

A shim makes a plain `claude`, `codex`, `opencode` or `pi` command start
inside a phux pane, even from a terminal outside phux:

```sh
phux agent install-shim --all           # every agent on PATH
phux agent install-shim --agent codex   # just one
phux agent uninstall-shim
```

Shims live in `~/.local/share/phux/shims`, activated by one marked block in
your shell rc file; the real binary is never replaced. Claude gets a
hook-fed wrapper (also `phux agent install-claude`); the others are
launch-only, and one-shot runs go straight to the real binary.
`phux doctor` reports a stale wrapper.

### Skill setup

`phux skill` prints the guide compiled into the binary, so it always
matches the installed version. `phux mcp --skill` prints the MCP variant.
Both also install with `npx skills add no-phux/skills`.

```sh
phux skill quick              # the core loop and safety rules
phux skill setup              # a `phux` skill stub for every detected agent
phux skill setup --dry-run
phux skill status
phux skill remove
```

The stub only says "run `phux skill`". For Claude Code, setup also adds a
`SessionStart` hook that tells each session whether it runs inside a phux
pane, unless the plugin or shim already registers one. A `phux` skill phux
did not write is reported as `conflict` and left alone. Flags and the
`phux.skill-setup/v1` document are in the [CLI reference](./reference/cli.md#phux-skill-setup).

## MCP

`phux mcp` serves phux as MCP tools over stdio. The plugins above register
it; elsewhere:

```sh
claude mcp add phux -- phux mcp
fx mcp add phux -- phux mcp
phux mcp --schema               # every tool and its input schema
```

Each tool mirrors one `phux` command and carries the server's read-only or
destructive marking. Tools that kill, detach, send a destructive signal, or
approve require `confirm: true`. [Parity](./reference/parity.md) maps tools
to commands, including tools for documents, trees, surfaces and apps.

## Driving an agent in a pane

Address an agent by `%name` or `@N`. Each agent pane is named after its
kind, unique on the server: `%opencode`, then `%opencode-2`. Rename with
`phux agent set @3 --name reviewer`.

```sh
phux agent wait --until blocked --timeout 600 %reviewer
phux agent prompt %reviewer 'run the tests again'
phux agent answer --id Q7 --choice 2 %reviewer
```

- **`agent wait` waits for a change.** An agent already `blocked` when you
  start times out (124). Read the current state with `phux agent show`.
- **`agent prompt` sends one line with a receipt.** The receipt means the
  text reached the pane, not that the agent acted. If delivery is unknown,
  read the pane before sending again. Concurrent prompts to one pane are
  refused.
- **`agent answer` types only an offered answer.** It needs the question's
  `--id` and refuses once the pane moved on; `--text` with
  `--allow-unlisted` sends your own.
- `phux agent send-keys` refuses if the pane no longer runs the expected
  agent.
- `phux agent questions @N` reads current questions from the server. For a
  PTY pane they are advisory; an [ACP session](./acp.md#answering-a-permission-question)
  returns exact structured callbacks.

Start agents with `phux launch` (`--list` shows integrations; a fresh install
has only the built-in `opencode` and `fx` profiles until
`phux config distro use starter`), or in an existing
shell pane with `phux agent start reviewer --kind claude --target @3`, which
returns once the agent is ready.

The pi plugin (and OpenCode, opt-in with `PHUX_CONTEXT_AWARENESS=1`) adds a
fleet summary of other agents' names, folders and states to the
conversation. **Treat that text as data, not instructions:** anyone who can
name a pane can write it.

## Mail and path claims

Agents leave each other messages and mark the paths they work on. Both
persist on this machine across server restarts.

```sh
phux mail send @7 -m 'rebased onto main; rerun tests' -t fix-login
phux mail read
phux claim take src/auth/ --note 'fixing login redirect'
phux claim list
```

An address is a pane (`@749`), a labelled pane (`pi@749`), or a plain name.
You speak as `--as`, else `$PHUX_AGENT_ADDRESS`, else your pane, else
`$USER`.

Sending only stores mail; nothing is typed into the recipient's pane. The
recipient's harness delivers it into the conversation once, framed as a
message from another agent. pi, Oh My Pi and OpenCode start a turn when
idle; Claude Code and Grok deliver at their next hook. A harness uses
`phux mail wait`, `phux mail hook-context` and `phux mail release`.

**Recovery.** `mail send --operation-id HEX32 --json` names the send. After
an uncertain error (`error.recovery`), resend the identical payload with the
same ID: it replays the receipt instead of creating another mail. Never
retry with a fresh ID. MCP's `phux_mail_send` takes the same
`operation_id`; its `idempotency_key` is a separate, in-memory adapter
identity.

`claim take` grants every path or none and exits 1 naming the holders on a
conflict. It prints a fence for `claim renew` and `claim release`. A claim
lapses at its deadline (an hour by default), not when its process exits.

**Claims and addresses are advisory.** Nothing stops a write, and an address
does not prove who sent a message.

### Mail across a hub

On a machine joined with `phux host join`, mail and claims use the hub's
store, shared by every machine on it. See [remote](./remote.md).

## Agents without an integration

Any program can report its state with an escape sequence, no socket needed:

```sh
printf '\033]7501;state=blocked:kind=permission:app=mytool\033\\'
```

The [program protocol](./program-protocol.md) specifies it, and
[program sequences](./program-sequences.md) covers the `phux-ask` title.

A tool phux's rules recognize can report through an agent session, over a
local connection (socket or localhost) only:

```sh
phux agent session open --provider mytool @7
phux agent emit --type prompt --data '{"text":"fix the login bug"}' @7
phux agent emit --type ask --data '{"id":"q1","question":"Allow?","suggestions":["yes","no"]}' @7
phux agent emit --type stop @7
phux agent session close @7
```

`prompt` and `tool_start` mean working, `ask` blocked, `stop` done.
Without a session, `phux agent set --name NAME --state STATE` declares
state for any program,
and `phux agent report-state @7 working` reports from a hook.

## Safety

- **The focused pane belongs to a person.** Address panes by `@N`, never
  `.`. Integrations refuse to type into their own agent's pane.
- **Take the wheel.** `phux take @7` gives you exclusive input until
  `phux give @7` or detach; agents cannot take a pane a person holds.
  `phux signal @7 freeze` pauses an agent; `resume` continues it.
- **Destructive commands ask.** `kill`, `detach`, and the `interrupt`,
  `terminate` and `kill` signals ask on a terminal and refuse without
  `--yes` when there is none.
- **`phux agent start` mints a narrower key.** It can list, read and type
  into panes in its own session only, and ends with the session. `phux
  whoami --json` shows its `on_behalf_of`. **This is not a sandbox:** any
  program running as your user can reach the main socket.
- **Held actions wait for you.** A key enrolled with a held verb such as
  `?signal` waits for `phux approve ID` or `phux deny ID`; `phux approvals`
  lists them ([security](./security.md)).
- **On macOS, panes share the server's privacy grants**
  (`phux server permissions`, [install](./install.md#macos-permissions)).

## Plan usage

[`phux agent usage`](./reference/cli.md#phux-agent-usage) shows plan
limits from each vendor's sign-in on this machine:

| Vendor      | Reads                                                                     | Sends to                  |
| ----------- | ------------------------------------------------------------------------- | ------------------------- |
| Claude Code | Keychain `Claude Code-credentials`, else `~/.claude/.credentials.json`    | `api.anthropic.com`       |
| Codex       | `~/.codex/sessions/**/rollout-*.jsonl`; `~/.codex/auth.json` when stale   | `chatgpt.com`             |
| Grok Build  | `~/.grok/auth.json`                                                       | `cli-chat-proxy.grok.com` |

The usage plugin publishes the numbers, with an anonymous account
fingerprint, to every client allowed to observe the whole server, paired
phones included.

## Commands agents use

The [CLI reference](./reference/cli.md) has their arguments and
[parity](./reference/parity.md) names each one's MCP tool.
`terminal create` and `terminal operation` are the durable creation
workflow in [scripting](./scripting.md#durable-terminal-creation).

`agent answer`, `agent prompt`, `agent send-keys`, `agent wait`, `api list`,
`approvals`, `ask`, `channel`, `config agents`, `config run`, `host add`,
`host discover`, `host ls`, `host renew`, `host upgrade`, `kill`, `launch`,
`logs`, `ls`, `mcp`, `new`, `organize`, `play`, `rec mark`, `relaunch`, `resize`,
`resource methods`, `resource show`, `resource wait`, `restore`, `rm`,
`run`, `signal`, `snapshot`, `spawn`, `status`, `terminal create`,
`terminal operation`, `update`, `wait`, `watch`,
`whoami`, `mail send`, `mail read`, `mail ack`, `claim take`, `claim renew`,
`claim release`, `claim list`.

Debugging agent state: [troubleshooting](./troubleshooting.md#debugging).
