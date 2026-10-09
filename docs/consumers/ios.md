---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-20
---

# iOS client

**TL;DR.** The iOS client is forthcoming. The requirements below are for
mobile integration; they do not establish release availability.

## Minimum `PHUX_REV`

`phux-mobile` consumes the `uniffi` lane of
[`crates/phux-client-ffi`](../../crates/phux-client-ffi) as one
revision-pinned artifact (ADR-0133, ADR-0135). Its pin must be at least
`7093116a955103cbd1a606e1fcfa2694c6a1625f` for the cwd/command/exit status
effects (`KernelStatus::Cwd`, `CommandStarted`, `CommandFinished`, `Exited`)
and `bbccc3ad88739d6c37aec985d2407dc41b92631a` for attach roles
(`RolePolicy`, ADR-0127; the `uniffi` lane declares it with
`set_attach_viewer`).
Neither surface has a mobile consumer yet.
