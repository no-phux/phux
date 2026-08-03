//! The satellite half of the machine registries (ADR-0038, ADR-0066).
//!
//! The user-facing verbs live in [`super::host`]: `phux host add|ls|rm|enroll
//! --role satellite` operates on the `[[satellites]]` registry through the
//! [`registry`] module below. The old `phux satellite` spelling survives one
//! release as a hidden deprecated alias that dispatches through the same
//! `host` implementation (see `host::run_deprecated_alias`).

// `pub(crate)` (not private): `phux host` — the ADR-0066 umbrella verb —
// delegates to this registry module rather than growing a merged store.
pub(crate) mod registry;
