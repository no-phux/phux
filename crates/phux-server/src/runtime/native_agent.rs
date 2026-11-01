//! Native launch transaction and exclusive authenticated controller broker.
use super::commands::CommandContext;
use crate::state::native_agent::{NativeBinding, NativePublication, fingerprint};
use crate::state::{ClientId, SharedState};
use bytes::Bytes;
use phux_agent_session::session::AgentSessionActor;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{
    MAX_NATIVE_OPERATIONS, NATIVE_HOST_SPEC_ENV, NATIVE_SCHEMA, NativeCapabilities, NativeCommand,
    NativeCommandResult, NativeDisposition, NativeIntent, NativePublicationReceipt, NativeRequest,
    NativeRequestPayload, NativeUpdate, NativeView, SpawnNativeAgent, SpawnedNativeAgent,
    validate_payload,
};
#[cfg(test)]
use phux_protocol::native_agent::{NativeOption, NativeQuestion, NativeQuestionItem, NativeState};
use phux_protocol::wire::frame::{AgentEvent, CommandResult, CommandValue, ErrorCode};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Notify;
#[cfg(test)]
use tokio::sync::oneshot;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

include!("native_agent/controller.rs");
include!("native_agent/closure.rs");

fn spawn(ctx: &CommandContext, spec: &SpawnNativeAgent) -> CommandResult {
    spawn_transaction(ctx, spec, None).map_or_else(|error| error, |spawned| json(&spawned))
}

mod integrations;

include!("native_agent/launch.rs");
include!("native_agent/publication.rs");
include!("native_agent/source_dispatch.rs");

#[cfg(test)]
mod tests;
