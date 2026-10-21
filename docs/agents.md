# Coding agents

Two things meet here. phux tracks the coding agents running in its panes,
so you can see which one needs you and hand it input without finding its
window. And agents use phux to run commands, read output, and wait for
things, through the same [commands scripts use](./scripting.md), an MCP
server, or a plugin for their host. Check the
[source versus installed-release boundary](./README.md#source-and-installed-releases)
before relying on a command from these pages.

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

`phux mission` projects `working`, `blocked`, and `done` into the task.
See [Work and missions](./organization.md#work-and-missions).

The state comes from the strongest source available:

1. a declaration (`phux agent set --state ...`), for the occupant it describes
2. the agent's own lifecycle stream (`phux agent emit`), when its host
   integration reports one
3. a hook report (`phux agent report-state`)
4. the pane's foreground process, which identifies the agent
5. the screen, matched against built-in rules for that agent

Confirmed departure or a changed occupant withdraws declared state to
`unknown` and clears its attention, preserving explicitly authored identity
and native-session fields. Failed process inspection is not evidence of
vacancy. Derived reporting can resume subject to identity ownership; one
source must not retract another source's ask or delete its declaration.
The [pane event arbiter and regression tests](../crates/phux-server/src/runtime/client/pane_events.rs)
own this provenance rule.

Rules ship for Claude Code, Codex, OpenCode, pi, OMP, Grok, Amp, Cursor,
Gemini CLI, Goose, and Aider. No match means `idle`, never `blocked`. Add or
override rules in `~/.config/phux/agent-rules/`. How this differs from Herdr and cmux
is in [phux vs tmux, Herdr, and cmux](./compare.md#compared-with-herdr).

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
is a single line with [bounded acknowledged-delivery semantics](./protocol.md#receipts-retries-and-backpressure);
a receipt means the PTY write completed, not that the agent acted on it.
Unknown delivery must not be resent under a new identity. A unique name is
not fresh process attestation: inspect `agent list` address refusals and use
exact ids for unattended targeting. `agent wait` fires on a transition,
so an agent that is already `blocked` when you start waiting times out.

Terminal `phux launch` needs a plugin that defines launch commands. The ones in
[`examples/plugins/agent-tools`](../examples/plugins/agent-tools) cover the
agents above; `phux config init --distro starter` enables them.
Native `phux launch opencode --acp` also has a built-in `opencode acp` profile;
see [ACP-owned sessions](./acp.md).

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

## Automatic fleet context

Host adapters using the shared [integration runtime](../integrations/runtime)
append bounded, sanitized fleet observations at the request tail, never in
the stable system prompt or tool schema. Peer-controlled labels, cwd and
state are **untrusted data, not instructions**. No terminal screens, prompt
contents or credentials are fetched automatically. The first emission is a
checkpoint, followed by changes only; compaction gets a checkpoint and
forces a fresh one for the next normal turn. Unavailability/recovery is
best-effort and must not block the model turn indefinitely.

`PHUX_CONTEXT_AWARENESS=0` disables this context where supported by the host
adapter. [Awareness code](../integrations/runtime/src/awareness.ts) and
[tests](../integrations/runtime/test/awareness.test.ts) own timeout, byte,
pane and checkpoint bounds. Host adapters remain siblings over this neutral
runtime; do not duplicate its state machine in each integration.

## Plan usage

`phux agent usage` prints the AI plan limits this host can see: Claude Code's
5-hour and weekly windows, Codex's, and Grok Build's allowance, with
utilization and reset times.

What it reads, and where anything goes:

| Vendor | Read on this host | Sent, and only to |
| --- | --- | --- |
| Claude Code | Keychain item `Claude Code-credentials` (via `/usr/bin/security`), else `~/.claude/.credentials.json`; account id from `~/.claude.json` | the access token, to `api.anthropic.com/api/oauth/usage` |
| Codex | `~/.codex/sessions/**/rollout-*.jsonl` (the newest tails) and `~/.codex/auth.json` | when the ledgers are over 10 minutes old: the access token and account id, to `chatgpt.com/backend-api/wham/usage` |
| Grok Build | `~/.grok/auth.json` | the session token and user id, to `cli-chat-proxy.grok.com` (an override must be `https://`) |

Tokens are never refreshed, written, logged, or published, and travel only
over HTTPS (`curl` reads them from stdin, not its command line). An expired
sign-in shows as `unavailable` until that agent runs again. Requests to one
vendor are at least two minutes apart and back off 3, 6, 12, then 15 minutes
on rate limits; a missing Keychain item is not looked for again for 15
minutes.

The [usage plugin](../examples/plugins/usage) (enabled by the starter
distro) runs `phux agent usage --publish` when a client attaches or an agent
changes state. A publish that finds another one running, or that comes
within ten seconds of the last, exits at once without touching the server
or any credential. It writes the `phux.usage/v1` Global metadata key, the
same document as `phux agent usage --json`: per plan a `provider`, `plan`,
`label`, an opaque 12-hex `account` fingerprint (never an email), `status`
(`ok`, `stale`, `unavailable`, `error`), and `windows` of `{id, minutes,
used_percent, resets_at_ms}`. The key is rewritten only when a number or
status changes, or every five minutes. Any client whose grant includes
`observe@global` can read it: the owner socket, and every paired device.
Readers ignore unknown fields.

Setting `phux.usage.refresh/v1` to a new value is only a hint: nothing
watches it, so it is noticed by the next publish a hook triggers, which then
skips the two-minute spacing (never a backoff). The plugin's `refresh`
action publishes at once. To stop publishing, see the
[plugin's README](../examples/plugins/usage/README.md#turning-it-off).
[Collector](../crates/phux-usage) and [verb](../crates/phux/src/commands/plan_usage.rs)
own the sources and the publish rule.

## ACP-owned sessions

Agent Client Protocol (ACP) providers run under a server-owned guardian,
not through terminal key/paste integration. The [ACP guide](./acp.md) owns
provider setup, readiness, typed commands, human permissions and uncertain
outcomes. Start there before launching a provider or resuming a conversation.

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
`phux agent answer`) can answer. Generic appends require permitted local
producer ingress and BIND+INPUT through the parent; they are not exclusive
to the connection that opened the stream. Same-owner socket clients share
that trust boundary. SDK-owned streams instead require their reserved
native controller; generic emit cannot impersonate one. See the
[append guard](../crates/phux-server/src/runtime/resource_commands.rs) and
[parent authorization](../crates/phux-server/src/policy/enforce.rs).
Native ACP questions use the separate human-only broker described above.

Without a stream, `phux agent set --name NAME --kind KIND @7` at least tells
phux who is in the pane, and `phux agent report-state` can set the state
from a hook.

## Mail and path claims (`schema` `phux.coord/v1`)

Agent coordination in the host's work store
([ADR-0187](https://github.com/no-phux/phux/blob/3cd9ddac22075c1b3db2674e2edc4e00c91d67f4/docs/adr/0187-agents-coordinate-through-phux.md)). An address is a
pane (`@749`), a labelled pane (`pi@749`, keyed by the pane), or a bare name
for something outside a pane; you speak as `--as`, else
`$PHUX_AGENT_ADDRESS`, else your pane, else `$USER`. These are host-local
advisory addresses/claims, not authenticated execution identity or filesystem
enforcement. [Claim transitions](../crates/phux-work/src/coord/claims.rs) and
the [keeper protocol](../crates/phux-work/src/wire.rs) own expiry and stale
keeper replacement; process liveness alone never authorizes stealing a lease.

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

## Delegated execution identity

`agent start` can mint a narrowed session credential and a separate local
capability socket, inherited through `PHUX_SOCKET`. It grants at most
Inventory, Observe, Input and Bind on the launcher's session, never Create,
Organize, Terminate or Signal, and never more than the launcher holds.
It can take a free lease but not one a human holds. This is not a same-UID
sandbox: the ordinary owner socket is still reachable by that user's
processes. A pane label, `--as` coordination address or path-claim holder is
advisory identity, not this authenticated principal.

Hot handoff carries live credentials and bound sockets; cold restart carries
none. Session deletion, explicit withdrawal or expiry revokes the socket
and its live authority. The [grant ledger](../crates/phux-server/src/delegation.rs)
and [socket lifecycle](../crates/phux-server/src/runtime/delegation.rs) own
these rules. Do not manufacture credentials or adopt stale fences to work
around a refusal.

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
Connections over the ordinary owner socket are never held. A delegated
local socket is not that full-owner path; see
[connection authority](./remote.md#connection-authority).
