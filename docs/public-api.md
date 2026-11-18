# Public API

```sh
phux api list
phux api describe terminal.create
phux api describe execution.result
phux api describe question.list
```

| Intention | CLI | MCP | Rust |
| --- | --- | --- | --- |
| Describe this server | `phux api list` / `phux api describe OPERATION` | `phux_api_describe` | `phux_client::terminal::describe` |
| Create, place and start | `phux terminal create --request-json JSON` | `phux_terminal_create` | `phux_client::terminal::create` |
| Recover a receipt, without effects | `phux terminal operation --session KEY --idempotency-key HEX32` | `phux_terminal_operation` | `phux_client::terminal::operation` |
| Reconcile a retained resource | `phux resource show --json ^FULL_RESOURCE_KEY` | `phux_resource_show` with `target: "^FULL_RESOURCE_KEY"` | `phux_client::resource::show::show_key` |
| Read or wait for an exact execution | `phux api call execution.result --request-json JSON` | `phux_execution_result` | `phux_client::resource::execution::result` |
| Read current questions | `phux api call question.list --request-json JSON` | `phux_agent_questions` | `phux_client::native_agent::questions::current_on` |
| Answer an exact question or recover its receipt | `question.respond` / `question.receipt` via `phux api call` | Not exposed | `phux_client::native_agent::questions::{respond_on, receipt_on}` |

These CLI commands emit versioned JSON. Errors are JSON on stderr with a
stable code; creation errors preserve the retry key and distinguish not sent,
unknown outcome and a committed but incomplete receipt. Creation success means
intent committed, **not** process exit or successful task completion.

## Run argv and wait in one command

```sh
phux terminal run --session work -- make test
```

`terminal run` resolves a full session key or an exact name, launches argv
directly in a new placed terminal, and waits for that exact execution. It
does not type into a shell or use focus. The working directory defaults to
the session's recorded directory when present; `--cwd` overrides it.
`--remote HOST` uses one registered owner for resolution, creation, and
observation. Output produced by the program remains in its terminal.

The JSON result includes the creation `receipt` and the exact execution
`result`. The command returns the child's status for `exited`, or `128 +
signal` for `signaled`. A wait deadline (`--timeout SECS`, default 600)
returns 125 without terminating the child, a confirmed launch failure
returns 126, and an unknown outcome returns 1. Use the JSON `status` to
distinguish these from a child returning those numbers. After an unreadable
launch response phux only tries a read-only receipt lookup. If that cannot
confirm the effect, use the returned `session` and `idempotency_key` with
`terminal operation` on the same authority.

## First use

The server must already be running. Start from its description and the durable
session keys in `phux ls --json`, not mutable names or a remembered `@N`:

```sh
phux api describe terminal.create
session=$(phux ls --json | jq -r '.sessions[] | select(.name == "work") | .key')
key=$(openssl rand -hex 16)
request=$(jq -n --arg session "$session" --arg key "$key" \
  '{session: $session, idempotency_key: $key, argv: ["/bin/sleep", "30"]}')
printf '%s' "$request" | phux api call terminal.create --request-json -
```

The strict request has `idempotency_key` (nonzero 32 hex digits), `session`
(full 26-digit durable key), and optional `argv`, `cwd`, `env`, `name`, and
`placement`. Unknown fields are rejected. Empty argv selects the server's
default shell; a nonempty argv executes directly in a **new** process. It is
never shell source and never typed into an existing terminal. Environment
maps cannot override server-owned `PHUX_` variables or `TERM`. An explicit
`cwd` must be an absolute, enterable directory on the executing server; it
never silently falls back to another directory. This does not sandbox
programs running as the serving Unix user.

Placement defaults to `{"type":"new_group"}`. To split beside a terminal
created by this workflow, save its receipt and use:

```json
{
  "type": "beside",
  "entry": "<receipt.placement.entry>",
  "resource": "<receipt.resource>",
  "instance": "<receipt.execution.instance>",
  "activation": 1,
  "axis": "horizontal",
  "ratio": 0.5
}
```

Use the actual activation returned, not a guessed `1`. Horizontal means side
by side; vertical means stacked. Ratio is the fraction kept by the anchor,
finite and strictly between zero and one. The stable entry, resource,
incarnation and activation must still match in the destination session.
Unrelated layout edits need not conflict: the server applies the placement
intention to the current arrangement. Focus and selection remain client-local.

## Identity and outcomes

A successful receipt contains:

- `authority`, `session`, `resource`: durable keys, not current handles;
- `placement.arrangement` and `placement.entry`: stable identities of the
  committed presentation, which later edits may move or remove;
- `execution`: the original activation's current state and incarnation,
  when still recorded; `failed` is not a successful process exit. Optional
  `diagnostic` retains the failed launch stage and platform `errno`, never argv,
  paths or environment. A missing executable/interpreter is `failed`, not an
  executed program exiting 127; a real exit 127 remains `completed/exited(127)`;
- `execution.persistence_pending`: executor evidence awaits a refused store
  write; the displayed state is the **last committed** state, not proof the
  process is still starting/running. The server retries the fenced report,
  never the launch, and clears this flag after persistence recovers;
- `execution_ref`: the exact execution identity for later observation, or
  `null` when the receipt cannot expose the activation;
- `terminal_id`: a current local handle only when the original execution
  still has its engine; never persist it as a durable address;
- `idempotency_key`, `replayed`, and `schema_version` (`1`).
  On the JSON receipt, `execution.state` and `execution.outcome` accept an
  unknown value, and wire `ErrorCode` already decodes an unrecognized code
  as `Unknown`, so naming a new JSON state, outcome, or error code does not
  bump `schema_version`. That rule is JSON-only. A `#[serde(other)]`
  `Unknown` does not keep the original token: it re-serializes as `unknown`.
  The binary `wire_enum!` / `enum_byte` path hard-fails on an unknown byte
  (`UnknownEnumValue`), so adding a wire enum value needs a feature bit.

Relaunch opens another activation. A receipt of the old creation does not
adopt that replacement execution: its execution/handle observation becomes
null. A lookup from the original session after a resource moves does not
expose the execution in its new session. A missing observation is not proof
that an execution ended successfully.

The server commits **membership + placement + launch intent** in one
organization-store transaction. A placement refusal, stale anchor or failed
store write starts no process and publishes no partial layout. The OS process
starts after commit. If that launch fails, the creation remains committed with
a retained `failed` execution; no caller needs to undo a layout or issue a
compensating kill. After a restart, `failed` with `unobserved` and no launch
diagnostic can mean the launch intent committed without durable evidence
that the process started. It does **not** prove no OS process started before
the owner crashed. `completed` with `unobserved` likewise does not invent an
exit status. Neither is permission to blindly retry effects.
The public workflow retains the resource until explicit
removal. Its failed/completed execution survives engine retirement, but an
explicit relaunch replaces the current activation and its result. This is **not** legacy
`spawn --retain=SECS` screen retention, and does not promise retained PTY
output after its engine is gone.

Generated arrangements obey the wire size/depth limits. SQLite also rejects
any record or receipt that its bounded loader cannot read back; this can
impose a tighter nesting bound than the wire. A store refusal commits no
partial records and starts no process. Retiring a carried screen during hot
handoff does not purge a durable execution result.

## Lost replies and retry scope

There is no automatic effect replay in this client module. Keep the original
request and key. After timeout, transport failure or an unreadable mutation
reply, the original outcome is unknown. Read its receipt first:

```sh
phux terminal operation --session "$session" --idempotency-key "$key"
```

This lookup never launches or edits anything. A retained identical creation
can also be explicitly replayed with the same key and identical payload; it
returns the original resource and entry without running effects. Reusing the
key for another payload is refused. Prefer lookup when recovering a lost
reply: it does not depend on remembering every original default.

Receipts belong to the authority's organization store and survive reconnects
and server restarts, **not** moving to another authority. They are bounded by
24 hours **or the oldest-first 10,000-receipt cap**, whichever removes them
first. A short elapsed time alone cannot establish retention. After removal
or expiry, lookup reports `receipt_not_retained` with outcome `unknown`.
This does not distinguish never received from received then evicted. A later
request that reuses the same idempotency key after the receipt has been
pruned is executed again: it is not a replay and it is not refused as a
duplicate. Reconcile records and external effects; never blindly replay with
either the old key or a fresh one. There is no exactly-once guarantee beyond
retained receipts. Receipt environment values are stored as unsalted
SHA-256 hashes (`sha256:` plus hex), not plaintext. The hash is not a
secret: a low-entropy value is guessable, and the same value hashes the
same way in every receipt, so receipts can be compared. Receipts written
before hashing keep their plaintext until they are pruned. Replacing or
deleting a receipt does not scrub SQLite WAL or free pages. Replaying an
old-format receipt (plaintext environment still stored) conflicts: the
operation id is reused with a payload that no longer compares equal.

Save `resource` alongside the retry key. The normal reconciliation read is:

```sh
phux resource show --json "^$resource"
```

This exact full-key selector returns a **schema 2 durable resource document**:
`authority`, `resource`, current `session`, `name`, `kind`, latest `execution`
and nullable `terminal_id`. It needs `INVENTORY` on the resource under its
**current** membership. It works after exit, engine retirement, receipt eviction
and restart; it does not fetch a giant inventory or parse human text. Existing
live-handle/other-selector inspection keeps its schema 1 engine document.
`phux ls --json` remains a live-engine inventory; retained record counts can
therefore exceed its engine rows. Removed and out-of-scope records both return
explicit `no_such_target`, not an invented exit result.

A resource key follows moves and relaunches: schema 2 observes its **latest**
activation, not the historical activation of a creation operation. Save the
receipt's `execution_ref` to wait for or read that exact activation instead.
A session-scoped grant loses the read after a move outside that session; a
resource-key grant follows the resource. The original operation lookup remains
fenced to its creation activation and destination session, and never adopts a
replacement. Receipt expiry does not remove the resource's retained result.

The MCP adapter also has its separate in-memory mutation ledger. It forwards
the creation key to the server, but its pending operation receipt and
`phux_operation_status` do not become durable. `phux_terminal_operation`
queries the server, not that adapter ledger. Pending status names the tool
without echoing its request arguments.

## Exact execution results

Copy `execution_ref` from a creation receipt (or durable resource inspection)
into the `execution` field of the request:

```sh
execution_request=$(jq -c '{execution: .execution_ref, timeout_secs: 30}' receipt.json)
phux resource execution --request-json "$execution_request"
```

Without `timeout_secs` this reads once; with a budget from 1 through 3600
seconds it waits while the matching execution is active or its evidence is
pending persistence. Add `--remote HOST` to use a registered owner instead
of the local socket. Once that route is resolved, reconnects use the same
route and each read checks the durable authority. The wait never starts a
server or resends an effect.

`observed` carries the exact execution's state and any outcome, including
nonzero exit, signal, failed launch, and loss without an observed exit.
`superseded` means a later activation replaced the pinned one; it does not
return the newer outcome. `unavailable` means the pinned evidence is absent
or no longer visible. Neither means that the process never ran. `timed_out`
may include the last exact observation and exits 124 in the CLI. A settled
read exits 0 in the CLI; process status belongs in `observation.outcome`.

## Discovery, authorization and events

`phux api list` asks the **connected server** for supported public
workflows, complete nested input/output schemas, completion meanings and
retry retention. `phux api describe OPERATION` selects one contract from that
answer. `phux api call OPERATION --request-json -` reads its request from
standard input and dispatches to that operation's typed client. Prefer
standard input so launch arguments and environment aren't visible in the
client's process argument list. List and describe can display operations a
particular CLI build cannot call yet. These schemas derive from the same public DTOs
used by CLI, MCP and Rust callers. Representable list bounds, identities,
absolute/NUL-free cwd, environment restrictions and placement numeric bounds
are included. `semantic_validation` names checks requiring host facts, current
membership/authority or UTF-8 byte counts (JSON Schema counts characters).
`error_schema` and `error_outcomes` derive from the emitted structured error
DTO and its code/outcome vocabulary. `not_sent` means this invocation sent no
mutation; `refused` means the server refused it; `committed` requires receipt
reconciliation; `unknown` never establishes that the original mutation failed.
A missing receipt read is always `unknown`, even when that read itself was
refused or could not connect. MCP wraps this document in its tool-error carrier. `phux --capabilities --json` continues to
describe the installed binary; it is not connected-server discovery.

Supported is not authorized. Every invocation still crosses authenticated
server dispatch. Discovery currently requires global `INVENTORY`; a
session-scoped caller may be able to call an operation without being able to
list it. Placed creation requires `CREATE` and `ORGANIZE` on the
destination session. Lookup requires `INVENTORY` there and does not disclose
another session's receipt. No new listener, generic invocation dispatcher,
local-only bypass, input lease exception or approval bypass is introduced.

Organization lifecycle events follow committed state. Their journal is
bounded; resume gaps require a fresh level-state read, not an assumption of
lossless ordering across disconnects. A creation receipt is not an agent turn,
a shell-command completion, or a replacement for process/lifecycle observation.

## Compatibility and migration

`CREATE_PLACEMENT` gates the additive placement field and richer receipt.
`API_DESCRIPTION` gates description and read-only receipt lookup. Clients
refuse before sending when the corresponding server support is absent;
new complete-intention calls never silently downgrade to spawn-plus-rollback.
Old clients keep their existing wire requests and ignore new receipt fields.
The durable-result policy is persisted on the resource, not inferred from an
engine-local flag or a receipt that may expire, so it survives hot handoff and
cold restart. Optional diagnostic/evidence fields extend organization records
and receipts; older peers skip their TLVs and new readers accept old records
without diagnostics. Exact durable-key inspection requires the additive
`DURABLE_RESOURCE_INSPECTION` feature; older servers are refused before sending
that filter, not silently queried through live handles. Persisted organization command, record and receipt additions are optional serde fields,
so old organization records remain readable; no new database table is needed.
Pending lifecycle reports are bounded in memory to a start and an end per
extant activation, ordered and fenced by resource, activation and instance.
Removal/relaunch discards stale evidence. Hot handoff refuses while reports
cannot be flushed. Orderly shutdown makes a final persistence attempt but must
still permit exit when the store is unavailable. Any exit with **undurable**
evidence, including a hard crash, can lose it; startup then reconciles only the
committed evidence (possibly Unobserved/Lost).
This is not a durable effect scheduler or an exactly-once process guarantee.
The launcher uses a private close-on-exec status channel with a two-second
deadline; on uncertainty it terminates/reaps only its own child and does not
retry execution. Platforms without the compiled launcher fail closed rather
than using a spawn path that cannot distinguish requested-exec failure.

Recorded launch directories are now honored literally by the organization
executor: if one disappears before a create/restore/relaunch, that activation
fails instead of executing in an unrelated fallback directory. Legacy raw
spawn retains its separate compatibility behavior.

Empty-session directory seeding uses atomic placement on capable servers.
Its older-server path remains explicitly isolated. Existing `spawn`, `launch`,
raw input, shell `run`, agent prompt and agent wait retain their separate
contracts. In particular, satellite routing, named legacy projections,
timed screen retention and integration provenance are not silently redefined
as this local durable workflow. Use the new workflow for new integrations;
those legacy adapters can migrate only when their distinct semantics are
preserved and tested. OSC 7501 program status and conversation-delivered agent
mail are unchanged.
