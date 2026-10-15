//! SDK controller reservations, operation identity, and upgrade recovery.
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use super::{ClientId, ServerState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOperation {
    pub command: NativeCommand,
    pub result: NativeCommandResult,
    pub delivered: bool,
    pub requester: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePublication {
    pub id: u64,
    pub fingerprint: String,
    pub receipt: Option<NativePublicationReceipt>,
    pub view: Option<NativeView>,
}
/// Private upgrade state; never exported as organization launch intent.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeBinding {
    pub launch: SpawnNativeAgent,
    pub agent: ResourceId,
    pub parent: ResourceId,
    pub activation: String,
    pub proof: String,
    pub credential: String,
    pub host_pid: Option<u32>,
    pub generation: u64,
    pub view: NativeView,
    pub closed: bool,
    pub operations: BTreeMap<u64, NativeOperation>,
    pub pending: VecDeque<u64>,
    pub answered: BTreeMap<String, u64>,
    pub publications: VecDeque<NativePublication>,
    pub publication_high_water: u64,
}
impl std::fmt::Debug for NativeBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBinding").field("agent", &self.agent).field("parent", &self.parent).field("generation", &self.generation).finish_non_exhaustive()
    }
}
#[derive(Debug, Default)]
pub(crate) struct NativeAgents {
    pub bindings: BTreeMap<ResourceId, NativeBinding>,
    pub wakes: BTreeMap<ResourceId, Arc<Notify>>,
    pub sealed: bool,
}
pub(crate) fn fingerprint<T: Serialize>(value: &T) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
impl NativeBinding {
    pub(crate) fn refuse(&mut self, command: NativeCommand, requester: u64, detail: impl Into<String>) -> NativeCommandResult {
        let result = NativeCommandResult { operation_id: command.operation_id, disposition: NativeDisposition::Refused, detail: Some(detail.into()) };
        if command.operation_id != 0 && self.operations.len() < MAX_NATIVE_OPERATIONS && !self.operations.contains_key(&command.operation_id) {
            self.operations.insert(command.operation_id, NativeOperation { command, result: result.clone(), delivered: false, requester });
        }
        result
    }
    pub(crate) fn submit(&mut self, command: NativeCommand, requester: u64) -> NativeCommandResult {
        let refused = |detail: &str| NativeCommandResult { operation_id: command.operation_id, disposition: NativeDisposition::Refused, detail: Some(detail.into()) };
        if let Some(prior) = self.operations.get(&command.operation_id) {
            return if prior.command == command { prior.result.clone() } else { refused("operation identity conflicts with its reserved payload") };
        }
        if command.operation_id == 0 { return refused("operation identity must be nonzero"); }
        if command.activation != self.activation { return self.refuse(command, requester, "activation does not match this agent session"); }
        if self.closed || !self.view.connected { return self.refuse(command, requester, "native controller is unavailable"); }
        if let Err(error) = command.intent.validate() { return self.refuse(command, requester, error); }
        if self.pending.len() >= MAX_NATIVE_PENDING || self.operations.len() >= MAX_NATIVE_OPERATIONS { return self.refuse(command, requester, "native command ledger/queue is full"); }
        let validation = match &command.intent {
            NativeIntent::Prompt { .. } => {
                if !self.view.capabilities.prompt { Err("provider does not support native prompt") }
                else if self.view.turn_id.is_some() && !self.view.capabilities.follow_up { Err("provider does not support a prompt during this turn") }
                else { Ok(()) }
            }
            NativeIntent::Interrupt { turn_id } => {
                if !self.view.capabilities.interrupt { Err("provider does not support native interrupt") }
                else if self.view.turn_id.as_ref() != Some(turn_id) { Err("turn is stale or absent") }
                else { Ok(()) }
            }
            NativeIntent::Answer { question_id, response } => {
                if !self.view.capabilities.questions { Err("provider does not support native questions") }
                else if self.answered.contains_key(question_id) { Err("question already has a reserved responder") }
                else if let Some(question) = self.view.questions.iter().find(|question| &question.id == question_id) {
                    validate_answer(question, response)
                } else { Err("question is stale or absent") }
            }
        };
        if let Err(error) = validation { return self.refuse(command, requester, error); }
        if let NativeIntent::Answer { question_id, .. } = &command.intent { self.answered.insert(question_id.clone(), command.operation_id); }
        let result = NativeCommandResult { operation_id: command.operation_id, disposition: NativeDisposition::Queued, detail: Some("reserved in server controller queue; not provider-admitted".into()) };
        self.pending.push_back(command.operation_id);
        self.operations.insert(command.operation_id, NativeOperation { command, result: result.clone(), delivered: false, requester });
        result
    }
    pub(crate) fn receive(&mut self) -> Option<NativeCommand> {
        let id = self.pending.pop_front()?;
        let operation = self.operations.get_mut(&id)?;
        operation.delivered = true;
        operation.result.disposition = NativeDisposition::Unknown;
        operation.result.detail = Some("dispatched to host; admission not yet acknowledged".into());
        Some(operation.command.clone())
    }
    pub(crate) fn complete(&mut self, result: NativeCommandResult) -> Result<(), String> {
        validate_payload(&result)?;
        let operation = self.operations.get_mut(&result.operation_id).ok_or("unknown operation identity")?;
        if !operation.delivered { return Err("operation was not dispatched to this host".into()); }
        if operation.result == result { return Ok(()); }
        if matches!(operation.result.disposition, NativeDisposition::Completed | NativeDisposition::Refused) { return Err("operation already has a final receipt".into()); }
        if result.disposition == NativeDisposition::Queued && result.detail.as_deref().is_none_or(str::is_empty) { return Err("queued receipt must identify its factual stage".into()); }
        operation.result = result;
        Ok(())
    }
    pub(crate) fn project(&self, update: &NativeUpdate) -> Result<NativeView, String> {
        validate_payload(update)?;
        let mut view = self.view.clone();
        match update {
            NativeUpdate::View { view: next } => {
                if next.activation != self.activation || next.backend != self.view.backend { return Err("view identity does not match reserved activation/backend".into()); }
                if next.questions.len() > MAX_NATIVE_QUESTIONS { return Err("too many native questions".into()); }
                let mut ids = std::collections::BTreeSet::new();
                for question in &next.questions { validate_question(question)?; if !ids.insert(&question.id) { return Err("duplicate question id".into()); } }
                view = next.clone();
            }
            NativeUpdate::QuestionOpened { question } => {
                validate_question(question)?;
                if view.questions.iter().any(|entry| entry.id == question.id) { return Err("question id already open".into()); }
                if self.answered.contains_key(&question.id) { return Err("question id has already been answered".into()); }
                if view.questions.len() >= MAX_NATIVE_QUESTIONS { return Err("too many native questions".into()); }
                view.questions.push(question.clone());
            }
            NativeUpdate::QuestionClosed { id, .. } => { view.questions.retain(|question| &question.id != id); }
            NativeUpdate::State { state, turn_id } => {
                view.turn_id.clone_from(turn_id);
                if *state == NativeState::Done { view.turn_id = None; view.questions.clear(); }
            }
            NativeUpdate::Transcript { entry } => {
                if entry.get("schema").and_then(serde_json::Value::as_str) != Some("phux.transcript/v1") { return Err("transcript entry must use phux.transcript/v1".into()); }
            }
            NativeUpdate::Notice { .. } => {}
        }
        validate_payload(&view)?;
        Ok(view)
    }
}
fn validate_question(question: &NativeQuestion) -> Result<(), String> {
    if question.id.is_empty() { return Err("native question must preserve a nonempty provider id".into()); }
    if !matches!(question.kind.as_str(), "confirm" | "select" | "input" | "editor" | "ask" | "permission" | "form") { return Err("unsupported native question kind".into()); }
    validate_payload(question)
}
fn validate_answer(question: &NativeQuestion, response: &serde_json::Value) -> Result<(), &'static str> {
    if response.get("cancelled").and_then(serde_json::Value::as_bool) == Some(true)
        && response.as_object().is_some_and(|object| object.len() == 1) { return Ok(()); }
    let valid = match question.kind.as_str() {
        "confirm" => response.get("confirmed").is_some_and(serde_json::Value::is_boolean),
        "select" | "permission" => response.get("option_id").and_then(serde_json::Value::as_str).is_some_and(|id| question.options.iter().any(|option| option.id == id)),
        "input" | "editor" => response.get("text").is_some_and(serde_json::Value::is_string),
        "form" => response.get("values").is_some_and(serde_json::Value::is_object),
        "ask" => response.get("answers").and_then(serde_json::Value::as_array).is_some_and(|answers| {
            let mut ids = std::collections::BTreeSet::new();
            answers.len() == question.questions.len() && answers.iter().all(|answer| {
                let Some(id) = answer.get("id").and_then(serde_json::Value::as_str) else { return false; };
                if !ids.insert(id) { return false; }
                let Some(item) = question.questions.iter().find(|item| item.id == id) else { return false; };
                answer.get("selected_options").and_then(serde_json::Value::as_array).is_some_and(|selected| {
                    let mut options = std::collections::BTreeSet::new();
                    (item.multi || selected.len() <= 1) && selected.iter().all(|value| value.as_str().is_some_and(|option| options.insert(option) && item.options.iter().any(|entry| entry.id == option)))
                }) && answer.get("custom_input").is_none_or(serde_json::Value::is_string)
            })
        }),
        _ => false,
    };
    if valid { Ok(()) } else { Err("response does not match the current provider question") }
}
impl ServerState {
    pub(crate) fn native_agents(&self) -> &NativeAgents { &self.native_agents }
    pub(crate) fn native_agents_mut(&mut self) -> &mut NativeAgents { &mut self.native_agents }
    pub(crate) fn native_owned_terminal(&self, parent: phux_core::ids::ResourceId) -> bool {
        self.registry().record(parent).is_some_and(|record| matches!(&record.launch,
            Some(phux_core::LaunchSpec::Terminal(launch)) if launch.env.iter().any(|(key, _)| key == NATIVE_HOST_SPEC_ENV)))
            || self.terminal_wire_of(parent).is_some_and(|wire| self.native_agents.bindings.values().any(|binding| binding.parent == wire))
    }
    pub(crate) fn native_host_identity(&self, client: ClientId) -> Option<(String, u32)> {
        let peer = self.peer_identity(client)?;
        if peer.transport != phux_protocol::policy::TransportType::UnixSocket || peer.uid != rustix::process::geteuid().as_raw() { return None; }
        let credential = self.authenticated_credential(client)?;
        super::super::runtime::delegation::delegated_credential(peer, Some(credential))?;
        Some((credential.id.clone(), peer.pid?))
    }
    pub(crate) fn restore_native_agents(&mut self, blob: &crate::upgrade::blob::StateBlob) {
        self.native_agents = NativeAgents::default();
        for carried in &blob.native_agents {
            if self.terminal_from_wire(&carried.agent).is_none() || self.terminal_from_wire(&carried.parent).is_none() { continue; }
            let mut binding = carried.clone();
            binding.generation = binding.generation.saturating_add(1);
            // Connection grants do not cross a re-exec. Undispatched
            // reservations are refused, not silently executed as a new client.
            // Dispatched operations remain ambiguous until the durable host
            // supplies its operation-ledger receipt.
            binding.pending.clear();
            for operation in binding.operations.values_mut() {
                if matches!(operation.result.disposition, NativeDisposition::Completed | NativeDisposition::Refused) { continue; }
                if !operation.delivered {
                    operation.result.disposition = NativeDisposition::Refused;
                    operation.result.detail = Some("daemon upgraded before dispatch; original connection authority ended".into());
                    if let NativeIntent::Answer { question_id, .. } = &operation.command.intent {
                        binding.answered.remove(question_id);
                    }
                } else {
                    operation.result.disposition = NativeDisposition::Unknown;
                    operation.result.detail = Some("daemon upgraded after dispatch; host receipt required, no automatic replay".into());
                }
            }
            if let Some(publication) = binding.publications.back_mut().filter(|publication| publication.receipt.is_none()) {
                let cut = blob.agent_sessions.iter().find(|stream| ResourceId::local(stream.wire_id) == binding.agent);
                if let Some(accepted) = cut.into_iter().flat_map(|stream| &stream.native_publications).find(|accepted| accepted.identity.id == publication.id && accepted.identity.fingerprint == publication.fingerprint) {
                    if let Some(view) = publication.view.as_ref() {
                        binding.view = view.clone();
                        // Actor acceptance is known, but a newly learned SDK
                        // identity is not acknowledged until its restore recipe
                        // was durably committed. The host retries this same id.
                        if view.provider_session_id == binding.launch.native_id {
                            publication.receipt = Some(NativePublicationReceipt {
                                publication_id: publication.id,
                                first_seq: accepted.accepted.first_seq,
                                last_seq: accepted.accepted.last_seq,
                            });
                            publication.view = None;
                        }
                    } else { binding.view.connected = false; }
                } else {
                    // Actor did not accept before its sealed cut; safe to retry
                    // the publication (never a model command) under the same id.
                    binding.publications.pop_back();
                    binding.publication_high_water = binding.publications.back().map_or(0, |publication| publication.id);
                }
            }
            if let Ok(bytes) = serde_json::to_vec(&binding.launch) {
                self.metadata_set(&phux_protocol::wire::frame::Scope::Resource(binding.parent.clone()), "phux.native-launch/v1", bytes);
            }
            self.native_agents.wakes.insert(binding.agent.clone(), Arc::new(Notify::new()));
            self.native_agents.bindings.insert(binding.agent.clone(), binding);
        }
    }
    pub(crate) fn set_native_host_executable(&mut self, executable: Option<std::path::PathBuf>) {
        self.config.native_host_executable = executable;
    }
    pub(crate) fn native_host_executable(&self) -> Result<std::path::PathBuf, std::io::Error> {
        self.config.native_host_executable.clone().map_or_else(std::env::current_exe, Ok)
    }
    /// Reserve an activation on an imported dormant terminal without changing
    /// its organization key, membership, name, or arrangement position.
    pub(crate) fn relaunch_native_guardian(
        &mut self,
        guardian: phux_core::ids::ResourceId,
        launch: &phux_core::TerminalLaunch,
        client: ClientId,
        operation: Option<phux_core::OperationId>,
        expected: Option<phux_core::Revision>,
    ) -> Result<(), phux_core::Refusal> {
        let record = self.registry().record(guardian).ok_or(phux_core::Refusal::NotFound)?.clone();
        if record.kind != phux_core::ResourceKind::Terminal || self.resource_handle(guardian).is_some()
            || record.activation.as_ref().is_some_and(|activation| activation.state.is_current()
                || activation.state == phux_core::ExecState::Lost) {
            return Err(phux_core::Refusal::InvalidArgument { field: "guardian", reason: "native insertion requires a dormant terminal without a current, lost, or retained engine".into() });
        }
        let command = phux_core::Command {
            operation: operation.unwrap_or_else(|| self.mint_operation()),
            op: phux_core::Op::ResourceRelaunch {
                resource: record.key,
                expected: expected.unwrap_or(record.revision),
                launch: Some(phux_core::LaunchSpec::Terminal(launch.clone())),
            },
        };
        let actor = self.actor_for(client);
        self.commit_command_from(&command, &actor, Some(client))?;
        Ok(())
    }
    /// Persist only a factual native provider identity, after its stream
    /// publication landed. No producer can change lifecycle or arrangement.
    pub(crate) fn persist_native_recipe(&mut self, agent: &ResourceId, view: &NativeView) -> Result<(), phux_core::Refusal> {
        let binding = self.native_agents.bindings.get(agent).ok_or(phux_core::Refusal::NotFound)?;
        let Some(native_id) = &view.provider_session_id else { return Ok(()); };
        if binding.launch.native_id.as_ref() == Some(native_id) { return Ok(()); }
        let mut recipe = binding.launch.clone();
        recipe.native_id = Some(native_id.clone());
        let parent = self.terminal_from_wire(&binding.parent).ok_or(phux_core::Refusal::NotFound)?;
        let record = self.registry().record(parent).ok_or(phux_core::Refusal::NotFound)?.clone();
        let activation = record.activation.as_ref().ok_or(phux_core::Refusal::NotFound)?.id;
        let Some(phux_core::LaunchSpec::Terminal(mut launch)) = record.launch else {
            return Err(phux_core::Refusal::InvalidArgument { field: "launch", reason: "native guardian is not a terminal launch".into() });
        };
        let encoded = serde_json::to_string(&recipe).map_err(|error| phux_core::Refusal::InvalidArgument { field: "native_recipe", reason: error.to_string() })?;
        let Some((_, value)) = launch.env.iter_mut().find(|(key, _)| key == NATIVE_HOST_SPEC_ENV) else {
            return Err(phux_core::Refusal::InvalidArgument { field: "native_recipe", reason: "native guardian has no explicit recipe marker".into() });
        };
        *value = encoded;
        self.commit_system(phux_core::Op::ResourceLaunchReported {
            resource: record.key, activation, launch: phux_core::LaunchSpec::Terminal(launch),
        })?;
        if let Some(binding) = self.native_agents.bindings.get_mut(agent) { binding.launch = recipe; }
        Ok(())
    }
}
