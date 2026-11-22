//! The dispatch guard: [`enforce`] behind three entry points
//! (`docs/remote.md#who-can-do-what`).
//!
//! The classification is [`phux_protocol::kinds`]; this module resolves each
//! row's subject against state (side-effect-free) and checks the grant's
//! clauses. Absent and unauthorized targets are refused identically. A Group
//! here is a session; the opaque `GroupId(1)` resolves to the local host.

use phux_core::ids::ResourceId as CoreResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::kinds::{
    Classification, Exemption, Subject, Verb, Verbs, classify_command, classify_frame,
};
use phux_protocol::scope::{EffectiveScopeSet, Selector};
use phux_protocol::wire::frame::{
    AttachTarget, Command, FrameKind, Scope, decode_session_keep_empty,
};

use super::hold::Admission;
use super::{Authority, ConnectionGrant, GroupPoint, POLICY_TARGET};
use crate::state::{ClientId, ServerState};

const OBSERVE: Verbs = Verbs::of(&[Verb::Observe]);
const CREATE: Verbs = Verbs::of(&[Verb::Create]);
const BIND: Verbs = Verbs::of(&[Verb::Bind]);
include!("enforce/dispatch.rs");
include!("enforce/subjects.rs");
include!("enforce/resolve.rs");
