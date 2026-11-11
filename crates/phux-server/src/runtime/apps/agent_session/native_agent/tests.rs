//! Exercise the production broker/record actor boundary, without a provider
//! model or an echo adapter. Host credentials are real narrowed delegations.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "isolated broker regressions"
)]
use super::binding::NativeBinding;
use super::binding::fingerprint;
use super::closure::*;
use super::controller::*;
use super::publication::*;
use super::source_dispatch::*;
use crate::runtime::commands::CommandContext;
use crate::state::ClientId;
use crate::state::SharedState;
use bytes::Bytes;
use phux_agent_session::session::AgentSessionActor;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::NATIVE_HOST_SPEC_ENV;
use phux_protocol::native_agent::NativeCapabilities;
use phux_protocol::native_agent::NativeCommand;
use phux_protocol::native_agent::NativeCommandResult;
use phux_protocol::native_agent::NativeDisposition;
use phux_protocol::native_agent::NativeIntent;
use phux_protocol::native_agent::NativeOption;
use phux_protocol::native_agent::NativePublicationReceipt;
use phux_protocol::native_agent::NativeQuestion;
use phux_protocol::native_agent::NativeQuestionItem;
use phux_protocol::native_agent::NativeRequest;
use phux_protocol::native_agent::NativeRequestPayload;
use phux_protocol::native_agent::NativeState;
use phux_protocol::native_agent::NativeUpdate;
use phux_protocol::native_agent::NativeView;
use phux_protocol::native_agent::SpawnNativeAgent;
use phux_protocol::native_agent::SpawnedNativeAgent;
use phux_protocol::wire::frame::CommandResult;
use phux_protocol::wire::frame::CommandValue;
use phux_protocol::wire::frame::ErrorCode;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
mod answer_reservations;
mod append_effects;
mod closure;
mod command_admission;
mod fixture;
mod integrations;
mod publication_recovery;
mod receipts_and_lifecycle;
mod viewer_spawn;
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::policy::ConnectionGrant;
use fixture::*;
use phux_agent_session::session::AgentSessionResource as _;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{Command, FrameKind, RESOURCE_ASKED_KEY, Scope};
use tokio::sync::mpsc;
