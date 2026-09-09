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

This route selects an existing presentation synchronously. Durable creation,
shared close, catalog navigation, and their placement continuations still use
their existing contracts. Their later command receipts must distinguish
admission from eventual execution/placement; queueing provider work is not an
applied presentation result.

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

The source-side `src/tests/navigation.test.mjs` suite also checks boot/transition
command order and the exact navigation payload following the commit marker.
`src/tests/tab-commands.test.mjs` checks FIFO capacity, captured-data ownership,
exact receipt matching, unknown delivery, and command-ID carry/exhaustion.

The first and third tests were observed failing against `81d78dff` before the
fix. Run `just cockpit-test` for the same-checkout FFI and compiled-core gate.
