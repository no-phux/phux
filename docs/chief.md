# Chief of staff

The chief of staff is one standing agent per phux mesh. You tell it what you
want from a phone (or any client); it does the work by going into panes and
shells with phux, then reports back. This page is the decision record and the
contract every client builds against. Its code is
[`integrations/chief`](../integrations/chief); the server half is the
reply push in [`push`](../crates/phux-server/src/push/mod.rs).

## Decision

**The chief is a [Pi Durable](https://github.com/earendil-works/pi/tree/main/packages/durable)
harness in an ordinary phux pane, found by role, spoken to through the
pane's acknowledged input, and heard through the pane's AgentSession stream.**

- *Durability is Pi Durable's, not ours.* `@earendil-works/pi-durable` is
  Earendil's harness built for exactly this ("long-running, durable, and
  malleable agents", [announcement](https://earendil.com/posts/pi-durable/)):
  every model request, tool call and queued message is a task checkpointed
  to SQLite (`~/.local/state/phux/chief/chief.sqlite`) before it is shown.
  A killed process reopens the same storage and continues the interrupted
  run; a resend with the same `requestId` is the same submission. Pi's
  coding-agent TUI (`pi --session-id`) persists a transcript but does not
  resume a run, so it is not used. Pi Durable is marked *experimental*;
  the package pins it exactly (`1.3.0`).
- *Pi's own model runtime.* Models, credentials and login come from Pi's
  `ModelRuntime`: the built-in catalog (generated from
  [models.dev](https://models.dev) and refreshed from pi.dev's catalog
  overlay), `~/.pi/agent/models.json` for custom providers, and
  `~/.pi/agent/auth.json` for credentials, shared with `pi` under its file
  lock so OAuth refresh-token rotation stays serialized. Every provider Pi
  supports is switchable. Default model: `anthropic/claude-sonnet-5`.
- *Tools: phux and the web, nothing else.* The 14 shared phux tools
  (`phux_panes`, `phux_create`, `phux_spawn`, `phux_run`, `phux_send_keys`,
  `phux_paste`, `phux_snapshot`, `phux_wait`, `phux_agent_prompt`,
  `phux_agent_wait`, …) plus `web_search` (Exa; keyless MCP or
  `EXA_API_KEY`) and `web_fetch`. The chief has no file or shell tool of its
  own: it opens shells in panes. The tools refuse the chief's own pane.
  This bounds *scope*, not authority: a shell pane is the user's shell.
- *A pane, not a service.* Clients already render AgentSession streams
  natively and already deliver acknowledged pane input, so the chief needs
  no new transport, port or credential. Rejected: a pi RPC bridge daemon
  (new process, transport and auth for no gain); an ACP-hosted chief
  (guardian-owned, but no Pi Durable ACP server exists yet; revisit when
  one does).
- *Supervision in two layers.* `phux-chief supervise` runs in the pane and
  restarts a chief that exits (2 s, backing off to 120 s while it keeps
  failing fast). `phux-chief ensure`, run by launchd at load and every
  60 s, starts the pane when no pane is tagged `chief` (killed pane, server
  restart, reboot). Neither ever restarts or signals the phux server.
- *One per mesh, placement by config.* `chief.json`'s `host` names the one
  host that runs it; `ensure` is a no-op elsewhere. Clients never assume a
  host: they discover the chief by role.

## Phone ↔ chief contract

Version `phux.chief/v1`. Everything rides existing wire features.

### Find the chief

On each connected host, the chief is the pane whose `phux.agent/v1` record
has `name == "chief"` (kind `pi`, session `chief:chief`), also tagged
`chief` (`phux.tags/v1` contains `"chief"`, so `#chief` addresses it from
the CLI). `phux.agent/v1` is one of the two keys a hub mirrors from its
satellites, so a phone attached to a hub also finds a chief on a satellite.
More than one match is a misconfiguration; prefer the one whose stream's
latest `status` record is newest. Its AgentSession child (provider `pi`,
native id `chief`) carries the thread. A restarted chief opens a **new**
AgentSession and replays its last 40 user/assistant messages into it
(stable entry ids), so a client that resets on a new session id still shows
one continuous thread.

### Send

Deliver **one line** of JSON through the pane's acknowledged input
(the `agent prompt` path: text plus Enter in one receipt):

```json
{"chief":1,"id":"<uuid>","type":"message","text":"…","locked":false}
```

- `id` (`[A-Za-z0-9._:-]{1,128}`) is the idempotency key. It becomes the Pi
  Durable `requestId`, so resending after a lost receipt never asks twice.
  Keep the outbox item until the chief's `ack` for that id arrives.
- `text` may contain newlines (JSON-escaped). Lines may be long: the chief
  reads its terminal raw, not in canonical mode.
- A pane input receipt is a kernel-queue receipt. **Admission** is the
  `ack` record below.

Other envelope `type`s, all with optional `id`:

| `type` | fields | effect |
| --- | --- | --- |
| `unlock` | | release held messages (see Locked) |
| `abort` | | stop the current turn |
| `status` | | republish `status` and `providers` |
| `models` | `query?` | publish `models` pages (credentialed models only) |
| `model` | `model` (`provider/id`), `thinking?` | switch the conversation's model; thinking is clamped to what the model supports |
| `thinking` | `level` | `off`…`xhigh`, clamped |
| `login` | `provider`, `method?` (`oauth`/`api_key`) | start a provider login |
| `login_answer` | `login`, `value` | answer the login's open prompt |
| `login_cancel` | `login` | cancel it |
| `logout` | `provider` | remove the stored credential |
| `compact` | `instructions?` | compact the context now |

A person typing in the pane is at the host: plain text is an unlocked
message and `/status`, `/model …`, `/login …`, `/code LOGIN VALUE`,
`/unlock` mirror the envelope (`/help` lists them).

### Hear

Read the chief pane's AgentSession stream. Besides the ordinary lifecycle
records (`prompt` → working, `tool_start`/`tool_end`, `stop` → done), it
carries two `provider_raw` schemas:

- `phux.transcript/v1` entries (user, assistant, thinking, tool, system)
  exactly as for any agent pane; render the thread from these.
- `phux.chief/v1` records, `{"schema":"phux.chief/v1","event":…}`:

| `event` | fields |
| --- | --- |
| `ack` | `id`, `status`: `admitted` / `held` / `duplicate` / `done` / `unanswered` / `rejected` / `started`; `detail?`, `submission?`, `released?` (unlock), `login?` |
| `status` | `version`, `host`, `pane`, `model {provider,id,name}`, `thinking`, `yolo`, `busy`, `held [{id,preview,at}]`, `login`, `self_heal` |
| `providers` | `page`, `pages`, `providers [{id,name,configured,source?,oauth,api_key}]` |
| `models` | `page`, `pages`, `query`, `models [{provider,id,name,context,reasoning}]` |
| `login` | `login`, `provider`, `step` and its fields (below) |
| `alert` | `findings [{fingerprint,source,title}]` from the error intake |

A message's `ack` goes `admitted` (durably queued) and later `done` or
`unanswered` (with `detail`). Each record stays under the 16 KiB codec line;
long lists are paged.

### Locked sends and `yolo`

The phone may send before Face ID unlock. It says so with `"locked": true`.

- Without `yolo` (the default) a locked message is **held**: stored durably
  in the conversation's `phux.chief.held` document, never shown to the model,
  so no tokens are spent and no tool runs. The chief answers
  `ack status=held`, adds it to `status.held`, and writes a system
  transcript line. After unlock the phone sends `{"type":"unlock"}`; held
  messages are admitted in order, prefixed `[sent while the phone was
  locked]`, and the ack lists `released` ids. A held message resent with
  the same id answers `duplicate`.
- With `yolo` (`"yolo": true` in `chief.json`, or `PHUX_CHIEF_YOLO=1` for
  the process; host-side only, never settable from the phone) a locked
  message runs at once, tools included, with the same prefix.

`locked` is the phone's own statement and the hold is a phone-side safety
policy, not host authentication: anyone who can type into the chief pane
already holds pane input on that host.

### Reply push with text

When a turn ends the chief writes `stop` with
`{"turn_id":"run-<n>","reply":"<final text>","reply_title":"Chief"}`
(`pushReplyText: false` omits `reply`). The server pushes a `stop` that
carries a non-blank `reply` to every registered device whose connection is
gone, once per pane and turn, with the same gateway contract as asks plus
three fields:

```json
{"schema_version":1,"host":"<grant host>","pane":"local:82","question":"run-54",
 "action":"reply","kind":"reply","title":"Chief","text":"<≤1024 bytes>"}
```

`text` is trimmed and cut on a character boundary to 1024 bytes with `…`;
`title` to 64. An ask's notice is unchanged and still content-free. The
gateway shows `title`/`text` as the alert when `kind == "reply"` and keeps
the fixed copy otherwise (a gateway that ignores the new fields still shows
the generic alert). A notification text reply to a `kind: reply` notice is
a new chief `message`, not an ask answer.

### Login from the phone

`{"type":"login","provider":"anthropic"}` starts Pi's own provider flow on
the host. Each step the human must take is a `login` record:

| `step` | fields | phone does |
| --- | --- | --- |
| `started` | `method` | show progress |
| `prompt` | `prompt {kind, message, placeholder?, options?}` | `select`: pick an option id; `manual_code`/`text`/`secret`: collect the value; send `login_answer` |
| `url` | `url`, `instructions?` | open the URL on the phone |
| `device_code` | `user_code`, `verification_uri`, `expires_in?` | show the code, open the URI |
| `info` / `progress` | `message`, `links?` | show |
| `done` / `failed` (`error`) / `cancelled` | | finish; `status`/`providers` follow |

Pick the headless option when offered (`copy_code` for Anthropic,
`device_code` for OpenAI Codex): a loopback callback on the host cannot
receive a phone browser's redirect, and the pasted code (or redirect URL)
completes the flow. Credentials land in Pi's `auth.json` on the host and
never cross the wire back. `api_key` logins prompt with `kind: secret`;
that value does cross the phone→host wire, inside the pinned mTLS
connection, and is never echoed.

## Self-correction

The chief's persona and skills (`integrations/chief/assets`, extended by
`~/.config/phux/chief.md` and `~/.config/phux/chief-skills/*.md`) make
errors its work. An intake on the host reads, every five minutes: new
`ERROR`/panic lines in the server log (fingerprinted by message template),
new `phux*.ips` crash reports, new `phux report` bundles, failed `main` runs
on `ownedRepos` (`gh run list`), and the chief's own faults (harness reports,
a tool failing three times running). The phone forwards its MetricKit
diagnostics as an ordinary chief message; there is no dedicated intake for
them yet. New findings become an `alert` record and, with `selfHeal: auto`,
one `[error intake]` turn per cooldown window (`selfHealCooldownMinutes`,
default 60). That turn investigates through phux shells and, for a defect in
an owned repository, fixes it in a worktree branch and opens a PR. It never
merges, releases, pushes to main, or touches the running server.

## Threat model changes

- **Reply text leaves the host.** For the chief only, and only while
  `pushReplyText` is on, up to 1 KiB of reply text passes through the push
  gateway and APNs/FCM in the clear to those services. The user opted in.
  Asks stay content-free. Keep secrets out of chief replies; the persona
  asks for outcome-first summaries, not raw output.
- **Pre-unlock sends.** A locked phone can queue work. Without `yolo` it
  cannot make the chief act until unlock; with `yolo` it can, which is the
  stated trade.
- **Prompt injection.** The chief reads terminals, web pages and other
  agents. Its persona treats all of it as data, but a model can still be
  steered, and its tools reach every shell on the mesh. That reach is the
  product; the boundaries are the persona's (owned repositories only, no
  merges or server restarts) and the human's review of its PRs.

## Run it

```sh
phux-chief install   # dotfiles wrapper: copy + build integrations/{runtime,chief}
phux-chief ensure    # what launchd runs; prints {"kind":"started","pane":"@N"}
phux-chief config    # effective config and paths
touch ~/.local/state/phux/chief/disabled   # stop ensure and the supervisor
```

Configuration is `~/.config/phux/chief.json`: `host`, `model`, `thinking`,
`yolo`, `pushReplyText`, `selfHeal` (`auto`/`notify`/`off`),
`selfHealCooldownMinutes`, `ownedRepos`, `authPath`, `session`, `cwd`.
State and logs are under `~/.local/state/phux/chief/`.
