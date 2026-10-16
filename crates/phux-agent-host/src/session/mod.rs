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

mod acp;
mod common;

use parking_lot::Mutex;
use phux_client::native_agent::NativeController;
use phux_protocol::{
    ids::ResourceId,
    native_agent::{
        MAX_NATIVE_OPERATIONS, MAX_NATIVE_PENDING, MAX_NATIVE_QUESTIONS, NativeCapabilities,
        NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent, NativeState,
        NativeUpdate, NativeView, validate_payload,
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::{Notify, mpsc, watch};

/// Only the guardian receives the binding proof; never a provider subprocess.
#[derive(Clone)]
pub struct HostSpec {
    /// The sole provider seam: `acp`.
    pub backend: String,
    /// Provider slug carried by the durable agent resource.
    pub provider: String,
    /// Provider executable and argv; never interpreted by a shell.
    pub command: Vec<String>,
    /// Executing host's working directory.
    pub cwd: PathBuf,
    /// Explicit launch environment, sanitized again before provider spawn.
    pub env: BTreeMap<String, String>,
    /// Controller socket for this host-owned binding.
    pub socket: PathBuf,
    /// Guardian Terminal that owns the provider's lifetime.
    pub parent: ResourceId,
    /// Precreated `AgentSession`; registering never creates a second resource.
    pub agent: ResourceId,
    /// Exact activation accepted by the broker.
    pub activation: String,
    /// Private owner proof, excluded from debug output and provider environments.
    pub binding_proof: String,
    /// Optional genuine provider session identity to resume.
    pub native_id: Option<String>,
}
impl std::fmt::Debug for HostSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSpec")
            .field("backend", &self.backend)
            .field("provider", &self.provider)
            .field("agent", &self.agent)
            .field("activation", &self.activation)
            .finish_non_exhaustive()
    }
}
/// Terminal host failures; ambiguous SDK operation outcomes remain typed receipts.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// Invalid launch specification or bounded native payload.
    #[error("invalid native host specification: {0}")]
    Invalid(String),
    /// Provider process or transport I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SDK execution, protocol, or owner-thread failure.
    #[error("native SDK: {0}")]
    Sdk(String),
    /// Broker registration or delivery failed.
    #[error("native controller: {0}")]
    Controller(String),
    /// Authoritative output exceeded the bounded queue and stopped the worker.
    #[error("native output backlog exhausted (4096 records / 64 MiB); worker stopped")]
    Backlog,
}

const OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const UPDATE_BYTES: usize = 14 * 1024;
fn json_size(value: &impl serde::Serialize) -> Result<usize, HostError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)
        .map_err(|error| HostError::Invalid(error.to_string()))?;
    Ok(counter.0)
}
fn replace_view(view: &mut NativeView, next: &NativeView) -> Result<(), HostError> {
    if next.activation != view.activation {
        return Err(HostError::Invalid("ACP activation changed".into()));
    }
    if view.provider_session_id.is_some()
        && next.provider_session_id.is_some()
        && view.provider_session_id != next.provider_session_id
    {
        return Err(HostError::Invalid(
            "provider session identity changed within an activation".into(),
        ));
    }
    let previous_identity = view.provider_session_id.clone();
    *view = next.clone();
    if view.provider_session_id.is_none() {
        view.provider_session_id = previous_identity;
    }
    Ok(())
}
#[derive(Debug, Clone)]
enum Output {
    Update(NativeUpdate),
    Result(NativeCommandResult),
}
#[derive(Debug, Default)]
struct Backlog {
    rows: VecDeque<(Output, usize)>,
    bytes: usize,
    failed: bool,
}
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
        if json_size(&update)? > UPDATE_BYTES {
            return Err(HostError::Invalid(
                "native update exceeds bounded record envelope".into(),
            ));
        }
        if matches!(
            &update,
            NativeUpdate::Transcript { .. } | NativeUpdate::Notice { .. }
        ) {
            return self.push(Output::Update(update));
        }
        let mut current = self.0.view.lock();
        *current = self.project_view(&current, &update)?;
        if let NativeUpdate::View { view } = &mut update
            && view.provider_session_id.is_none()
        {
            view.provider_session_id
                .clone_from(&current.provider_session_id);
        }
        // Queue under the view lock so another source revision cannot overtake it.
        let outcome = self.push(Output::Update(update));
        drop(current);
        outcome
    }
    fn project_view(
        &self,
        current: &NativeView,
        update: &NativeUpdate,
    ) -> Result<NativeView, HostError> {
        let mut candidate = current.clone();
        let view = &mut candidate;
        match &update {
            NativeUpdate::View { view: next } => {
                replace_view(view, next)?;
            }
            NativeUpdate::QuestionOpened { question } => {
                if view.questions.len() >= MAX_NATIVE_QUESTIONS {
                    return Err(HostError::Invalid("too many native questions".into()));
                }
                view.questions.retain(|old| old.id != question.id);
                view.questions.push(question.clone());
            }
            NativeUpdate::QuestionClosed { id, .. } => {
                view.questions.retain(|question| &question.id != id);
                self.0.responders.lock().remove(id);
            }
            NativeUpdate::State { state, turn_id } => {
                view.turn_id = if *state == NativeState::Done {
                    None
                } else {
                    turn_id.clone()
                };
            }
            _ => {}
        }
        if json_size(view)? > UPDATE_BYTES {
            return Err(HostError::Invalid(
                "aggregate native questions exceed bounded view".into(),
            ));
        }
        Ok(candidate)
    }

    fn begin_prompt(&self, pending_turn: String) -> Result<bool, HostError> {
        let mut view = self.0.view.lock();
        let follow_up = view.turn_id.is_some();
        if !follow_up {
            view.turn_id = Some(pending_turn);
            self.push(Output::Update(NativeUpdate::View { view: view.clone() }))?;
        }
        drop(view);
        Ok(follow_up)
    }
    fn reject_pending_turn(&self, pending_turn: &str) -> Result<(), HostError> {
        let mut view = self.0.view.lock();
        if view.turn_id.as_deref() == Some(pending_turn) {
            view.turn_id = None;
            self.push(Output::Update(NativeUpdate::View { view: view.clone() }))?;
        }
        drop(view);
        Ok(())
    }
    fn result(&self, result: NativeCommandResult) -> Result<(), HostError> {
        validate_payload(&result).map_err(HostError::Invalid)?;
        let mut ledger = self.0.ledger.lock();
        if let Some((_, saved)) = ledger.entries.get_mut(&result.operation_id) {
            if saved.as_ref().is_some_and(|old| {
                matches!(
                    old.disposition,
                    NativeDisposition::Completed | NativeDisposition::Refused
                ) && old != &result
            }) {
                return Ok(());
            }
            if result.disposition == NativeDisposition::Refused && saved.is_none() {
                self.0
                    .responders
                    .lock()
                    .retain(|_, operation_id| *operation_id != result.operation_id);
            }
            *saved = Some(result.clone());
        }
        // Keep receipt ordering atomic with its ledger revision.
        let outcome = self.push(Output::Result(result));
        drop(ledger);
        outcome
    }
    fn push(&self, output: Output) -> Result<(), HostError> {
        let bytes = match &output {
            Output::Update(value) => json_size(value)?,
            Output::Result(value) => json_size(value)?,
        };
        let mut backlog = self.0.backlog.lock();
        if backlog.failed
            || backlog.rows.len() >= 4096
            || backlog.bytes.saturating_add(bytes) > OUTPUT_BYTES
        {
            backlog.failed = true;
            drop(backlog);
            self.0.wake.notify_one();
            return Err(HostError::Backlog);
        }
        backlog.bytes += bytes;
        backlog.rows.push_back((output, bytes));
        drop(backlog);
        self.0.wake.notify_one();
        Ok(())
    }
    fn view(&self) -> NativeView {
        self.0.view.lock().clone()
    }
    fn state(&self, state: NativeState) -> Result<(), HostError> {
        let mut view = self.0.view.lock();
        if state == NativeState::Done {
            view.turn_id = None;
        }
        self.push(Output::Update(NativeUpdate::State {
            state,
            turn_id: view.turn_id.clone(),
        }))
    }
    fn notice(&self, text: &str) -> Result<(), HostError> {
        self.update(NativeUpdate::Notice {
            level: "info".into(),
            text: common::head(text, 8 * 1024),
        })
    }
}
#[derive(Debug)]
struct Ledger {
    entries: BTreeMap<u64, (NativeCommand, Option<NativeCommandResult>)>,
}
impl Ledger {
    fn reserve(&mut self, command: &NativeCommand) -> Result<Option<NativeCommandResult>, String> {
        if let Some((previous, result)) = self.entries.get(&command.operation_id) {
            if previous != command {
                return Err("operation id payload changed".into());
            }
            return Ok(Some(result.clone().unwrap_or_else(|| {
                common::result(
                    command.operation_id,
                    NativeDisposition::Unknown,
                    "SDK dispatch pending; never replay",
                )
            })));
        }
        // IDs are opaque random u64 values, not a monotonic sequence. Retain
        // every admitted identity until this owner ends; never evict and replay.
        if self.entries.len() >= MAX_NATIVE_OPERATIONS {
            return Err("native operation ledger full (1024 identities per activation)".into());
        }
        self.entries
            .insert(command.operation_id, (command.clone(), None));
        Ok(None)
    }
}

/// Own one real worker until it exits or its `AgentSession` is closed. No UI owns
/// an SDK. `command` is executable argv, never a shell string or PTY input.
pub async fn run(spec: HostSpec) -> Result<(), HostError> {
    validate_spec(&spec)?;
    // Bind before starting the provider; a refused binding cannot launch a model.
    let controller = NativeController::connect(
        &spec.socket,
        spec.agent.clone(),
        spec.activation.clone(),
        spec.binding_proof.clone(),
    )
    .await
    .map_err(|error| HostError::Controller(error.to_string()))?;
    let activation = spec.activation.clone();
    let sink = sink_for(&spec);
    let (commands, receive) = mpsc::channel(MAX_NATIVE_PENDING);
    let (shutdown, stopped) = watch::channel(false);
    let Worker { thread, mut exit } = spawn_worker(spec, sink.clone(), receive, stopped)?;
    let (outcome, worker_ended) = tokio::select! {
        result = control_commands(&controller, &sink, &activation, &commands) => (result, false),
        result = publish_outputs(&controller, &sink) => (result, false),
        result = &mut exit => (result.unwrap_or_else(|_| Err(HostError::Sdk("SDK owner thread terminated".into()))), true),
    };
    let _ = shutdown.send(true);
    drop(commands);
    // Joining stays off the runtime. Driver guards kill/reap only their own child.
    tokio::task::spawn_blocking(move || thread.join())
        .await
        .map_err(|error| HostError::Sdk(error.to_string()))?
        .map_err(|_| HostError::Sdk("SDK owner panicked".into()))?;
    if worker_ended {
        finish_worker(&controller, &sink, &outcome).await?;
    }
    let _ = controller.close().await;
    drop(controller);
    outcome
}

fn validate_spec(spec: &HostSpec) -> Result<(), HostError> {
    if spec.command.first().is_none_or(String::is_empty)
        || spec.activation.is_empty()
        || spec.binding_proof.is_empty()
    {
        return Err(HostError::Invalid(
            "command, activation and private binding proof are required".into(),
        ));
    }
    if spec.backend != "acp" {
        return Err(HostError::Invalid("unsupported native backend".into()));
    }
    Ok(())
}

fn sink_for(spec: &HostSpec) -> Sink {
    Sink(Arc::new(Shared {
        backlog: Mutex::new(Backlog::default()),
        wake: Notify::new(),
        responders: Mutex::new(BTreeMap::new()),
        transcript_mode: common::TranscriptMode::from_env(
            spec.env.get("PHUX_AGENT_TRANSCRIPT").map(String::as_str),
        ),
        ledger: Mutex::new(Ledger {
            entries: BTreeMap::new(),
        }),
        view: Mutex::new(NativeView {
            activation: spec.activation.clone(),
            backend: spec.backend.clone(),
            capabilities: NativeCapabilities::default(),
            connected: false,
            turn_id: None,
            provider_session_id: None,
            questions: Vec::new(),
        }),
    }))
}

struct Worker {
    thread: std::thread::JoinHandle<()>,
    exit: tokio::sync::oneshot::Receiver<Result<(), HostError>>,
}

fn spawn_worker(
    spec: HostSpec,
    sink: Sink,
    receive: mpsc::Receiver<NativeCommand>,
    stopped: watch::Receiver<bool>,
) -> Result<Worker, HostError> {
    let (exit_tx, exit) = tokio::sync::oneshot::channel();
    // ACP keeps its local runtime; blocking OMP never occupies the UI/daemon loop.
    let thread = std::thread::Builder::new()
        .name("phux-native-sdk".into())
        .spawn(move || {
            let result = match spec.backend.as_str() {
                "acp" => acp::run(spec, sink, receive, stopped),
                _ => Err(HostError::Invalid("unsupported native backend".into())),
            };
            let _ = exit_tx.send(result);
        })?;
    Ok(Worker { thread, exit })
}

async fn control_commands(
    controller: &NativeController,
    sink: &Sink,
    activation: &str,
    commands: &mpsc::Sender<NativeCommand>,
) -> Result<(), HostError> {
    loop {
        let command = receive_command(controller).await?;
        let refusal = command.intent.validate().err();
        let refusal = if command.activation == activation {
            refusal
        } else {
            Some("stale native activation".into())
        };
        let previous = sink.0.ledger.lock().reserve(&command);
        match previous {
            Ok(Some(result)) => {
                sink.result(result)?;
                continue;
            }
            Err(message) => {
                sink.result(common::result(
                    command.operation_id,
                    NativeDisposition::Refused,
                    &message,
                ))?;
                continue;
            }
            Ok(None) => {}
        }
        if let Some(message) = command_refusal(sink, &command, refusal) {
            sink.result(common::result(
                command.operation_id,
                NativeDisposition::Refused,
                &message,
            ))?;
        } else {
            commands
                .send(command)
                .await
                .map_err(|_| HostError::Sdk("SDK command receiver closed".into()))?;
        }
    }
}

async fn receive_command(controller: &NativeController) -> Result<NativeCommand, HostError> {
    loop {
        match controller.receive().await {
            Ok(command) => return Ok(command),
            Err(error) if error.is_retryable() => {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(error) => return Err(HostError::Controller(error.to_string())),
        }
    }
}

fn current_interrupt(view: &NativeView, turn_id: &str) -> bool {
    view.capabilities.interrupt.is_supported() && view.turn_id.as_deref() == Some(turn_id)
}

fn command_refusal(
    sink: &Sink,
    command: &NativeCommand,
    refusal: Option<String>,
) -> Option<String> {
    let view = sink.0.view.lock();
    let refusal = if view.connected {
        refusal
    } else {
        Some("provider SDK is not ready".into())
    };
    match &command.intent {
        NativeIntent::Prompt { .. } if !view.capabilities.prompt.is_supported() => {
            Some("provider prompt unsupported".into())
        }
        NativeIntent::Prompt { .. }
            if view.turn_id.is_some() && !view.capabilities.follow_up.is_supported() =>
        {
            Some("provider turn is active; follow-up unsupported".into())
        }
        NativeIntent::Interrupt { turn_id } if !current_interrupt(&view, turn_id) => {
            Some("stale turn or interrupt unsupported".into())
        }
        NativeIntent::Answer {
            question_id,
            response,
        } => {
            drop(view);
            answer_refusal(sink, question_id, response, command.operation_id, refusal)
        }
        _ => refusal,
    }
}

fn answer_refusal(
    sink: &Sink,
    question_id: &str,
    response: &serde_json::Value,
    operation_id: u64,
    refusal: Option<String>,
) -> Option<String> {
    // Preserve view -> responder lock ordering and recheck the live question.
    let current = sink.0.view.lock();
    if !current.connected || !current.capabilities.questions.is_supported() {
        return Some("question no longer pending".into());
    }
    let Some(question) = current
        .questions
        .iter()
        .find(|question| question.id == question_id)
    else {
        return Some("question no longer pending".into());
    };
    if let Err(message) = common::validate_answer(question, response) {
        return Some(message);
    }
    let mut responders = sink.0.responders.lock();
    if responders.contains_key(question_id) {
        return Some("question already has a responder".into());
    }
    if refusal.is_none() {
        responders.insert(question_id.to_owned(), operation_id);
    }
    drop(responders);
    drop(current);
    refusal
}

async fn publish_outputs(controller: &NativeController, sink: &Sink) -> Result<(), HostError> {
    loop {
        let notified = sink.0.wake.notified();
        let next = {
            let backlog = sink.0.backlog.lock();
            if backlog.failed {
                return Err(HostError::Backlog);
            }
            backlog.rows.front().map(|(output, _)| output.clone())
        };
        let Some(next) = next else {
            notified.await;
            continue;
        };
        deliver(controller, sink, next).await?;
        let mut backlog = sink.0.backlog.lock();
        if let Some((_, bytes)) = backlog.rows.pop_front() {
            backlog.bytes -= bytes;
        }
    }
}

async fn finish_worker(
    controller: &NativeController,
    sink: &Sink,
    outcome: &Result<(), HostError>,
) -> Result<(), HostError> {
    if let Err(error) = outcome {
        sink.update(NativeUpdate::Notice {
            level: "error".into(),
            text: common::head(&error.to_string(), 1024),
        })?;
    }
    let mut view = sink.view();
    view.connected = false;
    view.capabilities = NativeCapabilities::default();
    sink.update(NativeUpdate::View { view })?;
    loop {
        let next = sink
            .0
            .backlog
            .lock()
            .rows
            .front()
            .map(|(output, _)| output.clone());
        let Some(next) = next else {
            return Ok(());
        };
        deliver(controller, sink, next).await?;
        let mut backlog = sink.0.backlog.lock();
        if let Some((_, bytes)) = backlog.rows.pop_front() {
            backlog.bytes -= bytes;
        }
    }
}

async fn deliver(
    controller: &NativeController,
    sink: &Sink,
    output: Output,
) -> Result<(), HostError> {
    loop {
        let result = match &output {
            Output::Update(update) => controller.publish(update.clone()).await.map(|_| ()),
            Output::Result(result) => controller.complete(result.clone()).await,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.is_retryable() => {
                if sink.0.backlog.lock().failed {
                    return Err(HostError::Backlog);
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(error) => return Err(HostError::Controller(error.to_string())),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn changed_and_retired_operations_never_dispatch() {
        let mut ledger = Ledger {
            entries: BTreeMap::new(),
        };
        let command = NativeCommand {
            operation_id: 1,
            activation: "a".into(),
            intent: NativeIntent::Prompt {
                text: "hello".into(),
            },
        };
        assert!(ledger.reserve(&command).unwrap().is_none());
        assert_eq!(
            ledger.reserve(&command).unwrap().unwrap().disposition,
            NativeDisposition::Unknown
        );
        let mut changed = command.clone();
        changed.activation = "b".into();
        assert!(ledger.reserve(&changed).is_err());
        for id in 2..=MAX_NATIVE_OPERATIONS as u64 {
            let mut next = command.clone();
            next.operation_id = u64::MAX - id;
            assert!(ledger.reserve(&next).unwrap().is_none());
        }
        let mut overflow = command.clone();
        overflow.operation_id = 2;
        assert!(ledger.reserve(&overflow).is_err());
        assert!(ledger.reserve(&command).unwrap().is_some());
    }
    pub(super) fn sink() -> Sink {
        sink_with_mode(common::TranscriptMode::Content)
    }
    fn sink_with_mode(transcript_mode: common::TranscriptMode) -> Sink {
        Sink(Arc::new(Shared {
            backlog: Mutex::new(Backlog::default()),
            wake: Notify::new(),
            responders: Mutex::new(BTreeMap::new()),
            transcript_mode,
            ledger: Mutex::new(Ledger {
                entries: BTreeMap::new(),
            }),
            view: Mutex::new(NativeView {
                activation: "a".into(),
                backend: "acp".into(),
                capabilities: NativeCapabilities::default(),
                connected: false,
                turn_id: None,
                provider_session_id: None,
                questions: Vec::new(),
            }),
        }))
    }
    #[test]
    fn identity_is_provider_supplied_and_activation_fenced() {
        let sink = sink();
        assert!(sink.view().provider_session_id.is_none());
        let mut view = sink.view();
        view.provider_session_id = Some("official-session".into());
        sink.update(NativeUpdate::View { view: view.clone() })
            .unwrap();
        view.provider_session_id = Some("different-session".into());
        assert!(sink.update(NativeUpdate::View { view }).is_err());
        assert_eq!(
            sink.view().provider_session_id.as_deref(),
            Some("official-session")
        );
    }
    #[test]
    fn final_receipt_does_not_regress_on_redelivery() {
        let sink = sink();
        let command = NativeCommand {
            operation_id: u64::MAX,
            activation: "a".into(),
            intent: NativeIntent::Prompt { text: "hi".into() },
        };
        sink.0.ledger.lock().reserve(&command).unwrap();
        sink.result(common::result(
            u64::MAX,
            NativeDisposition::Completed,
            "actual SDK completion",
        ))
        .unwrap();
        sink.result(common::result(
            u64::MAX,
            NativeDisposition::Queued,
            "stale queue stage",
        ))
        .unwrap();
        assert_eq!(sink.0.backlog.lock().rows.len(), 1);
        let receipt = sink.0.ledger.lock().reserve(&command).unwrap().unwrap();
        assert_eq!(receipt.disposition, NativeDisposition::Completed);
    }
    #[test]
    fn transcript_boundary_preserves_envelope_privacy_and_escaped_budget() {
        use phux_client_core::session::agent_transcript::{
            ToolStatus, TranscriptEntry, TranscriptRole,
        };
        let sink = sink();
        common::text(
            &sink,
            "omp",
            "reply".into(),
            TranscriptRole::Assistant,
            "\"\\\né".repeat(20000),
            true,
        )
        .unwrap();
        common::tool(
            &sink,
            "omp",
            "write".into(),
            "write".into(),
            common::summarize_args(&serde_json::json!({"path":"safe.rs","content":"PRIVATE BODY"})),
            "PRIVATE OUTPUT".into(),
            ToolStatus::Ok,
        )
        .unwrap();
        let backlog = sink.0.backlog.lock();
        for (row, _) in &backlog.rows {
            let Output::Update(update @ NativeUpdate::Transcript { entry }) = row else {
                panic!("transcript expected");
            };
            assert!(json_size(update).unwrap() <= UPDATE_BYTES);
            assert_eq!(entry["schema"], "phux.transcript/v1");
            assert!(TranscriptEntry::from_data(entry).is_some());
            assert!(!serde_json::to_string(update).unwrap().contains("PRIVATE"));
        }
        let Output::Update(NativeUpdate::Transcript { entry }) = &backlog.rows[0].0 else {
            unreachable!()
        };
        assert_eq!(entry["entry"]["truncated"], true);
        drop(backlog);
    }
    #[test]
    fn transcript_off_retains_lifecycle_and_full_requires_opt_in() {
        use phux_client_core::session::agent_transcript::{ToolStatus, TranscriptRole};
        let off = sink_with_mode(common::TranscriptMode::Off);
        common::text(
            &off,
            "omp",
            "p".into(),
            TranscriptRole::User,
            "private".into(),
            true,
        )
        .unwrap();
        off.state(NativeState::Working).unwrap();
        assert_eq!(off.0.backlog.lock().rows.len(), 1);
        let full = sink_with_mode(common::TranscriptMode::Full);
        common::tool(
            &full,
            "omp",
            "t".into(),
            "read".into(),
            String::new(),
            "opt-in output".into(),
            ToolStatus::Ok,
        )
        .unwrap();
        let backlog = full.0.backlog.lock();
        let Output::Update(NativeUpdate::Transcript { entry }) = &backlog.rows[0].0 else {
            unreachable!()
        };
        assert_eq!(entry["entry"]["tool"]["output"], "opt-in output");
        drop(backlog);
    }
    #[test]
    fn only_matching_definitive_refusal_releases_responder() {
        let sink = sink();
        for operation_id in [1, 2] {
            sink.0
                .ledger
                .lock()
                .reserve(&NativeCommand {
                    operation_id,
                    activation: "a".into(),
                    intent: NativeIntent::Answer {
                        question_id: "q".into(),
                        response: serde_json::json!({"values":{}}),
                    },
                })
                .unwrap();
        }
        sink.0.responders.lock().insert("q".into(), 2);
        sink.result(common::result(
            1,
            NativeDisposition::Refused,
            "old rejection",
        ))
        .unwrap();
        assert_eq!(sink.0.responders.lock().get("q"), Some(&2));
        sink.result(common::result(
            2,
            NativeDisposition::Unknown,
            "ambiguous dispatch",
        ))
        .unwrap();
        assert_eq!(sink.0.responders.lock().get("q"), Some(&2));
        sink.result(common::result(
            2,
            NativeDisposition::Refused,
            "not admitted",
        ))
        .unwrap();
        assert_eq!(sink.0.responders.lock().get("q"), Some(&2));
        let command = NativeCommand {
            operation_id: 3,
            activation: "a".into(),
            intent: NativeIntent::Answer {
                question_id: "correctable".into(),
                response: serde_json::json!({"values":{}}),
            },
        };
        sink.0.ledger.lock().reserve(&command).unwrap();
        sink.0.responders.lock().insert("correctable".into(), 3);
        sink.result(common::result(
            3,
            NativeDisposition::Refused,
            "local schema rejection",
        ))
        .unwrap();
        assert!(!sink.0.responders.lock().contains_key("correctable"));
    }
}
