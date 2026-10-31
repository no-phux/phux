//! [`phux_core::apply`]: each command's effect and each of its refusals.
//! The harness asserts that every refusal leaves the organization
//! unchanged and that every success preserves all invariants.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use phux_core::{
    ActivationId, Actor, ArrangementOwner, Attrs, Command, Effect, EntryRef, Event, ExecState,
    ExitOutcome, Group, LaunchMode, LayoutNode, NewResource, Op, Outcome, ProcIdentity, Refusal,
    Reply, ResourceKey, ResourceKind, Revision, SessionKey, SessionLifetime, SourcePin, SplitDir,
    TerminateMode, Verb,
};

use crate::support::{Harness, created, job, shell};

const A1: ActivationId = ActivationId::FIRST;

const fn ended(resource: ResourceKey, outcome: Outcome) -> Op {
    Op::ActivationEnded {
        resource,
        activation: A1,
        state: ExecState::Completed,
        diagnostic: None,
        outcome,
    }
}

const fn exited() -> Outcome {
    Outcome::Exit(ExitOutcome::exited(0))
}

fn create(name: &str, lifetime: SessionLifetime, resource: Option<NewResource>) -> Op {
    Op::SessionCreate {
        name: name.to_owned(),
        lifetime,
        attrs: Attrs::default(),
        resource,
    }
}

mod execution;
mod organization;
mod sessions;
