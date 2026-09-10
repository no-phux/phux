---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-10
---

# Cockpit canvas implementation

**TL;DR.** Keep the TypeScript coordinator and compiled markup as the shipping
chrome, retain the native engine as the sole terminal geometry/identity owner,
and extend bounded navigation and appearance seams. Validate the real compiled
tree and provider behavior before accepting reference captures as layout evidence.

## Context

Baseline: `747dfde0`. User behavior is specified in [PRODUCT.md](PRODUCT.md).

- `clients/cockpit/src/app.native` and `src/windows/components/` declare chrome.
- `src/core.ts` owns navigation/modal presentation and effects-as-data.
- `src/native_extension.zig` measures `phux-terminal-space` from the real tree;
  `workspace_projection.zig` derives paint, hit targets and provider viewports.
- `src/cockpit/native/ts_navigation.zig` has revision-qualified catalog indices
  and bounded pages. Provider resource identities carry satellite host names;
  no complete host inventory is exposed by the FFI.
- `src/config/config.zig` supports comment-preserving `setKey` edits and explicit
  override precedence. The existing settings path applies themes only on Save.

## Proposed changes

1. Compose chrome from shared templates where practical, retaining stable
   `select_target` payloads and exact tab accessibility semantics. Use token
   surfaces and accent indicators, compact connection context and navigable
   overflow. Keep the compiled terminal-space measurement as geometry authority.
2. Extend navigation with bounded category and exact-host filtering, truthful
   known-host enumeration, structured result presentation and connected context.
   Preserve the unfiltered catalog index plus revision activation fence.
   Ported over #573: activation now uses main's captured provider-qualified
   catalog targets through the command FIFO, and known-host rows carry a
   filter token rather than activation authority (see `NAVIGATION_SEAM.md`).
3. Add a native appearance transaction owning the starting configuration and
   live font/placement state. A dedicated request/response seam handles begin,
   preview, cancel and save without abusing revision-qualified terminal intents.
   Read-modify-write only changed supported keys; reject read failures and large
   files. Keep write results available to the page.
   The committed placement is retained in the model while preview is active so
   unrelated topology writes cannot persist it. Existing local emulator cursor
   defaults update without overriding application-selected cursor shapes. A
   successful font save becomes the live reset baseline.
4. Keep settings state separate from topology. Configuration preview and
   cancellation must not create, restart, attach or detach execution. Preview
   changes settle terminal geometry directly and never tween it.
5. Use the existing SDK controls for hover/press feedback. New custom transition
   work must consume platform accessibility preferences and must not mutate grid
   extents per animation frame.

## Testing and validation

- `bash scripts/doctor.sh cockpit`, then baseline and final `just cockpit-test`
  with same-worktree Rust FFI and private Zig caches.
- Navigation codec/projection tests: exact host filters, host/title/directory
  search, unknown ownership, bounded paging and stale activation (behavior 3-6).
- Appearance tests: no preview writes, full rollback, modified-key persistence,
  missing destination, read/write refusal and explicit overrides (behavior 8-11).
- Compiled-tree layout/accessibility sweep across declared window sizes,
  top/side placement, navigation/settings states and secondary windows (1-8,12).
- Shipping TypeScript compilation, `zig fmt --check` on touched Zig, docs gate,
  before/after complexity measurement on touched branching functions.
- Isolated native dev app: assert publisher PID and markup watcher; capture
  navigation/settings/canvas reference images and drive focus return. Real host
  raster evidence is separate: a CPU reference image cannot prove CoreText ink.
- Independent fresh-context diff review; fix findings and rerun affected checks.

## Work ownership

One parent owns product decisions, settings and chrome. A separate isolated
navigation lane may implement the protocol/projection changes and focused tests;
the parent integrates and validates the combined shipping graph. Read-only
review remains independent. No upstream publication is part of this task.

## Implementation evidence

The navigation lane is `532f259a` (integrated from `a999c3b5`). Two independent
read-only reviews identified and verified fixes for stale painted actions,
refused navigation superseding accepted focus, preview persistence leakage,
font/cursor application, and configuration refusal edge cases.

Complexity uses TypeScript AST branch counting (TypeScript 5.9.3) and Lizard for
Zig, compared with `747dfde0`. New helpers are at most 10; the existing framework
`update` dispatcher drops from 86 to 76 while retaining commands in the only
return position the AOT compiler supports. Native `runFor` drops 9 to 6.
Appearance persistence was split during implementation: `destination` 14 to 4,
`persist` 15 to 5; parsing, path resolution and atomic replacement are separate
helpers. This is a scoped improvement, not a claim that all legacy functions
meet the default threshold.
