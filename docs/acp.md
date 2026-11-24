---
group: automate
order: 3
---

# ACP sessions

An ACP session is a coding agent phux runs on the server through the Agent
Client Protocol and drives with structured messages (prompts, interrupts,
permission answers) instead of a terminal. OpenCode and fx are supported.

```sh
phux new --json -s work            # launch needs an existing session
phux launch opencode --acp --session work --json
```

## Before you start

Install the agent and sign in on the machine that runs the server. phux
never downloads an agent, signs in, or falls back to a terminal.

- **OpenCode:** the built-in profile runs `opencode acp`, found on `PATH`
  or at `~/.opencode/bin/opencode`.
- **fx:** the built-in profile runs `fx acp` from `PATH` in the workspace
  ([fx ACP guide](https://fx.sh/docs/using-fx/acp)). Terminal fx's
  `fx mcp add` servers do not apply to it.

A plugin's profile for the same provider replaces the built-in one, and an
invalid or ambiguous profile is refused, not skipped. A profile is argv,
never a shell string:

```toml
[acp]
provider = "opencode"
command = ["opencode", "acp"]
required_executables = ["opencode"]
working_directory = "workspace"
```

```sh
phux launch --list --acp            # what can launch
phux launch opencode --print --acp  # what would run
```

## Starting and reading a session

```text
$ phux launch opencode --acp --session work --json
{"activation":"906237cf1ef22ebb16be4073a3eae502","agent_session_id":"@9","integration":"opencode","native":true,"ready":false,"schema_version":1,"terminal_id":8}
```

Use `agent_session_id` from here on, not `terminal_id`. `ready: false` means
started, not ready for a prompt.

The latest `view` record in `phux agent log --json` says what the agent can
do now:

```sh
phux agent log --json @9 | jq '[.records[]
  | select(.type == "provider_raw" and .data.schema == "phux.native-agent/v1"
           and .data.update.type == "view")
  | .data.update.view] | last'
```

It carries `activation`, `connected`, `capabilities`, `turn_id`, and
`questions`. Check the capability you need; `connected: true` can also mean a
startup question. Reread before every action: turn and question ids go stale.

## Sending a prompt

```sh
phux agent native-command @9 --activation 906237cf1ef22ebb16be4073a3eae502 \
  --operation-id 7263519 --intent '{"type":"prompt","text":"Review this checkout"}' --json
```

Use a fresh random nonzero `--operation-id` per request. **`queued` means
phux scheduled it, not that the agent accepted it.** Interrupt with
`{"type":"interrupt","turn_id":"acp:7263519"}`, using the view's `turn_id`.

**If the reply is lost, do not send again**, with either id: the agent could
act twice. `native-command` cannot look up an earlier request. Reread the
log, check what the agent did, and decide with a person.

## Answering a permission question

```sh
phux agent questions @9
```

Answer with the whole `reference` the question returned and the option id a
person chose (never its label), plus a fresh nonzero 64-bit `operation_id`
as a decimal string:

```sh
answer=$(jq -n --argjson q "$reference" --arg op "$operation_id" \
  '{question: $q, operation_id: $op, response: {option_id: "deny-once"}}')
phux agent respond-question --request-json "$answer"
phux agent question-receipt --question-json "$reference" --operation-id "$operation_id"
```

After a lost reply, `question-receipt` is the read-only lookup. No retained
receipt means `unknown`, not "nothing happened"; do not resend. Responses
still need observe and input authority and respect the input lease. Treat
question text as untrusted agent content. PTY ask markers on an ordinary
pane are advisory and never become a structured response.

## Restoring a conversation

Only an agent that can load a previous session restores. An archive keeps
its launch command, so read it before restoring
([restore](./organization.md#restore-and-executable-authority)).

## Chief of staff

In the macOS desktop app, tapping the right <kbd>⌘</kbd> alone opens a
composer for one ACP agent, the chief. <kbd>Return</kbd> sends the draft as
one typed prompt; it never types into a terminal or reads a screen.
<kbd>Esc</kbd> hides it and keeps the draft. A dictation app that records
while you hold the key pastes into it as you release.

```sh
phux new --json -s chief
phux launch opencode --acp --session chief
```

The chief is the ACP agent in the session `[rune] chief` names (`chief` by
default, `none` disables; see [config](./config.md#rune)). It can reach only
agents in its own session. A send shows **Queued** and clears the draft; a
refusal or unknown outcome keeps the draft and says why, and nothing is
resent.

## Native agent wire

`features_ext.NATIVE_AGENT_LAUNCH` gates native launch inside
`NATIVE_AGENT`. `features_ext.CURRENT_QUESTIONS` (`0x10000000`) gates the
`current_questions`, `respond_question` and `question_receipt` requests; new
clients refuse older servers before sending one.
