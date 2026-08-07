//! The satellite half of the machine registries (ADR-0038, ADR-0066).
//!
//! The user-facing verbs live in [`super::host`]: `phux host add|ls|rm|enroll
//! --role satellite` operates on the `[[satellites]]` registry through the
//! [`registry`] module below. The former `phux satellite` verb tree was
//! absorbed into `phux host` (ADR-0066) and removed in v0.12.1 once its
//! deprecation window closed (phux-dpjf).

// `pub(crate)` (not private): `phux host` — the ADR-0066 umbrella verb —
// delegates to this registry module rather than growing a merged store.
pub(crate) mod registry;
