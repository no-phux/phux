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

The source-side `src/tests/navigation.test.mjs` suite also checks boot/transition
command order and the exact navigation payload following the commit marker.

The first and third tests were observed failing against `81d78dff` before the
fix. Run `just cockpit-test` for the same-checkout FFI and compiled-core gate.
