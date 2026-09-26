---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-25
---

# Close windows before process exit

**TL;DR.** `resetRender()` now closes a live macOS window before forgetting the
Solid root. Dropping an open GPUI window during process-exit thread-local
teardown panics in the profiler journal. Apply this after patch 3. It does not
change Rust or the addon.

## Provenance

GPUIX `6d5e6887ad8dc6e94eb66043394f3d17a462c56a`, Zed
`81c99f816b4a5f69d3c014774068034c24d1d7af`. No dependency or lockfile changes.
The panic is `ForegroundJournalWriter` access from `WindowProfiler`'s drop while
`ApplicationHandle` thread-local destruction is already running. Closing the
window first records that event while the journal local is alive. A direct
reproduction exits 0 after `closeWindow()` and 134 without it.
