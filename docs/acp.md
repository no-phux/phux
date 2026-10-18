# ACP-owned sessions

These examples describe the [main-source interface](./README.md#source-and-installed-releases).
Check the installed CLI and server capabilities before relying on a feature.

Attached TUI agents use terminal integrations above. Agent Client Protocol
(ACP) sessions instead run a provider subprocess through the server-owned
`phux-agent-host` guardian and a reserved AgentSession controller. The operator on the executing
host must install and authenticate the provider first; phux neither downloads
it nor falls back to typing into a terminal when ACP is unavailable.

Inspect enabled integration templates without starting a provider:

```sh
phux launch --list --acp
phux launch opencode --print --acp
```

For example, the shipped [OpenCode template](../examples/plugins/agent-tools/integrations/opencode.toml)
declares argv, not a shell string:

```toml
[acp]
provider = "opencode"
command = ["opencode", "acp"]
required_executables = ["opencode"]
working_directory = "workspace"
```

With that plugin enabled and provider ready, `phux launch opencode --acp
--session work` launches into an existing owning session. The server must
advertise `NATIVE_AGENT`; legacy peers refuse before transmitting the new
command, not by key/paste fallback. Successful provider initialization is
not evidence of a completed model turn or full provider feature parity.
Current capability flags are authoritative; client filesystem/terminal
callbacks are not advertised. Conversation restore requires the provider's
`loadSession` capability. ACP archives currently retain executable launch
recipes, not the terminal-resume trusted-template boundary: inspect/trust
them before explicit restore ([restore authority](./organization.md#restore-and-executable-authority)).

Use [`agent native-command`](./reference/cli.md#phux-agent-native-command)
with the exact AgentSession, activation, nonzero operation identity and
turn/question ids from its structured view. A queued result is only server
reservation or provider-RPC scheduling, **not provider admission**. Completion
reports the actual ACP stop reason. RPC/transport loss is unknown, not proof
of failure: retain the original operation identity and observed receipts;
never automatically replay it or make a replacement id to retry side effects.
The CLI lookup limitation below matters after disconnect.
A provider question is a human-only first-responder reservation; an automation
principal cannot answer, and a dialog response queued is not proof it was
consumed. Ordinary terminal asks and native questions are different lanes.

## Read, submit, and reconcile

Use the **AgentSession** id returned by
`phux launch opencode --acp --session work --json`, not its `terminal_id`;
launch returns `ready: false`. With jq installed, read the
latest retained native view (example id `@9`; substitute your returned id):

```sh
phux agent log --json @9 > native-log.json
jq '[.records[] | select(.type == "provider_raw" and .data.schema == "phux.native-agent/v1" and .data.update.type == "view") | .data.update.view] | last' native-log.json
```

The selected object supplies `activation`, `connected`, `capabilities`,
`turn_id`, `provider_session_id` and `questions`. Missing/null is not ready;
check the current capability for the intended operation. `connected: true`
alone can also occur for a startup question, so do not infer prompt readiness
from it. Re-read before acting: question/turn ids can become stale.

For a view with activation `activation-A`, prompt support and no active turn:

```sh
phux agent native-command @9 --activation activation-A --operation-id 101 --intent '{"type":"prompt","text":"Review this checkout"}' --json
```

Operation numbers here are illustrative: allocate a random nonzero u64 per
new human action, keep it with the exact intent, and never reuse a tutorial
number on real work. For an interrupt, the intent is
`{"type":"interrupt","turn_id":"acp:101"}` only if that is the observed
`turn_id`. For a current `questions[]` permission entry with
`id: "acp-question:1"` and option `id: "allow-once"`, a **human**, after reading
its untrusted title/message and choosing that option, may send:

```sh
phux agent native-command @9 --activation activation-A --operation-id 102 --intent '{"type":"answer","question_id":"acp-question:1","response":{"option_id":"allow-once"}}' --json
```

Use the published question id, not `fields.source_question_id` or the option's
label. Other question kinds require their own response shape; the
[typed intents and answer validator](../crates/phux-protocol/src/native_agent.rs)
own those shapes. Do not automate a human answer or use terminal `agent answer`
as a native permission bypass.

A possible CLI receipt (submission, not completed work) is:

```json
{"schema_version":1,"agent_session_id":"@9","activation":"activation-A","result":{"operation_id":101,"disposition":"queued","detail":"ACP prompt scheduled; ACP v1 has no admission receipt"}}
```

**Uncertain lookup limitation:** current `agent native-command` submits once;
it is not a result-query verb. There is no separate CLI or `NativeRequest`
lookup for a retained operation id. Do not rerun the command as a "query".
`agent log` can re-read the provider view/transcript, not the operation-receipt
ledger; generic `watch` currently renders native result events as `unknown`
without their receipt fields. Integrations can observe typed
`AgentEvent::NativeAgentResult` / runtime `Event::NativeAgentResult` for the
exact session and operation, including retained event replay when available,
but a journal gap is not an outcome. After a lost CLI receipt, keep the id,
inspect the view and provider-side evidence with a human, and stop if the
outcome remains unknown.
See the [submit-once client and receipt tests](../crates/phux-client/src/native_agent/operations.rs)
and [CLI receipt envelope](../crates/phux/src/commands/agent/native_control.rs).

The [guardian ACP seam](../crates/phux-agent-host/src/session/acp.rs) owns
readiness and stop reasons; the [native operation ledger](../crates/phux-server/src/state/native_agent.rs)
and [command authority](../crates/phux-server/src/runtime/native_agent.rs)
own identity, queue bounds, human role and first response. Generic lifecycle
emit cannot forge these reserved native publications.

