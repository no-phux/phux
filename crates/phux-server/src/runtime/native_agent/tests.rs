//! Exercise the production broker/record actor boundary, without a provider
//! model or an echo adapter. Host credentials are real narrowed delegations.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "isolated broker regressions"
)]
use super::*;
mod append_effects;
mod closure;
mod integrations;
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::policy::ConnectionGrant;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{Command, FrameKind, RESOURCE_ASKED_KEY, Scope};
use tokio::sync::mpsc;

include!("tests/fixture.rs");
include!("tests/command_admission.rs");
include!("tests/publication_recovery.rs");
include!("tests/receipts_and_lifecycle.rs");
include!("tests/answer_reservations.rs");
include!("tests/viewer_spawn.rs");
