//! The grant engines and the dispatch guard, below the wire
//! (`docs/spec/workload-auth.md` §5-§8).
//!
//! The verb-only matrices derive every expectation from the classifier's own
//! row for the sample, so they pin the guard to `phux_protocol::kinds` rather
//! than to a second table: for each of the six verbs, a grant of only that
//! verb at Global admits exactly the rows that need only that verb and are
//! not owner-socket rows, admits the liveness and cleanup exemptions, and
//! refuses everything else.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::Utc;
use phux_core::ids::{ResourceId as CoreResourceId, SessionId as CoreSessionId};
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::{
    BootstrapId, FileUploadId, GroupId, InputOperationId, ResourceId as WireResourceId,
    SatelliteHost, SessionId as WireSessionId, StreamId,
};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::kinds::{
    COMMAND_RULES, Exemption, FRAME_RULES, Requirement, Verb, Verbs, frame_rule,
};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::{EffectiveScopeSet, TerminalScopeSet};
use phux_protocol::wire::frame::{
    AttachTarget, CONFIG_RELOAD_KEY, Command, FrameKind, InputMode, KillPrecondition,
    ListenerTransport, PerfOp, ReportedAgentState, SESSION_CREATE_KEY, SESSION_KEEP_EMPTY_KEY,
    Scope, SpawnResource, StateScope, TerminalSignal, ViewportInfo, WHOAMI_KEY,
    encode_session_keep_empty,
};
use phux_protocol::wire::info::SessionSnapshot;

use super::{
    Authority, ConnectionGrant, PolicyPosture, PostureError, PostureInputs, Request, ScopedPolicy,
    enforce,
};
use crate::auth::AuthenticatedCredential;
use crate::state::{ClientId, ServerState};
use crate::workload::{ReloadingWorkloadRegistry, WorkloadRegistry};

const CLIENT: ClientId = ClientId(7);
include!("tests/world_samples.rs");
include!("tests/verb_matrices.rs");

// -----------------------------------------------------------------------------
// The §6 tables, row by row: each row's verbs on its own subject selector.
// -----------------------------------------------------------------------------

/// One §6 row as the guard must decide it: `frame` reaches the row; every
/// grant set in `admit` is admitted; every set in `deny` is refused.
struct Row {
    frame: FrameKind,
    admit: Vec<Vec<String>>,
    deny: Vec<Vec<String>>,
}

fn row(frame: FrameKind, admit: &[&[&str]], deny: &[&[&str]]) -> Row {
    let sets = |sets: &[&[&str]]| -> Vec<Vec<String>> {
        sets.iter()
            .map(|set| set.iter().map(|scope| (*scope).to_owned()).collect())
            .collect()
    };
    Row {
        frame,
        admit: sets(admit),
        deny: sets(deny),
    }
}

/// `set` with `verb` taken out of every grant; a grant left with no verb
/// is dropped.
fn without_verb(set: &[String], verb: Verb) -> Vec<String> {
    let name = phux_protocol::scope::verb_name(verb);
    set.iter()
        .filter_map(|scope| {
            let (verbs, selector) = scope.split_once('@').unwrap();
            let verbs: Vec<&str> = if verbs == "*" {
                Verb::ALL
                    .iter()
                    .map(|verb| phux_protocol::scope::verb_name(*verb))
                    .collect()
            } else {
                verbs.split(',').collect()
            };
            let kept: Vec<&str> = verbs.into_iter().filter(|kept| *kept != name).collect();
            (!kept.is_empty()).then(|| format!("{}@{selector}", kept.join(",")))
        })
        .collect()
}

/// Whether the grant `set` spells admits `frame`; an empty set grants
/// nothing.
fn admits_set(world: &World, set: &[String], frame: &FrameKind) -> bool {
    if set.is_empty() {
        return false;
    }
    let scopes: Vec<&str> = set.iter().map(String::as_str).collect();
    admits(world, &scoped(&scopes), frame)
}

include!("tests/subject_rows.rs");
include!("tests/subject_assertions.rs");
include!("tests/engines.rs");
include!("tests/grant_edge_cases.rs");
include!("tests/snapshots_events.rs");

mod delegation;
include!("tests/keyed_selectors.rs");
