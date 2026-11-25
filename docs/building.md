---
group: build
order: 0
title: Build on phux: JSON CLI, MCP, wire protocol, OSC 7501 and plugins
description: There is no published SDK or client library yet. A program builds on the JSON CLI, MCP, the wire protocol, unpublished Rust client crates, OSC 7501, or plugins.
---

# Build on phux

How a program drives phux, shows its agents, or reports status into a pane.
There is no published SDK or client library yet.

## Entry points

- [Versioned `--json`](./scripting.md#json-output-and-errors). Text is the
  [CLI reference](./reference/cli.md).
- [MCP](./agents.md#mcp): `phux mcp` over stdio. The
  [parity table](./reference/parity.md) maps tools to commands.
- [`phux watch`](./scripting.md#watching-events): one JSON object per line.
- [Server protocol](./protocol.md): length-prefixed binary frames.
  [Compatibility](./compatibility.md) is the 1.x rule;
  [`wire-schema.json`](./spec/wire-schema.json) is generated from the codec.
- [Program Status Protocol (OSC 7501)](./program-protocol.md): one escape
  sequence, no library, no socket.
- [Plugin API](./plugins.md): a directory with a `phux-plugin.toml`.
- [Resource kinds](./resource-kinds.md): `phux agent doc serve` turns JSON
  lines on a pipe into a typed document every client draws.
- [Agent integrations](./agents.md#agent-setup).
- [Calling phux from a program](./agent-quickstart.md#call-phux-from-a-program)
  and [Embed phux](./embedding.md).

## Connect

`phux` finds the user's server; `--socket` or `PHUX_SOCKET` picks another.
With no server, commands exit 1 with `no_server`: tell the user to run
`phux server` or `phux service install` rather than starting one. Joined
[hub](./remote.md#linking-machines) machines appear as `mini/@7`. An app
that cannot run `phux` (a phone) speaks the [protocol](./protocol.md) over
QUIC after `phux pair`, and gets wire frames instead of the JSON below.

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

One row per pane, agent or not (`agent.kind` is `unknown` for no agent).
`terminal` is the address every other command takes. `state` is one of the
[agent states](./agents.md#agent-states); sort by `attention`. Show
`explanation`, which `phux agent explain` also prints. `address` is the
`%name` that reaches this pane, else `null` with `address_refusal` saying
why. `phux agent show --json @1` prints one pane; `--state blocked` keeps
only agents waiting on a person.

## Agent state

Agent state is evidence-ranked; screen is the floor. The order is in
[Precedence](./program-sequences.md#precedence). A screen that matches no
rule reads as `idle`, never `blocked`.

It has not been measured on a live Claude Code, Codex, OpenCode, or pi session, with or without an integration installed.
[`scripts/agent-verify`](../scripts/agent-verify/README.md) would record it
(billable model calls); CI runs it against a stub.

## Read and answer asks

An agent asks through a [`phux-ask` title](./program-sequences.md#questions-the-phux-ask-title)
or `phux ask`. Every watcher of the pane gets:

<!-- contract: asked-event -->
```json
{"event":"asked","terminal":"@1","id":"deploy-7","question":"Deploy to staging?","suggestions":["yes","no"],"elapsed_seconds":null,"seq":12,"ts_ms":1794690502155}
```

While a title question is open, the row's `title` is the `phux-ask` line,
so a client can read it without waiting. A `phux ask` question sets no
title and `phux agent answer` cannot answer it.

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

`--text TEXT` must match a suggestion unless `--allow-unlisted`. phux checks
the pane still asks that `id` right before typing the answer as one paste
plus Enter. On refusal nothing is typed; branch on `error.code`:

| Code | Do |
| --- | --- |
| `ask_stale`, `no_active_ask` | The agent moved on. Read the pane again. |
| `unlisted_answer`, `choice_out_of_range`, `no_suggestions` | Pick from `suggestions`, or pass `--allow-unlisted`. |
| `answer_refused` | Safe to retry. |
| `answer_delivery_unknown` (exit 3) | Part may have arrived. Do not resend; read the pane. |

## Follow events

`phux watch --json @1` prints events until the pane closes; for agents,
show `agent_state` and `asked`. A watch never attaches or resizes. On exit
its last stderr line is the resume cursor:

<!-- contract: watch-cursor -->
```json
{"cursor":"2b5af13f10d9a1f69188f055261b9722:12","cursor_void":false}
```

Pass `cursor` to `--after` to replay held events first. `cursor_void: true`
(a previous server run) or a `journal_gap` event means events were missed:
re-read `phux agent list`. `phux watch` takes one target; for a fleet, poll
`phux agent list` and watch the panes you show.
[`examples/agents/06-client.sh`](../examples/agents/06-client.sh) runs this
whole loop against a throwaway server.

## Who is typing

`phux resource show --json` carries, as `controller`, the connection that
last typed into the pane (key or paste, attached or via `send-keys`):

```json
{ "client": 7, "client_name": "phux-mobile", "route": "bearer-quic", "remote": true, "platform": "ios" }
```

`client` matches `input_holder` and `viewers`. `client_name` is
self-reported or `null`; phux sends `phux-tui/<v>`, `phux-cli/<v>` (any
headless verb, including MCP `phux_run`), or `phux-mcp/<v>`. `route` is the
[`phux whoami`](./remote.md#who-can-do-what) `auth_route`; `remote` is
`false` for the owner socket and loopback. `platform` is the client's
`HELLO` word (`[a-z0-9-]{1,16}`), absent if none. `controller` is `null`
once that connection closes; through a hub it names the hub. `phux watch`
emits `controller_changed`. Use it to pick a prompt, never to grant
anything.

## Host checklist

`phux doctor --json` ends with a `checklist`, which clients also get by
asking the server
([What works here](./troubleshooting.md#what-works-here)):

```json
{"schema_version": 1, "items": [{"id": "notifications", "title": "Asks and notifications",
  "status": "off", "reason": "...", "fix_command": null,
  "fix_action": "enable_notifications", "push_devices": []}]}
```

- `id`: `agent_status`, `notifications`, `remote_reach`, `file_transfer`,
  `restore`, `skill`; skip unknown ids.
- `status`: `ok`, `off`, `waiting` (will work without help), `unknown`
  (could not check).
- `fix_command`: commands to run on the host, one per line, or `null`.
- `fix_action`: a one-tap client action (`enable_notifications`) or `null`.
- `push_devices`: `notifications` only, device ids holding a push grant.

Adding an item or field keeps `schema_version`; renaming or removing one
bumps it.

## Version rules

`--json` documents and errors follow
[JSON output and errors](./scripting.md#json-output-and-errors);
`phux watch` lines carry no `schema_version` but follow the same rule. The wire
follows [Compatibility](./compatibility.md). Each `contract:` block on this
page is checked by
[`client_contract.rs`](../crates/phux/src/commands/agent/client_contract.rs).

## Not shipped

Only `phux-protocol` is on crates.io
([Releasing](./releasing.md#npm-and-cratesio)); the client and embed crates
are `publish = false`. There is no Python or JavaScript client package.

There is no JSON socket. A client speaks the binary
[transports](./protocol.md#transports) or spawns `phux` and reads stdout.

There is no example application.
[`examples/agents`](../examples/agents/README.md) holds scripts that spawn
`phux`.

The static plugin index is built from the GitHub topic `phux-plugin` and the
plugins in this repository ([Listing a plugin](./plugins.md#listing-a-plugin)).

[llms.txt](./llms.txt) is this repository's builder index.
