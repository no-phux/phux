//! Durable, viewer-independent owner of one native provider session.
//!
//! The root executable launches this library directly in the guardian Terminal.
//! Provider children inherit that process group. Reconnection never reconstructs
//! a provider client or replays a dispatched prompt. SDK answers are queued, not
//! claimed consumed. A bounded output backlog is drained independently of broker
//! availability; exhaustion terminates the worker rather than silently losing
//! authoritative output. The same `NativeController` retains publication retry
//! identities through daemon replacement.
//!
//! `PHUX_AGENT_TRANSCRIPT=0` suppresses transcript rows, not lifecycle or
//! questions. The default retains content-excluding tool summaries without
//! output; `full` explicitly opts into tool output. Worker environments are
//! rebuilt from the launch specification without controller bootstrap proofs
//! or production WebSocket credential/TLS overrides.
#![forbid(unsafe_code)]

mod acp;
mod common;
mod node;
mod omp;

use std::{collections::{BTreeMap, VecDeque}, path::PathBuf, sync::Arc};
use parking_lot::Mutex;
use phux_client::native_agent::NativeController;
use phux_protocol::{ids::ResourceId, native_agent::*};
use tokio::sync::{mpsc, watch, Notify};

/// Only the guardian receives the binding proof; never a provider subprocess.
#[derive(Clone)]
pub struct HostSpec {
    pub backend: String,
    pub provider: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub socket: PathBuf,
    pub parent: ResourceId,
    pub agent: ResourceId,
    pub activation: String,
    pub binding_proof: String,
    pub sdk_runtime: Option<PathBuf>,
    pub node_bridge: Option<PathBuf>,
    pub native_id: Option<String>,
}
impl std::fmt::Debug for HostSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSpec").field("backend", &self.backend)
            .field("provider", &self.provider).field("agent", &self.agent)
            .field("activation", &self.activation).finish_non_exhaustive()
    }
}
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("invalid native host specification: {0}")] Invalid(String),
    #[error(transparent)] Io(#[from] std::io::Error),
    #[error("native SDK: {0}")] Sdk(String),
    #[error("native controller: {0}")] Controller(String),
    #[error("native output backlog exhausted (4096 records / 64 MiB); worker stopped")] Backlog,
}

const OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const UPDATE_BYTES: usize = 14 * 1024;
fn json_size(value: &impl serde::Serialize) -> Result<usize, HostError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0 += bytes.len(); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|error| HostError::Invalid(error.to_string()))?;
    Ok(counter.0)
}
#[derive(Debug, Clone)]
enum Output { Update(NativeUpdate), Result(NativeCommandResult) }
#[derive(Debug, Default)]
struct Backlog { rows: VecDeque<(Output, usize)>, bytes: usize, failed: bool }
#[derive(Debug)]
struct Shared {
    backlog: Mutex<Backlog>,
    wake: Notify,
    view: Mutex<NativeView>,
    responders: Mutex<BTreeMap<String, u64>>,
    transcript_mode: common::TranscriptMode,
    ledger: Mutex<Ledger>,
}
#[derive(Debug, Clone)]
struct Sink(Arc<Shared>);
impl Sink {
    fn update(&self, mut update: NativeUpdate) -> Result<(), HostError> {
        validate_payload(&update).map_err(HostError::Invalid)?;
        if json_size(&update)? > UPDATE_BYTES {
            return Err(HostError::Invalid("native update exceeds bounded record envelope".into()));
        }
        if matches!(&update, NativeUpdate::Transcript { .. } | NativeUpdate::Notice { .. }) {
            return self.push(Output::Update(update));
        }
        let mut current = self.0.view.lock();
        {
            let mut candidate = current.clone();
            let view = &mut candidate;
            match &update {
                NativeUpdate::View { view: next } => {
                    if next.activation != view.activation { return Err(HostError::Invalid("SDK activation changed".into())); }
                    if view.provider_session_id.is_some() && next.provider_session_id.is_some() && view.provider_session_id != next.provider_session_id {
                        return Err(HostError::Invalid("provider session identity changed within an activation".into()));
                    }
                    let previous_identity = view.provider_session_id.clone();
                    *view = next.clone();
                    if view.provider_session_id.is_none() { view.provider_session_id = previous_identity; }
                }
                NativeUpdate::QuestionOpened { question } => {
                    if view.questions.len() >= MAX_NATIVE_QUESTIONS { return Err(HostError::Invalid("too many native questions".into())); }
                    view.questions.retain(|old| old.id != question.id);
                    view.questions.push(question.clone());
                }
                NativeUpdate::QuestionClosed { id, .. } => {
                    view.questions.retain(|question| &question.id != id);
                    self.0.responders.lock().remove(id);
                }
                NativeUpdate::State { state, turn_id } => {
                    view.turn_id = if *state == NativeState::Done { None } else { turn_id.clone() };
                }
                _ => {}
            }
            if serde_json::to_vec(view).map_err(|error| HostError::Invalid(error.to_string()))?.len() > 14 * 1024 {
                return Err(HostError::Invalid("aggregate native questions exceed bounded view".into()));
            }
            *current = view.clone();
        }
        if let NativeUpdate::View { view } = &mut update {
            if view.provider_session_id.is_none() { view.provider_session_id = current.provider_session_id.clone(); }
        }
        self.push(Output::Update(update))
    }
    fn begin_prompt(&self, pending_turn: String) -> Result<bool, HostError> {
        let mut view = self.0.view.lock();
        let follow_up = view.turn_id.is_some();
        if !follow_up {
            view.turn_id = Some(pending_turn);
            self.push(Output::Update(NativeUpdate::View { view: view.clone() }))?;
        }
        Ok(follow_up)
    }
    fn replace_turn(&self, turn_id: String) -> Result<(), HostError> {
        let mut view = self.0.view.lock();
        view.turn_id = Some(turn_id);
        self.push(Output::Update(NativeUpdate::View { view: view.clone() }))
    }
    fn reject_pending_turn(&self, pending_turn: &str) -> Result<(), HostError> {
        let mut view = self.0.view.lock();
        if view.turn_id.as_deref() == Some(pending_turn) {
            view.turn_id = None;
            self.push(Output::Update(NativeUpdate::View { view: view.clone() }))?;
        }
        Ok(())
    }
    fn result(&self, result: NativeCommandResult) -> Result<(), HostError> {
        validate_payload(&result).map_err(HostError::Invalid)?;
        let mut ledger = self.0.ledger.lock();
        if let Some((_, saved)) = ledger.entries.get_mut(&result.operation_id) {
            if saved.as_ref().is_some_and(|old| matches!(old.disposition, NativeDisposition::Completed | NativeDisposition::Refused) && old != &result) { return Ok(()); }
            if result.disposition == NativeDisposition::Refused && saved.as_ref().is_none_or(|old| old.disposition == NativeDisposition::Refused) {
                self.0.responders.lock().retain(|_, operation_id| *operation_id != result.operation_id);
            }
            *saved = Some(result.clone());
        }
        // Keep receipt ordering atomic with its ledger revision.
        self.push(Output::Result(result))
    }
    fn push(&self, output: Output) -> Result<(), HostError> {
        let bytes = match &output {
            Output::Update(value) => json_size(value)?,
            Output::Result(value) => json_size(value)?,
        };
        let mut backlog = self.0.backlog.lock();
        if backlog.failed || backlog.rows.len() >= 4096 || backlog.bytes.saturating_add(bytes) > OUTPUT_BYTES {
            backlog.failed = true;
            self.0.wake.notify_one();
            return Err(HostError::Backlog);
        }
        backlog.bytes += bytes;
        backlog.rows.push_back((output, bytes));
        self.0.wake.notify_one();
        Ok(())
    }
    fn view(&self) -> NativeView { self.0.view.lock().clone() }
    fn state(&self, state: NativeState) -> Result<(), HostError> {
        let mut view = self.0.view.lock();
        if state == NativeState::Done { view.turn_id = None; }
        self.push(Output::Update(NativeUpdate::State { state, turn_id: view.turn_id.clone() }))
    }
    fn notice(&self, text: String) -> Result<(), HostError> {
        self.update(NativeUpdate::Notice { level: "info".into(), text: common::head(&text, 8 * 1024) })
    }
}
#[derive(Debug)]
struct Ledger {
    entries: BTreeMap<u64, (NativeCommand, Option<NativeCommandResult>)>,
    retired_through: u64,
}
impl Ledger {
    fn reserve(&mut self, command: &NativeCommand) -> Result<Option<NativeCommandResult>, String> {
        if let Some((previous, result)) = self.entries.get(&command.operation_id) {
            if previous != command { return Err("operation id payload changed".into()); }
            return Ok(Some(result.clone().unwrap_or_else(|| common::result(command.operation_id, NativeDisposition::Unknown, "SDK dispatch pending; never replay"))));
        }
        if command.operation_id <= self.retired_through { return Err("operation outside retained ledger; never replay".into()); }
        if self.entries.len() >= MAX_NATIVE_OPERATIONS {
            let Some(id) = self.entries.iter().find_map(|(id, (_, result))| result.as_ref().map(|_| *id)) else { return Err("native operation ledger full".into()); };
            self.entries.remove(&id);
            self.retired_through = self.retired_through.max(id);
        }
        self.entries.insert(command.operation_id, (command.clone(), None));
        Ok(None)
    }
}

/// Own one real worker until it exits or its AgentSession is closed. No UI owns
/// an SDK. `command` is executable argv, never a shell string or PTY input.
pub async fn run(spec: HostSpec) -> Result<(), HostError> {
    if spec.command.first().is_none_or(String::is_empty) || spec.activation.is_empty() || spec.binding_proof.is_empty() {
        return Err(HostError::Invalid("command, activation and private binding proof are required".into()));
    }
    if !matches!(spec.backend.as_str(), "omp-rpc" | "acp" | "node") { return Err(HostError::Invalid("unsupported native backend".into())); }
    // Bind before starting the provider; a refused binding cannot launch a model.
    let controller = Arc::new(NativeController::connect(&spec.socket, spec.agent.clone(), spec.activation.clone(), spec.binding_proof.clone()).await.map_err(|error| HostError::Controller(error.to_string()))?);
    let sink = Sink(Arc::new(Shared {
        backlog: Mutex::new(Backlog::default()), wake: Notify::new(), responders: Mutex::new(BTreeMap::new()),
        transcript_mode: common::TranscriptMode::from_env(spec.env.get("PHUX_AGENT_TRANSCRIPT").map(String::as_str)),
        ledger: Mutex::new(Ledger { entries: BTreeMap::new(), retired_through: 0 }),
        view: Mutex::new(NativeView { activation: spec.activation.clone(), backend: spec.backend.clone(), capabilities: NativeCapabilities::default(), connected: false, turn_id: None, provider_session_id: None, questions: Vec::new() }),
    }));
    let (commands, receive) = mpsc::channel(MAX_NATIVE_PENDING);
    let (shutdown, stopped) = watch::channel(false);
    let worker_spec = spec.clone();
    let worker_sink = sink.clone();
    // ACP uses a local runtime on its own thread; the blocking OMP SDK never
    // occupies the daemon/UI cooperative loop. Each driver owns child teardown.
    let (exit_tx, mut exit_rx) = tokio::sync::oneshot::channel();
    let worker = std::thread::Builder::new().name("phux-native-sdk".into()).spawn(move || {
        let result = match worker_spec.backend.as_str() {
            "omp-rpc" => omp::run(worker_spec, worker_sink, receive, stopped),
            "acp" => acp::run(worker_spec, worker_sink, receive, stopped),
            "node" => node::run(worker_spec, worker_sink, receive, stopped),
            _ => unreachable!(),
        };
        let _ = exit_tx.send(result);
    })?;
    let ledger = &sink.0.ledger;
    let control = async {
        loop {
            let command = match controller.receive().await {
                Ok(command) => command,
                Err(error) if error.is_retryable() => { tokio::time::sleep(std::time::Duration::from_millis(250)).await; continue; }
                Err(error) => return Err(HostError::Controller(error.to_string())),
            };
            let mut refusal = command.intent.validate().err();
            if command.activation != spec.activation { refusal = Some("stale native activation".into()); }
            let previous = ledger.lock().reserve(&command);
            match previous {
                Ok(Some(result)) => { sink.result(result)?; continue; }
                Err(message) => { sink.result(common::result(command.operation_id, NativeDisposition::Refused, &message))?; continue; }
                Ok(None) => {}
            }
            let view = sink.view();
            if !view.connected { refusal = Some("provider SDK is not ready".into()); }
            match &command.intent {
                NativeIntent::Prompt { .. } if !view.capabilities.prompt => refusal = Some("provider prompt unsupported".into()),
                NativeIntent::Prompt { .. } if view.turn_id.is_some() && !view.capabilities.follow_up => refusal = Some("provider turn is active; follow-up unsupported".into()),
                NativeIntent::Interrupt { turn_id } if !view.capabilities.interrupt || view.turn_id.as_ref() != Some(turn_id) => refusal = Some("stale turn or interrupt unsupported".into()),
                NativeIntent::Answer { question_id, response } => {
                    let current = sink.0.view.lock();
                    let question = current.questions.iter().find(|question| &question.id == question_id);
                    if !current.connected || !current.capabilities.questions || question.is_none() { refusal = Some("question no longer pending".into()); }
                    else if let Err(message) = common::validate_answer(question.expect("checked question"), response) { refusal = Some(message); }
                    else {
                        let mut responders = sink.0.responders.lock();
                        if responders.contains_key(question_id) { refusal = Some("question already has a responder".into()); }
                        else if refusal.is_none() { responders.insert(question_id.clone(), command.operation_id); }
                    }
                }
                _ => {}
            }
            if let Some(message) = refusal { sink.result(common::result(command.operation_id, NativeDisposition::Refused, &message))?; }
            else { commands.send(command).await.map_err(|_| HostError::Sdk("SDK command receiver closed".into()))?; }
        }
        #[allow(unreachable_code)] Ok::<(), HostError>(())
    };
    let publication = async {
        loop {
            let notified = sink.0.wake.notified();
            let next = {
                let backlog = sink.0.backlog.lock();
                if backlog.failed { return Err(HostError::Backlog); }
                backlog.rows.front().map(|(output, _)| output.clone())
            };
            let Some(next) = next else { notified.await; continue; };
            deliver(&controller, &sink, next).await?;
            let mut backlog = sink.0.backlog.lock();
            if let Some((_, bytes)) = backlog.rows.pop_front() { backlog.bytes -= bytes; }
        }
    };
    let (outcome, worker_ended) = tokio::select! {
        result = control => (result, false),
        result = publication => (result, false),
        result = &mut exit_rx => (result.unwrap_or_else(|_| Err(HostError::Sdk("SDK owner thread terminated".into()))), true),
    };
    let _ = shutdown.send(true);
    drop(commands);
    // Joining happens off the current-thread runtime. Child guards kill/reap
    // before dropping the SDK; shutdown never kills the guardian's own group.
    tokio::task::spawn_blocking(move || worker.join()).await.map_err(|error| HostError::Sdk(error.to_string()))?.map_err(|_| HostError::Sdk("SDK owner panicked".into()))?;
    if worker_ended {
        if let Err(error) = &outcome { sink.update(NativeUpdate::Notice { level: "error".into(), text: common::head(&error.to_string(), 1024) })?; }
        let mut view = sink.view(); view.connected = false; view.capabilities = NativeCapabilities::default();
        sink.update(NativeUpdate::View { view })?;
        loop {
            let next = sink.0.backlog.lock().rows.front().map(|(output, _)| output.clone());
            let Some(next) = next else { break; };
            deliver(&controller, &sink, next).await?;
            let mut backlog = sink.0.backlog.lock();
            if let Some((_, bytes)) = backlog.rows.pop_front() { backlog.bytes -= bytes; }
        }
    }
    let _ = controller.close().await;
    outcome
}

async fn deliver(controller: &NativeController, sink: &Sink, output: Output) -> Result<(), HostError> {
    loop {
        let result = match &output {
            Output::Update(update) => controller.publish(update.clone()).await.map(|_| ()),
            Output::Result(result) => {
                controller.complete(result.clone()).await
            }
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.is_retryable() => {
                if sink.0.backlog.lock().failed { return Err(HostError::Backlog); }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(error) => return Err(HostError::Controller(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changed_and_retired_operations_never_dispatch() {
        let mut ledger = Ledger { entries: BTreeMap::new(), retired_through: 0 };
        let command = NativeCommand { operation_id: 1, activation: "a".into(), intent: NativeIntent::Prompt { text: "hello".into() } };
        assert!(ledger.reserve(&command).unwrap().is_none());
        assert_eq!(ledger.reserve(&command).unwrap().unwrap().disposition, NativeDisposition::Unknown);
        let mut changed = command.clone(); changed.activation = "b".into();
        assert!(ledger.reserve(&changed).is_err());
        ledger.entries.clear(); ledger.retired_through = 1;
        assert!(ledger.reserve(&command).is_err());
    }
    pub(super) fn sink() -> Sink { sink_with_mode(common::TranscriptMode::Content) }
    fn sink_with_mode(transcript_mode: common::TranscriptMode) -> Sink {
        Sink(Arc::new(Shared {
            backlog: Mutex::new(Backlog::default()), wake: Notify::new(),
            responders: Mutex::new(BTreeMap::new()), transcript_mode,
            ledger: Mutex::new(Ledger { entries: BTreeMap::new(), retired_through: 0 }),
            view: Mutex::new(NativeView { activation: "a".into(), backend: "omp-rpc".into(), capabilities: NativeCapabilities::default(), connected: false, turn_id: None, provider_session_id: None, questions: Vec::new() }),
        }))
    }
    #[test]
    fn identity_is_provider_supplied_and_activation_fenced() {
        let sink = sink();
        assert!(sink.view().provider_session_id.is_none());
        let mut view = sink.view(); view.provider_session_id = Some("official-session".into());
        sink.update(NativeUpdate::View { view: view.clone() }).unwrap();
        view.provider_session_id = Some("different-session".into());
        assert!(sink.update(NativeUpdate::View { view }).is_err());
        assert_eq!(sink.view().provider_session_id.as_deref(), Some("official-session"));
    }
    #[test]
    fn final_receipt_does_not_regress_on_redelivery() {
        let sink = sink();
        let command = NativeCommand { operation_id: u64::MAX, activation: "a".into(), intent: NativeIntent::Prompt { text: "hi".into() } };
        sink.0.ledger.lock().reserve(&command).unwrap();
        sink.result(common::result(u64::MAX, NativeDisposition::Completed, "actual SDK completion")).unwrap();
        sink.result(common::result(u64::MAX, NativeDisposition::Queued, "stale queue stage")).unwrap();
        assert_eq!(sink.0.backlog.lock().rows.len(), 1);
        let receipt = sink.0.ledger.lock().reserve(&command).unwrap().unwrap();
        assert_eq!(receipt.disposition, NativeDisposition::Completed);
    }
    #[test]
    fn transcript_boundary_preserves_envelope_privacy_and_escaped_budget() {
        use phux_client_core::session::agent_transcript::{TranscriptEntry, TranscriptRole, ToolStatus};
        let sink = sink();
        common::text(&sink, "omp", "reply".into(), TranscriptRole::Assistant, "\"\\\né".repeat(20000), true).unwrap();
        common::tool(&sink, "omp", "write".into(), "write".into(), common::summarize_args(&serde_json::json!({"path":"safe.rs","content":"PRIVATE BODY"})), "PRIVATE OUTPUT".into(), ToolStatus::Ok).unwrap();
        let backlog = sink.0.backlog.lock();
        for (row, _) in &backlog.rows {
            let Output::Update(update @ NativeUpdate::Transcript { entry }) = row else { panic!("transcript expected"); };
            assert!(json_size(update).unwrap() <= UPDATE_BYTES);
            assert_eq!(entry["schema"], "phux.transcript/v1");
            assert!(TranscriptEntry::from_data(entry).is_some());
            assert!(!serde_json::to_string(update).unwrap().contains("PRIVATE"));
        }
        let Output::Update(NativeUpdate::Transcript { entry }) = &backlog.rows[0].0 else { unreachable!() };
        assert_eq!(entry["entry"]["truncated"], true);
    }
    #[test]
    fn transcript_off_retains_lifecycle_and_full_requires_opt_in() {
        use phux_client_core::session::agent_transcript::{TranscriptRole, ToolStatus};
        let off = sink_with_mode(common::TranscriptMode::Off);
        common::text(&off, "omp", "p".into(), TranscriptRole::User, "private".into(), true).unwrap();
        off.state(NativeState::Working).unwrap();
        assert_eq!(off.0.backlog.lock().rows.len(), 1);
        let full = sink_with_mode(common::TranscriptMode::Full);
        common::tool(&full, "omp", "t".into(), "read".into(), String::new(), "opt-in output".into(), ToolStatus::Ok).unwrap();
        let backlog = full.0.backlog.lock();
        let Output::Update(NativeUpdate::Transcript { entry }) = &backlog.rows[0].0 else { unreachable!() };
        assert_eq!(entry["entry"]["tool"]["output"], "opt-in output");
    }
    #[test]
    fn only_matching_definitive_refusal_releases_responder() {
        let sink = sink();
        for operation_id in [1, 2] {
            sink.0.ledger.lock().reserve(&NativeCommand { operation_id, activation: "a".into(), intent: NativeIntent::Answer { question_id: "q".into(), response: serde_json::json!({"values":{}}) } }).unwrap();
        }
        sink.0.responders.lock().insert("q".into(), 2);
        sink.result(common::result(1, NativeDisposition::Refused, "old rejection")).unwrap();
        assert_eq!(sink.0.responders.lock().get("q"), Some(&2));
        sink.result(common::result(2, NativeDisposition::Unknown, "ambiguous dispatch")).unwrap();
        assert_eq!(sink.0.responders.lock().get("q"), Some(&2));
        sink.result(common::result(2, NativeDisposition::Refused, "not admitted")).unwrap();
        assert_eq!(sink.0.responders.lock().get("q"), Some(&2));
        let command = NativeCommand { operation_id: 3, activation: "a".into(), intent: NativeIntent::Answer { question_id: "correctable".into(), response: serde_json::json!({"values":{}}) } };
        sink.0.ledger.lock().reserve(&command).unwrap();
        sink.0.responders.lock().insert("correctable".into(), 3);
        sink.result(common::result(3, NativeDisposition::Refused, "local schema rejection")).unwrap();
        assert!(!sink.0.responders.lock().contains_key("correctable"));
    }
}
