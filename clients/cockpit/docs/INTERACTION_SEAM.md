---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Committed interaction seam

**TL;DR.** Core modality commits before native effects. Painting consumes
presentation and reports geometry; it does not establish input ownership or
write canonical workspace preferences.

## Ownership

- Phux owns confirmed shared workspace topology. Cockpit's native engine owns
  local placement, preferences, attachment readiness, and terminal input fences.
- The compiled TypeScript core owns the app-wide palette/settings mode. Only
  the active window displays that modal; terminal input is suspended across all
  windows while it is open.
- Chrome selection and tab placement may be speculative until an engine
  snapshot arrives. Painting that projection cannot write canonical placement
  or bypass a rejected intent. Measurements still use the displayed markup and
  the existing single geometry resolver.

## Commit delivery

`initialModel` and every core transition that changes modality emit an empty
`cockpit.committed` host command before their other commands. Palette edits also
emit it before navigation requests; repeated delivery is idempotent.

The pinned SDK commits its core root before walking the command batch. The
native bridge reads `Adapter.Host.model()` in that synchronous callback. The
UiApp model mirror still contains the previous value during the command walk,
so it must not be used here. No second mode payload crosses this seam.

The bridge projects the committed mode and calls `Engine.setInputSuspended`.
Existing owner-fenced cleanup retires captures, held-input state, and remote
focus before subsequent effects run. This marker does not advance workspace
revision or announce a chrome invalidation. Ordinary terminal output stays on
the existing native paint path.

This is a modality-transition contract, not a universal SDK commit hook.
Future changes to `paletteOpen` or `settingsOpen` must deliver the marker,
including model-only returns. The SDK restricts command construction to
`initialModel`/`update`; a helper cannot wrap or forward a `Cmd` value.

During SDK replay, host sends are suppressed. The event/replay adapter and
fallback-key entry read the committed mode directly instead. This path only
updates the bridge's routing projection: it does not invoke the live engine's
focus/capture cleanup. Replayed terminal input, viewport pumping, and lifecycle
focus effects are blocked at the extension, so recovering modal routing cannot
send traffic to an attached live provider.
Cold replay still registers channel and PTY effects with the replay executor
so journaled results have matching slots. Registration does not open a provider
transport or a real process; the workspace-refresh timer also skips live
provider refresh while replay is armed.

## Native publication transactions

Native event handling and persistence completion capture a small publication
checkpoint before mutation. The checkpoint compares active-window/focused
terminal identity and persistence failure state after the transition. A focus
change publishes chrome and advances the existing positional target fence;
persistence feedback publishes status without invalidating those targets.
An intent/provider commit that already announced the same dispatch supplies
its own sequence and revision, avoiding duplicate event announcements. A
refused command advances only sequence: any accompanying window/focus change
still receives a target revision and publication, without erasing the refusal.

These first observed domains complement the existing phase/title/cwd/attention
fingerprints. They do not scan terminal cell content or change the native output
path. A quiet split click must update the tab's projected title immediately;
ordinary output in the same pane must not request another chrome snapshot.

## Identity-qualified tab selection

Tab chrome captures the native target bytes in its painted event. The target
qualifies the existing tab ID by native window slot, window epoch, and tab-ID
allocation generation. Native resolves all components before adopting a window;
title, directory, attention, and tab reordering cannot retarget a held click.
Closed/reused windows and retired tabs reject without changing focus. Epochs
and allocation generations saturate; exhausted identities reject rather than
alias an earlier lifetime. These are process-local presentation identities.
Snapshot extension `tab_contexts` carries the five native window contexts beside
the independent agent-row record. Both the tab strip and side rail retain the
same captured target; agent summaries do not acquire selection authority.

`cockpit.tab-command` has one outstanding request and a 16-entry core FIFO,
including the outstanding request. Every queued entry owns its original target
and full-u64 command ID. A dedicated native completion slot returns applied or
rejected with the exact command ID, reason, and resulting sequence/revision.
Snapshot/navigation requests cannot overwrite it; invalidations cannot settle
commands. The core cancels unsent selections on an unknown/malformed outcome,
reports that uncertainty, and never automatically retries. A fresh explicit
selection receives a new ID. Exhausted command IDs require a new app lifetime.

## Provider-qualified catalog selection

Catalog rows carry their own versioned opaque target bytes. Pointer and
accessibility events capture those bytes from painted markup; keyboard submit
uses the current highlighted row. The core copies a target into the same FIFO
as tab selections. Filtering, pagination, metadata, and placement movement
cannot turn a held target into a different resource. Native resolves current
placement only after validating the full identity and its context. A retired
identity or replaced provider/host/connection rejects before adopting focus or
superseding earlier pending selection.

The catalog target contains the complete provider-qualified `TerminalRef`, or
the existing Phux session ID. Provider and host instance contexts are
process-local monotonic u64 allocations, shared across provider constructors;
exhaustion refuses a new instance rather than reusing a value. Connection epochs
are checked u64 generations. No pointer, hash, JS number coercion, or display
index participates in identity. Local terminal IDs retain all 64 bits, remote
IDs retain their kind, ID, and up to 255 exact host bytes. These contexts are
presentation authority for this process, never durable work IDs.

Retained rows during reconnect keep the provenance of their old inventory:
session catalogs retain their publication epoch, and placed terminals retain
their published owner's epoch. An old row painted after reconnect starts must
not acquire the new connection's authority. Available rows require membership
in the current provider catalog. Resolution also checks the placed replica's
epoch before focusing it.

The internal little-endian command packet is version 1, kind (1 tab, 2
catalog, 3 operation), command ID u64, then target bytes. Kind 3 carries the
12-byte revision-fenced intent for creation, split, close, or shared reorder.
Tab packets remain 32 bytes.
Catalog packets are at most 308 bytes; their target layout is:

| Bytes | Meaning |
|---|---|
| 0, 1 | Target version 2; resource kind 0 local, 1 Phux terminal, 2 session |
| 2..10 | Provider ID u64 |
| 10..18, 18..26, 26..34 | Provider instance, host instance, inventory epoch u64 |
| 34 onward | Local ID u64; or remote kind u32 + ID u32 + host length u8 + host bytes; or session ID u32 |

Local host/epoch fields are zero. Catalog pages retain the revision/query/offset
read fence and the 4096-byte response bound. A row record is display index u16,
label length u8, target length u16, opaque target bytes, and bounded UTF-8 label.
The index is only for page/display bookkeeping. The shipping core does not emit
the compatibility positional navigation intent.

The 27-byte receipt remains version, status, reason, command ID u64, sequence
u64, revision u64. Status 1 is `applied`, 2 `rejected`, and 3
`accepted_pending`; both non-rejections have reason zero. Applied means an
existing presentation was selected synchronously, a local scratch command was
applied, or the already confirmed current session was selected idempotently.
Pending means durable creation, attachment, shared admission/edit, or session
switching was accepted. It has a distinct core outcome
and visible notice, and frees the admission FIFO slot.

## Eventual outcomes and optional presentation

`Creation.Pending` and `shared_mutations.Pending` reserve completion storage
before provider effects. Each existing coordinator has 16 slots; an unconsumed
completion still occupies its slot, but does not count as active work or reserve
pane capacity. A full owner rejects a new command before dispatch. These are
process-local receipts about Phux work, not a second durable work store.

The original command ID follows creation, attachment, shared admission, and
every retirement path. Original execution, follow-up resource attachment, and
shared mutation retain their separate request IDs and connection epochs. A
successful spawn remains successful if attachment or placement later fails.
Matching provider evidence observed before disconnect is recorded before only
the unresolved remainder becomes unknown. Missing confirmation never means
rollback, destruction, or permission to retry the original command.

Operation, shared-mutation confirmation, local placement, and optional focus
are independent result fields. Publishing a selection/placement hint does not
establish placement. Completion verifies the exact live terminal, winning
shared topology, adopted projection, and native destination lifetime. A newer
explicit selection can supersede focus without failing successful work or
placement. Concurrent singleton hints are offered across projection passes.
Unknown shared outcomes do not set the shared refusal flag.

Session navigation reserves the same creation entry before leaving the old
session. Only its explicit unbound handoff survives intentional teardown; the
replacement epoch is bound once. The entry owns exact provider/host context and
up to 4096 server-identity bytes (larger identities reject before effects).
Confirmed attachment and a successful shared projection complete session-only
selection, including an empty session. A terminal target continues attachment
or admission in that same entry. Session lifecycle attachment is not a resource
operation request; no resource request ID is fabricated for it.

`cockpit.command-results` is an independent read/ack bridge slot. A request is
`[1, 0]` to read, or version 1, source byte, command ID u64 to acknowledge a
previously decoded result and read the next. Sources are UI creation (1), UI
shared edit (2), and native shared edit (3). Native shared commands use existing
coordinator tickets, in a separate namespace from full-u64 UI command IDs.
Repeated acknowledgements are harmless; cancellation never cancels execution.

An empty reply is `[1, 0]`. A result is a 74-byte little-endian header plus at
most 273 bytes of full provider-qualified terminal identity:

| Bytes | Meaning |
|---|---|
| 0..6 | Version, source, operation, placement, focus, typed reason |
| 6..14, 14..22 | Command ID and original operation epoch u64 |
| 22..26, 26..34 | Original request u32 and mutation ticket u64 |
| 34..42 | Resource attachment and placement request IDs u32 |
| 42..50 | Provider error domain and code u32 |
| 50..66 | Attachment and placement epochs u64 |
| 66, 67 | Optional mutation outcome (0 absent), reserved zero |
| 68..72, 72..74 | Target session ID u32, terminal byte length u16 |

Operation tags are success (1), refused/not confirmed (2), and unknown (3).
Placement tags are placed (1), refused (2), destination lost (3), unknown (4),
and not requested (5). Focus tags are focused (1), superseded (2), and not
requested (3). Native retains the result until the compiled core decodes and
acknowledges that exact source and command. Failed reads may be repeated;
original commands are never automatically replayed.

The core keeps a bounded 16-result recent history and one retained exception
notice. Successful work stays quiet. Delivery errors have a separate transient
notice; duplicate receipts cannot erase or roll back an operation exception.
An invalidation arriving during a read schedules another read, so an empty
in-flight reply cannot swallow the only completion wake.

## Acceptance evidence

The shipping extension tests exercise the compiled core and native bridge:

- `shipping overlay commit suspends remote focus and input before a frame`:
  actual settings open/close messages suspend and restore remote focus; text,
  key release, pointer, and file-drop paths cannot reach the terminal meanwhile.
- `committed palette owns input even when an older model is painted`:
  stale painting cannot reopen terminal input or change Escape's destination.
- `painting speculative placement cannot bypass a refused intent`:
  painting a speculative side strip after rejection preserves canonical
  placement, sequence, and revision.
- `replayed modality routes fallback keys without live terminal effects` and
  `committed app modal blocks raw pointers across native windows` cover replay
  recovery and the decorated host's all-window routing.
- `cold replay registers provider and PTY results without live startup` checks
  replay armed before the first lifecycle/frame event.
- `persistence failure and recovery publish without a later command` checks
  status-only publication without a positional revision change.
- `quiet split pointer focus publishes chrome without terminal output` checks
  focus-dependent titles and ordinary-output publication silence.
- `refused command window adoption still fences ambient targets` checks the
  independent revision fence after refusal and successful-command deduplication.
- `held painted tab action follows identity after metadata and reorder` checks
  an event captured from compiled markup, delivered after replacement snapshots.
- `tab command receipt survives snapshot and navigation requests and rejects
  reused windows` checks completion independence, exact high command-ID bits,
  and refusal without focus mutation.
- `retired tab targets cannot alias reused IDs after allocation rollover`
  checks tab and window lifetime exhaustion.
- `navigation held painted catalog target follows metadata filtering and window
  movement` checks a compiled markup event across replacement pages and native
  placement movement.
- `navigation rejected captured identity preserves focus pending selection and
  receipt slot` checks exact high command-ID bits and independent completion.
- `navigation catalog receipts distinguish shared admission and current session
  application` uses the Rust-produced provider fixtures for truthful admission.
- `navigation retained rows cannot acquire replacement connection authority`
  checks rows captured during reconnect before reused numeric IDs publish.

The source-side `src/tests/navigation.test.mjs` suite also checks boot/transition
command order and the exact navigation payload following the commit marker.
`src/tests/tab-commands.test.mjs` checks FIFO capacity, captured-data ownership,
exact receipt matching, unknown delivery, command-ID carry/exhaustion, mixed
tab/catalog ordering, and observably distinct pending admission.

The first and third tests were observed failing against `81d78dff` before the
fix. Run `just cockpit-test` for the same-checkout FFI and compiled-core gate.
