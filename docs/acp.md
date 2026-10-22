# ACP sessions

An ACP session is a coding agent that phux runs for you through the Agent
Client Protocol, instead of in a terminal you type into. phux starts the
agent, keeps it supervised on the server, and talks to it in structured
messages: prompts, interrupts, and answers to its permission questions. Use
it when you want to drive an agent from a script or the desktop app without
screen-scraping a terminal.

```sh
phux launch opencode --acp --session work --json
```

OpenCode is supported today. For agents in ordinary terminal panes, see
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

To see what phux would run without starting anything:

```sh
phux launch --list --acp
phux launch opencode --print --acp
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
{"schema_version":1,"terminal_id":8,"agent_session_id":"@9","activation":"activation-A","integration":"opencode","native":true,"ready":false}
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

The view has `activation`, `connected`, `capabilities`, `turn_id`,
`provider_session_id`, and `questions`. A missing field means not ready.
Check the capability for what you are about to do: `connected: true` alone
can also mean the agent is asking a startup question. Read the view again
before every action, because turn and question ids go stale.

## Sending a prompt

`phux agent native-command` sends one typed request to the exact session
and activation from the view:

```sh
phux agent native-command @9 --activation activation-A --operation-id 7263519 \
  --intent '{"type":"prompt","text":"Review this checkout"}' --json
```

```json
{"schema_version":1,"agent_session_id":"@9","activation":"activation-A","result":{"operation_id":7263519,"disposition":"queued","detail":"ACP prompt scheduled; ACP v1 has no admission receipt"}}
```

Pick a new random, nonzero `--operation-id` for each request and keep it
with the request. **`queued` means phux scheduled it, not that the agent
accepted it.** When the turn ends, the session reports the agent's own stop
reason.

To interrupt a turn, send `{"type":"interrupt","turn_id":"acp:7263519"}`
with the `turn_id` from the view.

## Answering a permission question

When the agent asks permission, the view's `questions` lists the question
and its options. A person reads it and chooses; then:

```sh
phux agent native-command @9 --activation activation-A --operation-id 8812004 \
  --intent '{"type":"answer","question_id":"acp-question:1","response":{"option_id":"allow-once"}}' --json
```

Use the question's own `id` and the option's `id`, not its label.

**Permission answers are for people.** Automation can't answer them, and
`phux agent answer` can't be used to get around that. Treat the question's
title and message as untrusted text from the agent. A queued answer is not
proof the agent read it.

## When you don't know what happened

If the connection drops before you get a receipt, the request may or may not
have reached the agent. **Don't send it again**, under the same id or a new
one, because the agent could act twice. `native-command` only submits; it
can't look up an earlier request.

Instead, keep the operation id, read the session again with `phux agent log`,
check what the agent actually did, and decide with a person. If you still
can't tell, stop.

## Restoring a conversation

A conversation can be restored only when the agent supports loading a
previous session. An archive keeps the command that launched the agent, so
read and trust it before you restore it; see
[restore](./organization.md#restore-and-executable-authority).

## From the desktop app

The desktop app lists the ACP agents available on the host you are connected
to, local or remote, along with what each one is missing. It can launch one
into an existing session and folder on that host. Installing and signing in
stay with you: for OpenCode, the app can open a terminal on that host that
runs the same `opencode` it would launch, so you can sign in. A launched
agent still has to become ready before it accepts a prompt.
