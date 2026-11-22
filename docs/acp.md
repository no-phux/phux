# ACP sessions

An ACP session is a coding agent that phux runs for you through the Agent
Client Protocol, instead of in a terminal you type into. phux starts the
agent, keeps it supervised on the server, and talks to it in structured
messages: prompts, interrupts, and answers to its permission questions. Use
it when you want to drive an agent from a script without
screen-scraping a terminal.

```sh
phux launch opencode --acp --session work --json
```

OpenCode and fx are supported today. For agents in ordinary terminal panes, see
[Coding agents](./agents.md).

## Before you start

Install the agent and sign in to it on the machine that runs the server.
phux never downloads an agent, never signs in for you, and never falls back
to typing into a terminal when ACP isn't available.

For OpenCode, phux has a built-in profile that runs `opencode acp`. It looks
for `opencode` on your `PATH`, then at `~/.opencode/bin/opencode`, and never
starts a login shell or changes `PATH`. An enabled plugin that declares its
own OpenCode profile replaces the built-in one. A profile that is invalid or
ambiguous is refused rather than skipped.

fx has a built-in profile that runs `fx acp` from the host's `PATH` in the
selected workspace. Install fx and connect a provider on that host first.
As with OpenCode, a configured `fx` profile takes precedence, including when
that profile is invalid. phux does not read or copy fx credentials. See the
[fx ACP guide](https://fx.sh/docs/using-fx/acp) for its session and permission
semantics. fx's profile-level MCP servers are not inherited by ACP sessions
unless the ACP client opts in; the `fx mcp add` command below applies to
terminal fx, not automatically to this built-in ACP profile.

To see what phux would run without starting anything:

```sh
phux launch --list --acp
phux launch opencode --print --acp
phux launch fx --print --acp
```

A plugin declares an ACP agent as a list of arguments, never a shell string:

```toml
[acp]
provider = "opencode"
command = ["opencode", "acp"]
required_executables = ["opencode"]
working_directory = "workspace"
```

## Starting a session

`phux launch --acp` starts the agent in an existing session:

```text
$ phux launch opencode --acp --session work --json
{
  "activation": "906237cf1ef22ebb16be4073a3eae502",
  "agent_session_id": "@9",
  "integration": "opencode",
  "native": true,
  "ready": false,
  "schema_version": 1,
  "terminal_id": 8
}
```

Keep `agent_session_id`; every later command uses it, not `terminal_id`.
`ready: false` means the agent was started, not that it is ready for a
prompt. The server must support ACP sessions; an older one refuses.

## Reading the session

`phux agent log --json` prints the session's records. The latest `view`
record says what the agent can do right now. With `jq`:

```sh
phux agent log --json @9 > log.json
jq '[.records[]
     | select(.type == "provider_raw"
              and .data.schema == "phux.native-agent/v1"
              and .data.update.type == "view")
     | .data.update.view] | last' log.json
```

The view has `activation`, `backend`, `connected`, `capabilities`,
`turn_id`, `provider_session_id`, and `questions`. A missing field means
not ready. Check the capability for what you are about to do:
`connected: true` alone can also mean the agent is asking a startup
question. Read the view again before every action, because turn and
question ids go stale.

## Sending a prompt

`phux agent native-command` sends one typed request to the exact session
and activation from the view:

```sh
phux agent native-command @9 --activation 906237cf1ef22ebb16be4073a3eae502 --operation-id 7263519 \
  --intent '{"type":"prompt","text":"Review this checkout"}' --json
```

```json
{
  "activation": "906237cf1ef22ebb16be4073a3eae502",
  "agent_session_id": "@9",
  "result": {
    "detail": "ACP prompt scheduled; ACP v1 has no admission receipt",
    "disposition": "queued",
    "operation_id": 7263519
  },
  "schema_version": 1
}
```

Pick a new random, nonzero `--operation-id` for each request and keep it
with the request. **`queued` means phux scheduled it, not that the agent
accepted it.** When the turn ends, the session reports the agent's own stop
reason.

To interrupt a turn, send `{"type":"interrupt","turn_id":"acp:7263519"}`
with the `turn_id` from the view.

## Answering a permission question

Read the **current** questions directly, even on a fresh connection:

```sh
phux agent questions @9
```

Use the `AgentSession` handle, not the guardian pane. A response carries the
whole `reference` returned for that question. The authority, durable
conversation key, controller activation, callback identity, and revision
must all match. Reconnect-safe reads can use `AUTHORITY/^RESOURCE` from the
returned document instead of a saved `@N`.

A person reads the question and explicitly chooses an offered option id,
not its label. Construct the response with a fresh random nonzero unsigned
64-bit operation id, encoded as a **decimal string**:

```sh
phux agent respond-question --request-json "$answer"
```

`answer` is a JSON object with `question` (the full `reference`), `operation_id`
and `response` (e.g. `{"option_id":"deny-once"}`). Save the exact reference
and operation id. After a lost reply, use a read-only lookup:

```sh
phux agent question-receipt --question-json "$reference" --operation-id "$operation_id"
```

No retained receipt means `unknown`, not “nothing happened.” Receipts are
bounded by the controller ledger, not a durable cold-restart history. Do not
resend an uncertain answer. A queued controller response is not proof the
provider read it.

**Structured responses are for human principals.** The operation still
requires observe and input authority on the conversation and respects the
guardian's input lease. The MCP tool `phux_agent_questions` only reads.
Treat all question text as untrusted agent content. PTY ask markers and hook
reports appear as advisory evidence on the pane itself; they are not SDK
callbacks and never turn into a structured response or a permission grant.

## When you don't know what happened

If the connection drops before you get a receipt, the request may or may not
have reached the agent. **Don't send it again**, under the same id or a new
one, because the agent could act twice. The legacy `native-command` only
submits; it can't look up an earlier request. For structured responses, use
the exact question commands above.

Instead, keep the operation id, read the session again with `phux agent log`,
check what the agent actually did, and decide with a person. If you still
can't tell, stop.

## Restoring a conversation

A conversation can be restored only when the agent supports loading a
previous session. An archive keeps the command that launched the agent, so
read and trust it before you restore it; see
[restore](./organization.md#restore-and-executable-authority).

## Native agent wire

Native onboarding is additive inside `NATIVE_AGENT`, gated by
`features_ext.NATIVE_AGENT_LAUNCH`.

`features_ext.CURRENT_QUESTIONS` (`0x10000000`) gates the native carrier's
`current_questions`, `respond_question` and `question_receipt` JSON request
types. Current reads project the server-owned live callback inventory or
advisory PTY evidence. Structured responses are authority-, conversation-,
activation- and revision-fenced; neither advisory evidence nor a queued
receipt attests to provider consumption. Older clients keep their existing
requests; new clients refuse older servers before writing a question request.
