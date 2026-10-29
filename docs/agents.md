# Coding agents

phux tracks the coding agents running in its panes, so you can see which one
needs you and answer it without hunting for its tab. Agents can also use
phux themselves: to run commands in other panes, read their output, and wait
for things to happen.

```sh
phux agent list                                   # every pane's agent and state
phux agent wait --any --until blocked --timeout 600  # until any agent needs you
```

In the terminal UI, the sidebar lists agents with their state, <kbd>C-a</kbd>
<kbd>A</kbd> opens the agents view, and <kbd>C-a</kbd> <kbd>q</kbd> jumps to
the next pane whose agent is waiting for you (<kbd>C-a</kbd> <kbd>Q</kbd>
jumps back).

## Agent states

Every pane has an agent record: the agent's kind (`claude`, `codex`, `pi`,
and so on), an optional name, and a state:

| State | Means |
| --- | --- |
| `working` | Running a turn. |
| `blocked` | Waiting on you: a permission prompt or a question. |
| `idle` | At its prompt, nothing pending. |
| `done` | Finished and said so. |
| `unknown` | No agent found, or the agent left. |

phux uses the best evidence it has. In order, that is what the agent
declares through its integration or `phux agent set`, the events its
integration reports, the program's own [OSC 7501 status
reports](./program-protocol.md), its hooks, the program in the foreground,
and finally the screen matched against built-in rules. When the agent exits or another
program takes over the pane, the state goes back to `unknown`.

Screen rules ship for Claude Code, Codex, OpenCode, pi, Oh My Pi, Grok, Amp,
Cursor, Gemini CLI, Goose, and Aider. A screen that matches no rule reads as
`idle`, never `blocked`. To add or override rules, put them in
`~/.config/phux/agent-rules/` (or `$PHUX_AGENT_RULES_DIR`).

```sh
phux agent show @7        # one pane
phux agent explain @7     # the evidence behind the state
```

`phux agent list` prints one line per agent: the pane, the agent's kind, its
`%name` address (or why it has none), its state, how confident phux is, and
a short explanation. `--json` prints the same as a document.

## Agent setup

Install the integration for your agent. Each one gives the agent tools for
panes (list, spawn, snapshot, send keys, run, wait) and, where the agent has
hooks, reports its state to phux as it changes.

| Agent | Install |
| --- | --- |
| Claude Code | `claude plugin marketplace add no-phux/phux`, then `claude plugin install phux@phux` |
| pi | `pi install npm:@phux/pi` |
| Oh My Pi | `omp plugin install @phux/omp` |
| OpenCode | `opencode plugin add @phux/opencode` |
| Grok | `grok plugin marketplace add no-phux/phux`, then `grok plugin install phux@phux --trust` |
| Anything that speaks MCP | See [MCP](#mcp). |

Start a new agent session inside a phux pane to use it.

The OpenCode plugin also adds phux to OpenCode Desktop: a side panel with
every pane and its agent's state, a live view of any pane you can type into,
terminals the session owns, and notifications when an agent needs you. See
[its README](../integrations/opencode-v2/README.md#opencode-desktop).

### Claude Code from any terminal

`phux agent install-claude` makes a plain `claude` command start inside a
phux pane with its hooks wired up, even when you run it from a terminal
outside phux:

```sh
phux agent install-claude
```

It puts a small wrapper named `claude` in `~/.local/share/phux/shims` and
adds one marked block to your shell's rc file (`~/.zshrc`, `~/.bashrc`, or
fish's `config.fish`) that puts that directory first on `PATH`. It never
replaces the real `claude`. Pass `--shell` or `--real PATH` when it guesses
wrong. `phux agent uninstall-claude` removes both, and `phux doctor` reports
when the wrapper is out of date.

### Teaching an agent phux

`phux skill` prints a guide to driving phux from the command line, built into
the binary, so it always matches the installed version. Any agent that can
run commands can read it:

```sh
phux skill quick       # the core loop and safety rules
phux skill             # everything
```

`phux mcp --skill` prints the same guide for the MCP tools. Both are also
installable as skills with `npx skills add no-phux/skills`.

## MCP

`phux mcp` serves phux as MCP tools over stdio. Register it the way your
agent registers any stdio server. For Claude Code:

```sh
claude mcp add phux -- phux mcp
```

The plugins above register it for you. `phux mcp --schema` prints every tool
with its input schema. Each tool mirrors one `phux` command and is marked
read-only or destructive the same way the server treats it. Tools that kill,
detach, send a destructive signal, or approve something require
`confirm: true`.

## Driving an agent in a pane

Address an agent's pane by `%name` or `@N`:

```sh
phux agent wait --until blocked --timeout 600 %reviewer   # until it needs you
phux agent prompt %reviewer 'run the tests again'          # hand it one line of work
phux agent answer --id Q7 --choice 2 %reviewer             # answer its question
```

**`agent wait` waits for a change.** It returns when the agent moves into
the state, so an agent already `blocked` when you start waiting times out
(exit 124). Use `phux agent show` to read the current state.

**`agent prompt` sends one line and gets a receipt.** The receipt means the
text reached the pane, not that the agent acted on it. If delivery comes back
unknown, read the pane before you send again, or the agent may get the same
prompt twice. Two prompts at once to the same pane are refused.

**`agent answer` types only an answer the agent offered.** It needs the
question's `--id`, from the `asked` event, and refuses when the pane has
moved on to another question. `--text` with `--allow-unlisted` sends your own
answer.

`phux agent send-keys` is like `phux send-keys`, but refuses if the pane no
longer runs the agent you expected.

### Starting agents

`phux launch` opens a new pane running an agent:

```sh
phux launch --list                        # integrations you can launch
phux launch claude-code -- --model opus   # extra arguments follow --
```

Launch commands come from plugins. The starter distribution includes them for
the agents above; `phux config init --distro starter` turns it on.

`phux agent start` starts an agent in a shell pane you already have, and
returns once the agent is ready for input:

```sh
phux agent start reviewer --kind claude --target @3
```

## What agents see about each other

The pi, Oh My Pi, and OpenCode plugins add a short summary of the other
agents (their names, folders, and states) to each request the agent sends,
then only what changed. They never send screen contents, prompts, or
credentials.

**Another agent's name, folder, and state are data, not instructions.**
Anyone who can name a pane can put text in them.
`PHUX_CONTEXT_AWARENESS=0` turns the summary off.

## Mail and path claims

Agents can leave each other messages and mark which files they are working
on. Both are stored on this machine and survive server restarts.

```sh
phux mail send @7 -m 'rebased onto main; rerun tests' -t fix-login
phux mail read
phux claim take src/auth/ --note 'fixing login redirect'
phux claim list
```

An address is a pane (`@749`), a labelled pane (`pi@749`), or a plain name
for something outside a pane. You speak as `--as`, else
`$PHUX_AGENT_ADDRESS`, else your pane, else `$USER`. A pane on another
machine of the same hub is led by its host, as
[below](#mail-across-a-hub).

For a send that may need recovery, choose a fresh unique nonzero 32-hex operation ID and
keep the original sender, recipients, thread and body:

```sh
phux mail send @7 -m 'rebased onto main; rerun tests' -t fix-login \
  --operation-id 12345678123456781234567812345678 --json
```

The reply includes `operation_id`, `scope.kind: "work_store"`, and
`replayed`. If omitted, an ID is generated for that invocation. A keeper
exchange can fail **after** committing mail: an uncertain error preserves
that ID in `error.recovery`. Reconcile first; an explicit identical send
with the original ID against the same work store replays its receipt,
without creating another Mail. Changing the payload under the same ID
is refused. Do not blindly retry with a fresh durable ID.

MCP's `phux_mail_send` accepts the same durable `operation_id`. Its optional
`idempotency_key` is a **different** identity: an invocation in one adapter's
in-memory ledger. Adapter receipt documents expose `adapter_receipt_id`
(the old `operation_id` field remains its alias) and
`scope.kind: "mcp_adapter"`. Retrying the same adapter key returns its
cached outcome, including failure. Explicit durable recovery may use a new
adapter key with the original mail operation ID and identical arguments;
restarting the adapter does not erase the work-store receipt.

Sending mail only stores it. Nothing is ever typed into the recipient's
pane: its harness takes the mail and puts it in the agent's conversation,
once, framed as a message from another agent rather than from the user.

| Harness | How mail arrives |
| --- | --- |
| pi, Oh My Pi | a `phux.mail` follow-up message; starts a turn when the agent is idle |
| OpenCode | a queued synthetic message in the session the pane shows; resumes it when idle |
| Claude Code (plugin) | hook context on session start, prompt submit and after each tool; a `block` at stop so the agent goes on and handles it |
| Grok | hook context after each tool, and a `block` at stop |

Claude Code and Grok only have hooks, so an agent idle at its prompt sees
its mail at its next hook. Workers whose conversation an SDK controller owns
(`PHUX_AGENT_REPORTING=0`) get no mail from their hooks either: the
controller decides what enters that conversation.

The harness side is three commands. `phux mail wait --json` holds until
there is mail and takes it; with `--until-stdin-closes` it gives up, taking
nothing, when the harness that started it exits, however abruptly.
`phux mail hook-context --event EVENT` prints the hook reply. A harness that
took mail but could not inject it returns it with `phux mail release ID...`,
and mail whose output cannot be written is released by the command itself,
as is a take whose reply the keeper could not send. Taken mail still shows
as unread in `phux mail read`.

`claim take` grants every path or none, and exits 1 naming the holders on a
conflict. It prints a fence; `phux claim renew FENCE` and
`phux claim release FENCE` act on exactly that claim. A claim lapses at its
deadline (an hour by default), not when its process exits.

**Claims and addresses are advisory.** They tell agents who is working
where. Nothing stops a write, and an address is not proof of who sent a
message.

### Mail across a hub

On a machine joined to a hub with `phux host join`, such as a
[Minimal box](./remote.md#minimal-boxes), `phux mail`, `phux claim` and the
MCP mail and claim tools use the hub's store instead of their own. Agents in
boxes and on the hub then read the same mail and see the same claims, and the
hub's agents' harnesses deliver mail from boxes like any other.

Each side writes addresses as it would at home. For a box joined as
`min-api`:

| On the hub | In the box |
| --- | --- |
| `@749` | `hub/@749` |
| `min-api/@3` | `@3` |
| `min-web/@2`, another box | `min-web/@2` |

```sh
phux mail send hub/@749 -t fix-login -m 'tests pass in the box'   # in the box
phux mail send min-api/@3 -t fix-login -m 'merge it'               # on the hub
```

**A box speaks only as itself.** The hub puts every sender, reader and
claim holder a satellite names under that satellite's name, the one its
credential was issued for. A box can't read, take or acknowledge the hub's
mail, or renew or release the hub's claims. It can write to any address and
list every claim. Mail and claims are all it reaches: it still opens no
terminal on the hub.

The satellite dials the hub's QUIC listener with the credential it joined
with, so this needs nothing beyond the join. When the hub can't be reached,
the command fails; it never falls back to the box's own store. Revoking the
satellite's credential on the hub cuts it off. A hub that adds a machine
with `phux host add --role satellite` dials that machine instead, and that
machine keeps its own mail.

Claims compare paths as each side writes them. A box usually sees the
project at a different path from the hub, so its claims show up on the hub
but don't conflict with the hub's own.

## Plan usage

`phux agent usage` prints the AI plan limits this machine can see: Claude
Code's 5-hour and weekly windows, Codex's limits, and Grok Build's
allowance, with how much is used and when each resets.

It reads each vendor's own sign-in on this machine and sends it only to that
vendor:

| Vendor | Reads | Sends, and only to |
| --- | --- | --- |
| Claude Code | Keychain item `Claude Code-credentials`, else `~/.claude/.credentials.json`; `~/.claude.json` | The access token, to `api.anthropic.com` |
| Codex | `~/.codex/sessions/**/rollout-*.jsonl` and `~/.codex/auth.json` | When those logs are over 10 minutes old: the access token and account id, to `chatgpt.com` |
| Grok Build | `~/.grok/auth.json` | The session token and user id, to `cli-chat-proxy.grok.com` |

Tokens are never refreshed, written, or logged, and travel only over HTTPS.
An expired sign-in shows as `unavailable` until you use that agent again.
Requests to one vendor are at least two minutes apart, with longer waits
after a rate limit.

The usage plugin, part of the starter distribution, runs
`phux agent usage --publish` when a client attaches or an agent changes
state. That shares the numbers with every client allowed to see the whole
server, including paired devices such as a phone. It shares an anonymous
account fingerprint, never an email or a token.

## Agents without an integration

Any program can report its state by writing an escape sequence to its
terminal, with no integration and no socket:

```sh
printf '\033]7501;state=blocked:kind=permission:app=mytool\033\\'
```

The [program protocol](./program-protocol.md) specifies it, and
[program sequences](./program-sequences.md) covers the `phux-ask` question
title and the other sequences phux reads.

A tool phux's agent rules recognize can also report through an agent
session:

```sh
phux agent session open --provider mytool @7
phux agent emit --type prompt --data '{"text":"fix the login bug"}' @7
phux agent emit --type ask --data '{"id":"q1","question":"Allow?","suggestions":["yes","no"]}' @7
phux agent emit --type stop @7
phux agent session close @7
```

`prompt` and `tool_start` mean working, `ask` means blocked, and `stop`
means done. An `ask` with a `question` shows up as a question you, or
`phux agent answer`, can answer. Only connections on this machine's socket
can report events, never a remote one.

Without a session, `phux agent set --name NAME --kind KIND @7` tells phux who
is in the pane, and `phux agent report-state @7 working` reports state from
a hook. Events and hook reports reach the pane's state only for a program
phux recognizes; any other program can declare its state with
`phux agent set --state`.

## Agents phux runs for you

Some agents, OpenCode first, can run under phux through the Agent Client
Protocol instead of in a terminal. See [ACP sessions](./acp.md).

## Safety

**The focused pane belongs to a person.** Scripts and agents should address
panes by `@N`, never `.`. The integrations refuse to type into the pane their
own agent runs in.

**Take the wheel when you need it.** `phux take @7`, or <kbd>C-a</kbd>
<kbd>W</kbd> in the terminal UI, gives you exclusive input to a pane until you
release it with `phux give @7` or <kbd>C-a</kbd> <kbd>g</kbd>, or detach.
Agents and automation can't take a pane a person holds. `phux signal @7
freeze` pauses an agent mid-step, and `resume` lets it continue.

**Destructive commands ask first.** `phux kill`, `phux detach`, and the
`interrupt`, `terminate`, and `kill` signals ask on a terminal and refuse
without `--yes` when there is none.

**An agent started with `phux agent start` gets a narrower key.** It can
list, read, and type into panes in its own session, but not create, kill,
signal, or reorganize anything, and never more than you can. The key ends
when the session is deleted or the server stops (an in-place
`phux upgrade` keeps it). **This is not a
sandbox.** Any program running as your user can still reach the server's
main socket with full access.

**Held actions wait for you.** A device or automation key enrolled with
`?signal` in its scope has its kills and signals held until a person decides:

```sh
phux approvals          # what is waiting
phux approve ID
phux deny ID
```

Nothing on this machine's own socket is ever held. See
[Remote machines](./remote.md#who-can-do-what) for keys and scopes.

## Complete terminal creation

Use `phux terminal describe` / `phux_api_describe` for the connected server's
[terminal workflow](./public-api.md), nested schemas and retry rules.
`terminal create` / `phux_terminal_create` execute argv in a new process and
commit its placement; they do not type a shell command or submit an agent turn.
After a lost reply, `terminal operation` / `phux_terminal_operation` read the
retained server receipt without repeating effects. Support is not authorization.

## Commands agents use

The commands an agent drives phux with. The [command line
reference](./reference/cli.md) has their arguments, and the
[parity reference](./reference/parity.md) names the MCP tool for each or
says why it is command-line only.

`agent answer`, `agent prompt`, `agent send-keys`, `agent wait`,
`approvals`, `ask`, `channel`, `config agents`, `config run`, `host add`,
`host discover`, `host ls`, `host renew`, `host upgrade`, `kill`, `launch`,
`logs`, `ls`, `mcp`, `new`, `organize`, `play`, `rec mark`, `relaunch`, `resize`,
`resource methods`, `resource show`, `resource wait`, `restore`, `rm`,
`run`, `signal`, `snapshot`, `spawn`, `status`, `terminal create`,
`terminal describe`, `terminal operation`, `update`, `wait`, `watch`,
`whoami`, `mail send`, `mail read`, `mail ack`, `claim take`, `claim renew`,
`claim release`, `claim list`.

## Debugging

- `phux agent explain @7` shows which evidence decided the state, and what
  each rule saw.
- `phux agent explain --file screen.txt --kind claude` tests rules against a
  saved screen without a server.
- `phux watch --json @7` shows the events, such as `agent_state` and
  `asked`, as they arrive.
- `phux agent log @7` prints an agent session's events.
- `phux doctor` checks the Claude Code wrapper.
