# Coding agents

Two things meet here. phux tracks the coding agents running in its panes,
so you can see which one needs you and hand it input without finding its
window. And agents use phux to run commands, read output, and wait for
things, through the same [commands scripts use](./scripting.md), an MCP
server, or a plugin for their host.

## What phux knows about an agent

For every pane, phux keeps an agent record: the agent's `kind` (`claude`,
`codex`, `pi`, ...), an optional `name`, and a `state`:

| State     | Means                                                |
| --------- | ---------------------------------------------------- |
| `working` | running a turn                                       |
| `blocked` | waiting on a person: a permission prompt, a question |
| `idle`    | at its prompt, nothing pending                       |
| `done`    | finished and said so                                 |
| `unknown` | no agent found, or the agent left                    |

The state comes from the strongest source available:

1. a declaration (`phux agent set --state ...`), until it is cleared
2. the agent's own lifecycle stream (`phux agent emit`), when its host
   integration reports one
3. a hook report (`phux agent report-state`)
4. the pane's foreground process, which identifies the agent
5. the screen, matched against built-in rules for that agent

Rules ship for Claude Code, Codex, OpenCode, pi, OMP, Grok, Amp, Cursor,
Gemini CLI, Goose, and Aider. No match means `idle`, never `blocked`. Add or
override rules in `~/.config/phux/agent-rules/`.

```sh
phux agent list                 # every pane's agent and state
phux agent show @7              # one pane, with the evidence
phux agent explain @7           # why the detector decided what it did
```

In the TUI, the sidebar lists agents with their state, `Ctrl-A A` opens the
fleet view, and `Ctrl-A q` jumps to the next pane whose agent is waiting
for you.

## Talking to an agent in a pane

```sh
phux agent wait --until blocked --timeout 600 %reviewer   # block until it needs you
phux agent prompt %reviewer 'run the tests again'          # one acknowledged line
phux agent answer --id Q7 --choice 2 %reviewer             # answer a published question
phux agent start reviewer --kind claude --target @3        # type the launch line into a shell
phux launch claude-code -- --model opus                    # new pane running an agent
```

`%name` addresses the pane whose agent record has that name. `agent prompt`
is a single line, delivered once; a receipt means the text reached the
pane, not that the agent acted on it. `agent wait` fires on a transition,
so an agent that is already `blocked` when you start waiting times out.

`phux launch` needs a plugin that defines launch commands. The ones in
[`examples/plugins/agent-tools`](../examples/plugins/agent-tools) cover the
agents above; `phux config init --distro starter` enables them.

## Connecting your agent host

Each integration gives the agent tools for panes (list, spawn, snapshot,
send keys, run, wait) and, where the host has hooks, reports the agent's
own state back to phux.

| Host              | Install                                                                                 |
| ----------------- | --------------------------------------------------------------------------------------- |
| Claude Code       | `claude plugin marketplace add no-phux/phux` then `claude plugin install phux@phux`     |
| pi                | `pi install npm:@phux/pi`                                                               |
| Oh My Pi          | `omp plugin install @phux/omp`                                                          |
| OpenCode          | `opencode plugin add @phux/opencode`                                                    |
| Grok              | `grok plugin marketplace add no-phux/phux` then `grok plugin install phux@phux --trust` |
| anything with MCP | see [MCP](#mcp)                                                                         |

Each package's README under [`integrations/`](../integrations) lists its
tools and settings.

For Claude Code there is also `phux agent install-claude`. It puts a shim
ahead of `claude` on your `PATH` so that running `claude` from a plain
terminal opens it inside a phux pane with its hooks wired up.
`phux agent uninstall-claude` removes it.

To make an agent read the manual, `phux skill` prints a guide to operating
phux from the CLI, and `phux mcp --skill` prints one for the MCP tools. The
same guides are installable as skills with `npx skills add no-phux/skills`.

## JSON index

Agent-facing leaf verbs (the parity test reads this section independently
of the MCP table):

`agent answer`, `agent prompt`, `agent send-keys`, `agent wait`,
`approvals`, `ask`, `channel`, `config agents`, `config run`, `host add`,
`host ls`, `host renew`, `host upgrade`, `kill`, `launch`, `logs`, `ls`,
`mcp`, `new`, `organize`, `play`, `rec mark`, `relaunch`, `resize`,
`resource methods`, `resource show`, `resource wait`, `restore`, `rm`,
`run`, `signal`, `snapshot`, `spawn`, `status`, `update`, `wait`, `watch`,
`whoami`, `mail send`, `mail read`, `mail ack`, `claim take`, `claim renew`,
`claim release`, `claim list`.

The [CLI reference](./reference/cli.md) defines their arguments; the
[parity reference](./reference/parity.md) maps them to tools or explains
why a verb is CLI-only.

## MCP

`phux mcp` serves the CLI surface as MCP tools over stdio. Register it the
way your host registers any stdio server; for Claude Code that is
`claude mcp add phux -- phux mcp`, and the plugins above register it for
you.

[`phux mcp --schema`](./reference/parity.md) prints the tool catalog with
input schemas. Each tool mirrors one `phux` command and carries the same
read-only and destructive hints the server enforces. Tools that kill,
detach, send a destructive signal, or approve something take
`confirm: true`.

## Reporting state from your own tool

If you are writing an integration for a host not listed above, report
through a session stream:

```sh
phux agent session open --provider mytool @7
phux agent emit --type prompt --data '{"text":"..."}' @7
phux agent emit --type ask --data '{"id":"q1","question":"Allow?","suggestions":["yes","no"]}' @7
phux agent emit --type stop @7
phux agent session close @7
```

Record types are `session_start`, `prompt`, `tool_start`, `tool_end`,
`notification`, `ask`, `stop`, `session_end`, `state`, and `provider_raw`.
`prompt` and `tool_start` mean working, `ask` means blocked, `stop` means
done. An `ask` with a `question` also shows up as a question the user (or
`phux agent answer`) can answer. Only the client that opened the session may
append to it.

Without a stream, `phux agent set --name NAME --kind KIND @7` at least tells
phux who is in the pane, and `phux agent report-state` can set the state
from a hook.

## Mail and path claims (`schema` `phux.coord/v1`)

Agent coordination in the host's work store
([ADR-0187](https://github.com/no-phux/phux/blob/3cd9ddac22075c1b3db2674e2edc4e00c91d67f4/docs/adr/0187-agents-coordinate-through-phux.md)). An address is a
pane (`@749`), a labelled pane (`pi@749`, keyed by the pane), or a bare name
for something outside a pane; you speak as `--as`, else
`$PHUX_AGENT_ADDRESS`, else your pane, else `$USER`.

`mail send` returns `{schema, mail, bells}`: `mail` is
`{id, thread, from, to, body, created_at, read_at, acked_at}` and each bell is
`{to, outcome}` with outcome `rung` or `not rung: REASON`. Mail is stored
before any bell rings; a pane with no agent record is never rung. `mail read`
returns `{schema, reader, mail: [...]}` oldest first and marks what it returns
read unless `--peek`; with `--thread` it includes what the reader sent there.
`mail ack` returns `{schema, mail}`; a message not sent to the reader is
`mail_not_found`, exit 1.

`claim take` returns `{schema, holder, claim: {outcome, claims}}` with
`outcome` `granted` or `conflict`; a conflict exits 1 and takes nothing.
Each claim is `{path, holder, exclusive, fence, note, expires_at}`
(`expires_at` in Unix seconds). Each atomic acquisition shares one `fence`
across its path set. Taking again never adopts or renews an existing lease,
even from the same pane. `claim renew FENCE` returns `{schema, holder, claims}`
and extends only that live acquisition, preserving its paths and sharing
mode; a stale or foreign receipt is `claim_not_found`, exit 1.
`claim release FENCE` returns `{schema, holder, released}` and removes only
that acquisition. Repeated or stale cleanup returns zero, never removing a
replacement. There is no path-only or release-all mutation.
`claim list` returns `{schema, claims}`. A worker exit is not lease expiry;
handoff requires stopping writes, releasing the exact old receipt, then
acquiring a fresh fence. Do not silently upgrade a stale receipt to a newly
listed fence.

## Safety

**The focused pane is a person's.** Scripts and agents should address panes
by id, never `.`, and the integration packages refuse to type into the pane
their own host is running in.

**Input leases.** `phux take @7` gives the caller exclusive input to a pane;
`phux give @7` returns it. A lease lasts as long as the connection that
took it, so from the TUI `Ctrl-A W` holds one until you detach or press
`Ctrl-A g`, while a one-shot `phux take` releases when the command exits.
An agent or automation principal cannot take a lease a human holds.

**Confirmation.** `phux kill`, `phux detach`, and the destructive `phux signal`
variants ask on a terminal and refuse without `--yes` when there is none,
so a script has to say what it means.

**Approvals.** Connections authenticated with a scoped workload credential
(see [remote](./remote.md)) can be made to hold kill and signal actions for
a person to approve: `phux approvals`, `phux approve ID`, `phux deny ID`.
Connections over the local socket run with the owner's authority and are
never held.
