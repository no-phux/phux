# Public terminal workflow

Create a terminal in a session, place it, execute argv, and keep a receipt
without implementing phux-specific layout publication or rollback. This is
an implemented vertical slice, not a replacement for every existing verb.

| Intention | CLI | MCP | Rust |
| --- | --- | --- | --- |
| Describe this server | `phux terminal describe` | `phux_api_describe` | `phux_client::terminal::describe` |
| Create, place and start | `phux terminal create --request-json JSON` | `phux_terminal_create` | `phux_client::terminal::create` |
| Recover a receipt, without effects | `phux terminal operation --session KEY --idempotency-key HEX32` | `phux_terminal_operation` | `phux_client::terminal::operation` |
| Reconcile a retained resource | `phux resource show --json ^FULL_RESOURCE_KEY` | `phux_resource_show` with `target: "^FULL_RESOURCE_KEY"` | `phux_client::resource::show::show_key` |

All three CLI commands emit versioned JSON. Errors are JSON on stderr with a
stable code; creation errors preserve the retry key and distinguish not sent,
unknown outcome and a committed but incomplete receipt. Success means intent
committed, **not** process exit or successful task completion.

## First use

The server must already be running. Start from its description and the durable
session keys in `phux ls --json`, not mutable names or a remembered `@N`:

```sh
phux terminal describe
session=$(phux ls --json | jq -r '.sessions[] | select(.name == "work") | .key')
key=$(openssl rand -hex 16)
request=$(jq -n --arg session "$session" --arg key "$key" \
  '{session: $session, idempotency_key: $key, argv: ["/bin/sleep", "30"]}')
phux terminal create --request-json "$request"
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
compensating kill. After a restart, `failed` together with outcome
`unobserved` means the launch intent was **committed, never launched**. It is
not a completed process whose exit went unseen (`completed` + `unobserved`).
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
activation, not the historical activation of a creation operation. Compare the
saved activation and instance before attributing a result to that operation.
A session-scoped grant loses the read after a move outside that session; a
resource-key grant follows the resource. The original operation lookup remains
fenced to its creation activation and destination session, and never adopts a
replacement. Receipt expiry does not remove the resource's retained result.

The MCP adapter also has its separate in-memory mutation ledger. It forwards
the creation key to the server, but its pending operation receipt and
`phux_operation_status` do not become durable. `phux_terminal_operation`
queries the server, not that adapter ledger.

## Discovery, authorization and events

`terminal describe` asks the **connected server** for supported public
workflows, complete nested input/output schemas, completion meanings and
retry retention. It does not enumerate new server methods from an old
client's compiled catalog. These schemas derive from the same public DTOs
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
server dispatch. Placed creation requires `CREATE` and `ORGANIZE` on the
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
