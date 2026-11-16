# Build a phux client

Show the coding agents running in phux in your own app, and let a person
answer the ones that are waiting.

This page is for a dashboard, a menu-bar app, or an agent terminal. There is
no published SDK or client library yet, and this page needs none: the app
runs the `phux` binary and reads JSON. Everything here is
in one script you can run,
[`examples/agents/06-client.sh`](../examples/agents/06-client.sh).

| You want | Run |
| --- | --- |
| Every agent and its state | `phux agent list --json` |
| The question an agent is asking | the `asked` event, or the pane's `title` |
| To answer it | `phux agent answer --id ID --choice N --json TARGET` |
| What happens next | `phux watch --json TARGET` |

[Build on phux](./building.md) maps every other surface: MCP, the wire
protocol, OSC 7501, and plugins.

## Connect

### This machine

Each user runs one phux server, reached through a Unix socket that `phux`
finds by itself. Set `PHUX_SOCKET` (or pass `--socket PATH`) to talk to
another server, such as a throwaway one for tests:

```sh
phux server --socket /tmp/phux-test.sock --session demo &
PHUX_SOCKET=/tmp/phux-test.sock phux agent list --json
```

With no server running, a command exits 1 with the `no_server` error on
stderr. Show the person `phux server` or `phux service install`; do not start
a server for them behind their back.

### Other machines

- **Link them.** On a [hub](./remote.md#linking-machines), the machines
  joined to it show up in the hub's own `phux agent list`, their panes named
  `mini/@7`. `phux watch mini/@7` follows one of them through the hub.
- **Run phux there.** `ssh mini phux agent list --json` prints the same
  document.
- **Pair.** An app that cannot run `phux`, such as a phone, speaks the
  [server protocol](./protocol.md) over QUIC. `phux pair` on the server
  prints a one-time link ([pairing with a link](./remote.md#pairing-with-a-link)).
  The phone apps connect with it through `phux-client-ffi`
  ([Embed phux](./embedding.md)). Agent records and `asked` events arrive as
  wire frames there, described by [`wire-schema.json`](./spec/wire-schema.json),
  not as the JSON on this page.

## Read agents

```sh
phux agent list --json
```

<!-- contract: agent-list -->
```json
{
  "schema_version": 1,
  "agents": [
    {
      "terminal": "@1",
      "session": "demo",
      "window": "window-0",
      "agent": { "id": "claude", "label": "reviewer", "kind": "claude" },
      "state": "blocked",
      "confidence": 1.0,
      "attention": "high",
      "title": "phux-ask[deploy-7]:Deploy to staging??s=yes|no",
      "cwd": "/home/me/src/app",
      "sources": [
        {
          "kind": "agent_record",
          "signal": "phux.agent/v1 record published for this pane",
          "confidence": 1.0,
          "observed": "{\"name\":\"reviewer\",\"kind\":\"claude\",\"state\":\"blocked\"}"
        }
      ],
      "explanation": "the phux.agent/v1 record for this pane (ADR-0040)",
      "agent_session": { "resource": "@2", "provider": "claude", "native_id": "4f0c2a9e" },
      "address": "%reviewer",
      "address_refusal": null
    }
  ]
}
```

One row per pane, agent or not:

| Key | Means |
| --- | --- |
| `terminal` | The pane's address, `@1`, or `mini/@7` for a linked machine. Pass it to every other command. |
| `session` | The phux session's name. |
| `agent.kind` | `claude`, `codex`, `open_code`, `pi`, `omp`, `grok`, `amp`, `cursor_agent`, `gemini`, `goose`, `aider`, `plugin`, `declared`, or `unknown` (no agent). |
| `agent.label` | A name to show: the agent's own name when it has one. |
| `state` | `working`, `blocked` (waiting on a person), `idle`, `done`, or `unknown`. See [agent states](./agents.md#agent-states). |
| `attention` | `none`, `low`, `normal`, or `high`. Sort by it. |
| `confidence`, `sources`, `explanation` | Why phux thinks so. Show `explanation`; `phux agent explain` prints the same. |
| `title`, `cwd` | The pane's title and working directory, or `null`. |
| `agent_session` | The agent's session stream, or `null` for most panes. |
| `address` | `%name` when that name reaches this pane, else `null`; `address_refusal` says why not. |

`phux agent show --json @1` prints the same document for one pane.
`--state blocked` keeps only the agents waiting on a person.

## Read asks

An agent asks a question by setting its pane title to a
[`phux-ask` line](./program-sequences.md#questions-the-phux-ask-title), or
through `phux ask` from its hook. Either way, phux sends an `asked` event to
every watcher of the pane:

<!-- contract: asked-event -->
```json
{"event":"asked","terminal":"@1","id":"deploy-7","question":"Deploy to staging?","suggestions":["yes","no"],"elapsed_seconds":null,"seq":12,"ts_ms":1794690502155}
```

`id` is what an answer names. `suggestions` are the answers the agent
offered, in order, and may be empty. `elapsed_seconds` is how long the agent
has been waiting, when it said, else `null`.

To read what a pane is asking right now, without waiting for the event, read
its `title` from `phux agent list`. While a title question is open, the
title is the `phux-ask` line itself, as in the example above. Parse it with
the grammar on that page. A question sent through `phux ask` sets no title,
so it reaches you only as an event, and `phux agent answer` cannot answer
it: the person answers in the agent.

## Answer an ask

```sh
phux agent answer --id deploy-7 --choice 1 --json @1
```

<!-- contract: agent-answer -->
```json
{
  "schema_version": 1,
  "terminal": "@1",
  "ask": { "id": "deploy-7", "question": "Deploy to staging?", "suggestions": ["yes", "no"] },
  "answer": "yes",
  "source": "choice",
  "operation_id": "59d77208-06fb-4a80-98c6-57a532297c49",
  "delivered": true
}
```

`--choice N` sends the Nth suggestion. `--text TEXT` sends text, which must
be one of the suggestions unless you add `--allow-unlisted`. phux types the
answer as one paste and Enter, in a single acknowledged write.

phux checks that the pane is still asking that `id` right before it types,
so an answer never lands in whatever replaced the question. A refusal
prints a JSON error on stderr and types nothing. Branch on `error.code`:

| Code | Do |
| --- | --- |
| `ask_stale`, `no_active_ask` | The agent moved on. Read the pane again. |
| `unlisted_answer`, `choice_out_of_range`, `no_suggestions` | Pick from `suggestions`, or pass `--allow-unlisted`. |
| `answer_refused` | Nothing was typed. Running the same command again is safe. |
| `answer_delivery_unknown` (exit 3) | Some of the answer may have arrived. Do not send it again; read the pane with `phux snapshot`. |

## Follow events

```sh
phux watch --json @1
```

This prints one JSON object per line until the pane closes or you stop it.
The events are listed in [watching events](./scripting.md#watching-events);
for agents, `agent_state` and `asked` are the ones to show. Ignore events and
keys you do not know. A watch never attaches to the pane or resizes it.

When a watch stops, its last line on stderr says where to resume:

<!-- contract: watch-cursor -->
```json
{"cursor":"2b5af13f10d9a1f69188f055261b9722:12","cursor_void":false}
```

Pass `cursor` to `--after`, and the events the server still holds since
then arrive before live ones. `cursor_void: true` means the cursor came from
an earlier server run. That, or a `journal_gap` event, means you may have
missed events: read `phux agent list` again.

`phux watch` follows one pane. For a whole fleet, read `phux agent list` on
a timer and watch the panes you show. `phux watch --until asked --timeout
600 @1` exits 0 at the first question, for scripts that only wait.

## The contract

- A `--json` document has a `schema_version`. It changes only when a key is
  removed, renamed, or retyped. New keys appear without a change, so ignore
  keys you do not know ([JSON output and errors](./scripting.md#json-output-and-errors)).
- On failure, stdout is empty and stderr holds one JSON object with
  `error.code`. `phux --capabilities --json` lists the codes.
- Watch lines carry no `schema_version`; the same rule applies to their keys.
- Each JSON example on this page is checked by a test
  ([`client_contract.rs`](../crates/phux/src/commands/agent/client_contract.rs)).
  It fails if a key shown here is removed or changes type.

## Run the example

```sh
bash examples/agents/06-client.sh
```

It starts a throwaway server on a private socket, makes a pane ask a
question, reads it from `phux agent list` and `phux watch`, answers it, and
checks that the pane received the answer. Build this checkout first, or set
`PHUX` to an installed `phux`.
