---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-15
---

# iOS client

**TL;DR.** Coming soon.

---

Coming soon. An Android client is coming as well.

## Minimum `PHUX_REV`

`phux-mobile` consumes `phux-client-core` through UniFFI directly, not
Cockpit's stable C ABI (`phux-client-ffi`), which stays independent
(`phux-mobile` `rust/phux-mobile-ffi/Cargo.toml`). Two phase-2 surfaces
matter once a client adopts them, so `phux-mobile` re-pins `PHUX_REV`
rev-for-rev ([ADR-0035](../adr/0035-agent-asked-event.md)) to at least:

- `7093116a955103cbd1a606e1fcfa2694c6a1625f` — cwd, command, and exit
  status effects: `KernelStatus::Cwd`, `CommandStarted`, `CommandFinished`,
  and `Exited` (`crates/phux-client-core/src/session.rs`), part of the
  same kernel contract UniFFI exposes; Cockpit's remote provider consumes
  the FFI mirror of these ([`cockpit.md`](./cockpit.md), "Status effects").
- `bbccc3ad88739d6c37aec985d2407dc41b92631a` — attach roles: the
  `role_policy` byte `phux_protocol::wire::frame::RolePolicy` carries on
  `ATTACH_RESOURCE` and session `ATTACH` (ADR-0127). This is a protocol
  field declared at attach, not a `KernelStatus` variant, and
  `phux-client-core` does not yet expose a way to set it.

Neither surface has a `phux-mobile` consumer yet; this names the floor for
when one lands, not a claim that one exists.
