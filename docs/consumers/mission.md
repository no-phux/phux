---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-10-15
---

# Mission control

**TL;DR.** Mission control is a client composition for tasks. It is decided
and not built; the decision lives in the architecture record. The verb that
starts a task's agent is `start`.

<!-- impl-status: spec-only; probe: PhuxMissionStore -->
> **Status: spec-only.** No `phux mission` verb and no mission store exist
> in this tree. Clients will decode `phux.mission/v1` once that schema
> lands. The decision is
> [ADR-0173](../adr/0173-mission-control-is-a-client-composition.md).
