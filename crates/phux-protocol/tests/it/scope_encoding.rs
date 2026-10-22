//! Canonical scope encoding and the registry grammar
//! (`docs/spec/workload-auth.md` §5).
//!
//! The goldens pin the §5 bytes, and every refusal rule the spec lists gets
//! its own image, so a lenient decoder cannot hide behind a normalizing
//! round trip.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use phux_protocol::scope::{Host, ScopeGrant, TerminalScopeSet};

fn host(name: &str) -> Host {
    Host::new(name).expect("valid host")
}

fn grant(text: &str) -> ScopeGrant {
    ScopeGrant::parse(text).expect("valid grant")
}

fn set(texts: &[&str]) -> TerminalScopeSet {
    TerminalScopeSet::from_grants(texts.iter().map(|text| grant(text))).expect("valid set")
}

mod canonical;
mod effective;
mod grammar;
mod keyed;
