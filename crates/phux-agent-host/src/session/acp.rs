//! Official ACP v1 connection, with optional surfaces explicitly unadvertised.
//! Cursor blocking extensions follow <https://cursor.com/docs/cli/acp>, not text
//! inference. Filesystem/terminal/auth callbacks are never implicitly approved.
#![allow(
    clippy::future_not_send,
    reason = "official ACP futures run only on the worker LocalSet"
)]
use super::{HostError, HostSpec, Sink, common};
use agent_client_protocol::{self as sdk, Agent as _};
use parking_lot::Mutex;
use phux_client_core::session::agent_transcript::{ToolStatus, TranscriptRole};
use phux_protocol::native_agent::{
    NativeCapabilities, NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent,
    NativeOption, NativeQuestion, NativeQuestionItem, NativeState, NativeUpdate, validate_payload,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, rc::Rc};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

struct ToolRow {
    title: String,
    summary: String,
    output: String,
    status: ToolStatus,
}
impl ToolRow {
    const fn new() -> Self {
        Self {
            title: String::new(),
            summary: String::new(),
            output: String::new(),
            status: ToolStatus::Running,
        }
    }
    fn update(
        &mut self,
        title: Option<String>,
        input: Option<Value>,
        output: Option<Value>,
        status: Option<sdk::ToolCallStatus>,
        include_output: bool,
    ) {
        if let Some(title) = title {
            self.title = common::head(&title, 256);
        }
        if let Some(input) = input {
            self.summary = common::summarize_args(&input);
        }
        if include_output && let Some(output) = output {
            self.output = common::head(&output.to_string(), 4096);
        }
        if let Some(status) = status {
            self.status = match status {
                sdk::ToolCallStatus::Completed => ToolStatus::Ok,
                sdk::ToolCallStatus::Failed => ToolStatus::Error,
                _ => ToolStatus::Running,
            };
        }
    }
}

#[derive(Default)]
struct Rows {
    serial: u64,
    question_serial: u64,
    current: Option<(String, TranscriptRole, String)>,
    prompt_user_id: Option<String>,
    tools: BTreeMap<String, ToolRow>,
}
impl Rows {
    fn reserve_tool(&mut self, id: &str) -> Result<(), HostError> {
        if self.tools.contains_key(id) || self.tools.len() < 128 {
            return Ok(());
        }
        let retired = self
            .tools
            .iter()
            .find_map(|(id, row)| (row.status != ToolStatus::Running).then(|| id.clone()));
        let Some(retired) = retired else {
            return Err(HostError::Sdk("too many active ACP tool calls".into()));
        };
        self.tools.remove(&retired);
        Ok(())
    }
}
struct Callback {
    sink: Sink,
    provider: String,
    // ACP runs on a LocalSet, but question waits borrow the callback across await.
    // Keep those borrows Send without moving the connection off its local runtime.
    waiting: Mutex<BTreeMap<String, oneshot::Sender<Value>>>,
    rows: Mutex<Rows>,
}
impl Callback {
    async fn ask(&self, mut question: NativeQuestion) -> sdk::Result<Value> {
        self.prepare_question(&mut question)?;
        let id = question.id.clone();
        let (tx, rx) = oneshot::channel();
        {
            let mut waiting = self.waiting.lock();
            if waiting.contains_key(&id) {
                return Err(sdk::Error::invalid_params());
            }
            waiting.insert(id.clone(), tx);
        }
        if let Err(error) = self
            .sink
            .update(NativeUpdate::QuestionOpened { question })
            .and_then(|()| self.sink.state(NativeState::Blocked))
        {
            self.waiting.lock().remove(&id);
            return Err(sdk::Error::internal_error().data(json!({"message":error.to_string()})));
        }
        let answer = rx.await.unwrap_or_else(|_| json!({"cancelled":true}));
        self.waiting.lock().remove(&id);
        self.sink
            .update(NativeUpdate::QuestionClosed {
                id,
                reason: "ACP callback returned explicit response".into(),
            })
            .map_err(|error| host_to_acp(&error))?;
        if self.waiting.lock().is_empty() && self.sink.view().turn_id.is_some() {
            self.sink
                .state(NativeState::Working)
                .map_err(|error| host_to_acp(&error))?;
        }
        Ok(answer)
    }
    fn prepare_question(&self, question: &mut NativeQuestion) -> sdk::Result<()> {
        // A tool can request permission repeatedly. Source tool IDs are not
        // callback identities and must not bypass the broker responder ledger.
        question.fields["source_question_id"] = Value::String(question.id.clone());
        {
            let mut rows = self.rows.lock();
            rows.question_serial = rows
                .question_serial
                .checked_add(1)
                .ok_or_else(sdk::Error::internal_error)?;
            let serial = rows.question_serial;
            drop(rows);
            question.id = format!("acp-question:{serial}");
        }
        common_bounds(question)?;
        if !self.sink.view().connected {
            let mut view = self.sink.view();
            view.connected = true;
            view.capabilities.questions = true.into();
            self.sink
                .update(NativeUpdate::View { view })
                .map_err(|error| host_to_acp(&error))?;
        }
        Ok(())
    }

    async fn cursor_ask(&self, mut fields: Value) -> sdk::Result<Value> {
        let ask: CursorAsk =
            serde_json::from_value(fields.clone()).map_err(|_| sdk::Error::invalid_params())?;
        fields["allow_custom_input"] = Value::Bool(false);
        let question = NativeQuestion {
            id: ask.tool_call_id,
            kind: "ask".into(),
            title: ask.title.unwrap_or_default(),
            message: String::new(),
            options: Vec::new(),
            questions: ask
                .questions
                .into_iter()
                .map(|question| NativeQuestionItem {
                    id: question.id,
                    question: question.prompt,
                    header: String::new(),
                    options: question
                        .options
                        .into_iter()
                        .map(|option| NativeOption {
                            id: option.id,
                            label: option.label,
                            description: None,
                        })
                        .collect(),
                    multi: question.allow_multiple,
                    recommended: None,
                })
                .collect(),
            fields,
        };
        let answer = self.ask(question).await?;
        Ok(if answer["cancelled"].as_bool() == Some(true) {
            json!({"outcome":{"outcome":"cancelled"}})
        } else {
            json!({"outcome":{"outcome":"answered","answers":answer["answers"].as_array().ok_or_else(sdk::Error::invalid_params)?.iter().map(|answer| json!({"questionId":answer["id"],"selectedOptionIds":answer["selected_options"]})).collect::<Vec<_>>()}})
        })
    }

    async fn cursor_plan(&self, fields: Value) -> sdk::Result<Value> {
        let plan: CursorPlan =
            serde_json::from_value(fields.clone()).map_err(|_| sdk::Error::invalid_params())?;
        let question = NativeQuestion {
            id: plan.tool_call_id,
            kind: "confirm".into(),
            title: plan.name.unwrap_or_else(|| "Approve provider plan".into()),
            message: plan.plan,
            options: Vec::new(),
            questions: Vec::new(),
            fields,
        };
        let answer = self.ask(question).await?;
        let outcome = if answer["cancelled"].as_bool() == Some(true) {
            "cancelled"
        } else if answer["confirmed"].as_bool() == Some(true) {
            "accepted"
        } else {
            "rejected"
        };
        Ok(json!({"outcome":{"outcome":outcome}}))
    }

    fn cancel_questions(&self) {
        let waiting = std::mem::take(&mut *self.waiting.lock());
        for (_, sender) in waiting {
            let _ = sender.send(json!({"cancelled":true}));
        }
    }
    fn finish_prompt(&self, id: u64) -> Result<(), HostError> {
        self.cancel_questions();
        self.flush_text()?;
        self.rows.lock().prompt_user_id = None;
        self.sink.reject_pending_turn(&format!("acp:{id}"))?;
        self.sink.state(NativeState::Done)
    }
    fn prompt_entry(&self, id: u64, text: &str) -> Result<(), HostError> {
        self.flush_text()?;
        common::text(
            &self.sink,
            &self.provider,
            format!("acp-prompt:{id}"),
            TranscriptRole::User,
            text.to_owned(),
            true,
        )?;
        self.rows.lock().prompt_user_id = Some(format!("acp-prompt:{id}"));
        Ok(())
    }
    fn flush_text(&self) -> Result<(), HostError> {
        let mut rows = self.rows.lock();
        if let Some((id, role, text)) = rows.current.take() {
            common::text(&self.sink, &self.provider, id, role, text, true)?;
        }
        drop(rows);
        Ok(())
    }
    fn chunk(&self, role: TranscriptRole, content: sdk::ContentBlock) -> Result<(), HostError> {
        let Some(text) = content_text(content) else {
            return self.sink.notice(
                "Provider emitted non-text content; native attachment rendering unavailable",
            );
        };
        if self.sink.view().turn_id.is_some() && self.waiting.lock().is_empty() {
            self.sink.state(NativeState::Working)?;
        }
        let mut rows = self.rows.lock();
        let current = match rows.current.take() {
            Some((id, previous, text)) if previous != role => {
                common::text(&self.sink, &self.provider, id, previous, text, true)?;
                None
            }
            current => current,
        };
        let (id, role, mut current) = current.unwrap_or_else(|| {
            rows.serial += 1;
            let id = if role == TranscriptRole::User {
                rows.prompt_user_id
                    .clone()
                    .unwrap_or_else(|| format!("acp-message:{}", rows.serial))
            } else {
                format!("acp-message:{}", rows.serial)
            };
            (id, role, String::new())
        });
        // Keep one additional byte so the standard transcript truncation marker
        // remains truthful, without retaining an unbounded provider response.
        let room = (12 * 1024 + 4_usize).saturating_sub(current.len());
        current.push_str(&common::head(&text, room));
        let (id, role, current) = rows.current.insert((id, role, current));
        let outcome = common::text(
            &self.sink,
            &self.provider,
            id.clone(),
            *role,
            current.clone(),
            false,
        );
        drop(rows);
        outcome
    }
    fn tool(
        &self,
        id: String,
        title: Option<String>,
        input: Option<Value>,
        output: Option<Value>,
        status: Option<sdk::ToolCallStatus>,
    ) -> Result<(), HostError> {
        self.flush_text()?;
        let mut rows = self.rows.lock();
        rows.reserve_tool(&id)?;
        let row = rows.tools.entry(id.clone()).or_insert_with(ToolRow::new);
        row.update(
            title,
            input,
            output,
            status,
            self.sink.0.transcript_mode == common::TranscriptMode::Full,
        );
        if status == Some(sdk::ToolCallStatus::InProgress) && self.waiting.lock().is_empty() {
            self.sink.state(NativeState::Working)?;
        }
        let outcome = common::tool(
            &self.sink,
            &self.provider,
            id,
            row.title.clone(),
            row.summary.clone(),
            row.output.clone(),
            row.status,
        );
        drop(rows);
        outcome
    }
    fn tool_update(&self, tool: sdk::ToolCallUpdate) -> Result<(), HostError> {
        self.tool(
            tool.tool_call_id.to_string(),
            tool.fields.title,
            tool.fields.raw_input,
            tool.fields.raw_output.or_else(|| {
                tool.fields
                    .content
                    .and_then(|content| serde_json::to_value(content).ok())
            }),
            tool.fields.status,
        )
    }

    fn answer(&self, id: u64, question_id: &str, response: Value) -> NativeCommandResult {
        let sender = self.waiting.lock().remove(question_id);
        sender.map_or_else(
            || {
                common::result(
                    id,
                    NativeDisposition::Refused,
                    "ACP callback is no longer pending",
                )
            },
            |sender| match sender.send(response) {
                Ok(()) => common::result(
                    id,
                    NativeDisposition::Queued,
                    "explicit response queued; not a provider-consumption receipt",
                ),
                Err(_) => common::result(
                    id,
                    NativeDisposition::Refused,
                    "ACP callback is no longer pending",
                ),
            },
        )
    }
    fn startup_command(&self, command: NativeCommand) -> NativeCommandResult {
        let id = command.operation_id;
        match command.intent {
            NativeIntent::Answer {
                question_id,
                response,
            } => self.answer(id, &question_id, response),
            _ => common::result(
                id,
                NativeDisposition::Refused,
                "ACP session initialization is not complete",
            ),
        }
    }
}
fn content_text(content: sdk::ContentBlock) -> Option<String> {
    match content {
        sdk::ContentBlock::Text(text) => Some(text.text),
        sdk::ContentBlock::ResourceLink(resource) => Some(resource.uri),
        _ => None,
    }
}

fn host_to_acp(error: &HostError) -> sdk::Error {
    sdk::Error::internal_error().data(json!({"message":error.to_string()}))
}
fn common_bounds(question: &NativeQuestion) -> sdk::Result<()> {
    validate_payload(question).map_err(|_| sdk::Error::invalid_params())
}
#[async_trait::async_trait(?Send)]
impl sdk::Client for Callback {
    async fn request_permission(
        &self,
        request: sdk::RequestPermissionRequest,
    ) -> sdk::Result<sdk::RequestPermissionResponse> {
        if self
            .sink
            .view()
            .provider_session_id
            .as_deref()
            .is_some_and(|id| id != request.session_id.to_string())
        {
            return Err(sdk::Error::invalid_params());
        }
        let fields = serde_json::to_value(&request).map_err(|_| sdk::Error::invalid_params())?;
        let question = NativeQuestion {
            id: request.tool_call.tool_call_id.to_string(),
            kind: "permission".into(),
            title: request
                .tool_call
                .fields
                .title
                .clone()
                .unwrap_or_else(|| "Tool permission".into()),
            message: String::new(),
            options: request
                .options
                .iter()
                .map(|option| NativeOption {
                    id: option.option_id.to_string(),
                    label: option.name.clone(),
                    description: None,
                })
                .collect(),
            questions: Vec::new(),
            fields,
        };
        let response = self.ask(question).await?;
        let outcome = if response["cancelled"].as_bool() == Some(true) {
            sdk::RequestPermissionOutcome::Cancelled
        } else {
            let id = response["option_id"]
                .as_str()
                .ok_or_else(sdk::Error::invalid_params)?;
            let option = request
                .options
                .iter()
                .find(|option| option.option_id.to_string() == id)
                .ok_or_else(sdk::Error::invalid_params)?;
            sdk::RequestPermissionOutcome::Selected(sdk::SelectedPermissionOutcome::new(
                option.option_id.clone(),
            ))
        };
        Ok(sdk::RequestPermissionResponse::new(outcome))
    }
    async fn session_notification(
        &self,
        notification: sdk::SessionNotification,
    ) -> sdk::Result<()> {
        if self
            .sink
            .view()
            .provider_session_id
            .as_deref()
            .is_some_and(|id| id != notification.session_id.to_string())
        {
            return Err(sdk::Error::invalid_params());
        }
        let result = match notification.update {
            sdk::SessionUpdate::UserMessageChunk(chunk) => {
                self.chunk(TranscriptRole::User, chunk.content)
            }
            sdk::SessionUpdate::AgentMessageChunk(chunk) => {
                self.chunk(TranscriptRole::Assistant, chunk.content)
            }
            sdk::SessionUpdate::AgentThoughtChunk(chunk) => {
                self.chunk(TranscriptRole::Thinking, chunk.content)
            }
            sdk::SessionUpdate::ToolCall(tool) => self.tool(
                tool.tool_call_id.to_string(),
                Some(tool.title),
                tool.raw_input,
                tool.raw_output
                    .or_else(|| serde_json::to_value(tool.content).ok()),
                Some(tool.status),
            ),
            sdk::SessionUpdate::ToolCallUpdate(tool) => self.tool_update(tool),
            sdk::SessionUpdate::Plan(plan) => self
                .sink
                .notice(&serde_json::to_string(&plan).map_err(|_| sdk::Error::invalid_params())?),
            _ => Ok(()),
        };
        result.map_err(|error| host_to_acp(&error))
    }
    async fn ext_method(&self, request: sdk::ExtRequest) -> sdk::Result<sdk::ExtResponse> {
        if self.provider != "cursor-agent" && self.provider != "cursor" {
            return Err(sdk::Error::method_not_found());
        }
        let fields: Value =
            serde_json::from_str(request.params.get()).map_err(|_| sdk::Error::invalid_params())?;
        let response = match request.method.as_ref() {
            "cursor/ask_question" => self.cursor_ask(fields).await?,
            "cursor/create_plan" => self.cursor_plan(fields).await?,
            _ => return Err(sdk::Error::method_not_found()),
        };
        let raw =
            serde_json::value::to_raw_value(&response).map_err(|_| sdk::Error::internal_error())?;
        Ok(sdk::ExtResponse::new(raw.into()))
    }
    async fn ext_notification(&self, notification: sdk::ExtNotification) -> sdk::Result<()> {
        match notification.method.as_ref() {
            "cursor/update_todos" | "cursor/task" | "cursor/generate_image" => self
                .sink
                .notice(&format!(
                    "{}: {}",
                    notification.method,
                    notification.params.get()
                ))
                .map_err(|error| host_to_acp(&error)),
            _ => Err(sdk::Error::method_not_found()),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorAsk {
    tool_call_id: String,
    title: Option<String>,
    questions: Vec<CursorQuestion>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorQuestion {
    id: String,
    prompt: String,
    options: Vec<CursorOption>,
    #[serde(default)]
    allow_multiple: bool,
}
#[derive(Deserialize)]
struct CursorOption {
    id: String,
    label: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorPlan {
    tool_call_id: String,
    name: Option<String>,
    plan: String,
}

pub(super) fn run(
    spec: HostSpec,
    sink: Sink,
    commands: mpsc::Receiver<NativeCommand>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), HostError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime
        .block_on(tokio::task::LocalSet::new().run_until(run_local(spec, sink, commands, shutdown)))
}

fn sdk_error(error: &sdk::Error) -> HostError {
    HostError::Sdk(error.to_string())
}

type IoTask = tokio::task::JoinHandle<sdk::Result<()>>;

async fn run_local(
    spec: HostSpec,
    sink: Sink,
    mut commands: mpsc::Receiver<NativeCommand>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), HostError> {
    let mut process = std::process::Command::new(&spec.command[0]);
    process.args(&spec.command[1..]);
    common::configure(&mut process, &spec);
    let mut child = tokio::process::Command::from(process)
        .kill_on_drop(true)
        .spawn()?;
    let outgoing = child
        .stdin
        .take()
        .ok_or_else(|| HostError::Sdk("missing ACP stdin".into()))?
        .compat_write();
    let incoming = child
        .stdout
        .take()
        .ok_or_else(|| HostError::Sdk("missing ACP stdout".into()))?
        .compat();
    let callback = Rc::new(Callback {
        sink: sink.clone(),
        provider: spec.provider.clone(),
        waiting: Mutex::new(BTreeMap::new()),
        rows: Mutex::new(Rows::default()),
    });
    let (connection, io) =
        sdk::ClientSideConnection::new(callback.clone(), outgoing, incoming, |future| {
            tokio::task::spawn_local(future);
        });
    let connection = Rc::new(connection);
    let mut io = tokio::task::spawn_local(io);
    let outcome = async {
        let Some(id) = startup(
            &spec,
            &connection,
            &callback,
            &mut commands,
            &mut shutdown,
            &mut io,
        )
        .await?
        else {
            return Ok(());
        };
        let session = Session {
            connection,
            callback: callback.clone(),
            id,
        };
        session.serve(&mut commands, &mut shutdown, &mut io).await
    }
    .await;
    callback.cancel_questions();
    let _ = child.kill().await;
    let _ = child.wait().await;
    io.abort();
    outcome
}

async fn initialize(
    spec: &HostSpec,
    connection: &sdk::ClientSideConnection,
) -> Result<sdk::SessionId, HostError> {
    // Default capabilities explicitly exclude client filesystem and terminal execution.
    // The SDK's default callbacks return method_not_found even if a peer ignores negotiation.
    let initialized = connection
        .initialize(
            sdk::InitializeRequest::new(sdk::ProtocolVersion::V1)
                .client_capabilities(sdk::ClientCapabilities::default())
                .client_info(sdk::Implementation::new(
                    "phux-agent-host",
                    env!("CARGO_PKG_VERSION"),
                )),
        )
        .await
        .map_err(|error| sdk_error(&error))?;
    if initialized.protocol_version != sdk::ProtocolVersion::V1 {
        return Err(HostError::Sdk("ACP protocol version unsupported".into()));
    }
    if let Some(id) = &spec.native_id {
        if !initialized.agent_capabilities.load_session {
            return Err(HostError::Sdk(
                "provider does not advertise session/load".into(),
            ));
        }
        let id = sdk::SessionId::new(id.clone());
        connection
            .load_session(sdk::LoadSessionRequest::new(id.clone(), spec.cwd.clone()))
            .await
            .map_err(|error| sdk_error(&error))?;
        Ok(id)
    } else {
        connection.new_session(sdk::NewSessionRequest::new(spec.cwd.clone())).await
            .map(|session| session.session_id)
            .map_err(|error| HostError::Sdk(format!("ACP session initialization failed (authenticate externally if required): {error}")))
    }
}

async fn startup(
    spec: &HostSpec,
    connection: &sdk::ClientSideConnection,
    callback: &Callback,
    commands: &mut mpsc::Receiver<NativeCommand>,
    shutdown: &mut watch::Receiver<bool>,
    io: &mut IoTask,
) -> Result<Option<sdk::SessionId>, HostError> {
    let initialization = initialize(spec, connection);
    tokio::pin!(initialization);
    loop {
        tokio::select! {
            result = &mut initialization => {
                let id = result?;
                callback.flush_text()?;
                let mut view = callback.sink.view();
                view.connected = true;
                view.provider_session_id = Some(id.to_string());
                view.capabilities = NativeCapabilities {
                    prompt: true.into(), interrupt: true.into(), questions: true.into(), follow_up: false.into(),
                };
                callback.sink.update(NativeUpdate::View { view })?;
                return Ok(Some(id));
            }
            result = &mut *io => return Err(transport_error(result)),
            _ = shutdown.changed() => return Ok(None),
            command = commands.recv() => {
                let Some(command) = command else { return Ok(None); };
                callback.sink.result(callback.startup_command(command))?;
            }
        }
    }
}

fn transport_error(result: Result<sdk::Result<()>, tokio::task::JoinError>) -> HostError {
    match result {
        Ok(Ok(())) => HostError::Sdk("ACP transport closed".into()),
        Ok(Err(error)) => sdk_error(&error),
        Err(error) => HostError::Sdk(error.to_string()),
    }
}

struct Session {
    connection: Rc<sdk::ClientSideConnection>,
    callback: Rc<Callback>,
    id: sdk::SessionId,
}
impl Session {
    async fn serve(
        &self,
        commands: &mut mpsc::Receiver<NativeCommand>,
        shutdown: &mut watch::Receiver<bool>,
        io: &mut IoTask,
    ) -> Result<(), HostError> {
        loop {
            tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                result = &mut *io => return Err(transport_error(result)),
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    self.dispatch(command).await?;
                }
            }
        }
    }
    async fn dispatch(&self, command: NativeCommand) -> Result<(), HostError> {
        let id = command.operation_id;
        match command.intent {
            NativeIntent::Prompt { text } => self.prompt(id, text),
            NativeIntent::Interrupt { turn_id } => self.interrupt(id, turn_id).await,
            NativeIntent::Answer {
                question_id,
                response,
            } => self
                .callback
                .sink
                .result(self.callback.answer(id, &question_id, response)),
        }
    }
    fn prompt(&self, id: u64, text: String) -> Result<(), HostError> {
        let sink = &self.callback.sink;
        if sink.view().turn_id.is_some() {
            return sink.result(common::result(
                id,
                NativeDisposition::Refused,
                "ACP turn already active",
            ));
        }
        sink.begin_prompt(format!("acp:{id}"))?;
        sink.state(NativeState::Working)?;
        self.callback.prompt_entry(id, &text)?;
        let connection = self.connection.clone();
        let session_id = self.id.clone();
        let callback = self.callback.clone();
        // ACP's prompt response is end-of-turn, never an admission receipt.
        sink.result(common::result(
            id,
            NativeDisposition::Queued,
            "ACP prompt scheduled; ACP v1 has no admission receipt",
        ))?;
        tokio::task::spawn_local(async move {
            let result = connection
                .prompt(sdk::PromptRequest::new(session_id, vec![text.into()]))
                .await;
            let _ = callback.finish_prompt(id);
            let receipt = match result {
                Ok(response) => common::result(
                    id,
                    NativeDisposition::Completed,
                    &format!("ACP stop reason: {:?}", response.stop_reason),
                ),
                Err(error) => common::result(id, NativeDisposition::Unknown, &error.to_string()),
            };
            let _ = callback.sink.result(receipt);
        });
        Ok(())
    }
    async fn interrupt(&self, id: u64, turn_id: String) -> Result<(), HostError> {
        let sink = &self.callback.sink;
        if sink.view().turn_id.as_ref() != Some(&turn_id) {
            return sink.result(common::result(
                id,
                NativeDisposition::Refused,
                "ACP turn already ended",
            ));
        }
        self.callback.cancel_questions();
        let result = self
            .connection
            .cancel(sdk::CancelNotification::new(self.id.clone()))
            .await;
        let receipt = match result {
            Ok(()) => common::result(
                id,
                NativeDisposition::Queued,
                "ACP cancel notification sent; awaiting actual stop reason",
            ),
            Err(error) => common::result(id, NativeDisposition::Unknown, &error.to_string()),
        };
        sink.result(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unsupported_client_execution_callbacks_fail_explicitly() {
        use sdk::Client as _;
        let callback = Callback {
            sink: crate::session::tests::sink(),
            provider: "custom".into(),
            waiting: Mutex::new(BTreeMap::new()),
            rows: Mutex::new(Rows::default()),
        };
        let session = sdk::SessionId::new("session");
        let read = callback
            .read_text_file(sdk::ReadTextFileRequest::new(
                session.clone(),
                "/never-read",
            ))
            .await
            .unwrap_err();
        assert_eq!(read.code, sdk::ErrorCode::MethodNotFound);
        let terminal = callback
            .create_terminal(sdk::CreateTerminalRequest::new(session, "never-executed"))
            .await
            .unwrap_err();
        assert_eq!(terminal.code, sdk::ErrorCode::MethodNotFound);
        let params = serde_json::value::to_raw_value(&json!({})).unwrap();
        let extension = callback
            .ext_method(sdk::ExtRequest::new("unknown", params.into()))
            .await
            .unwrap_err();
        assert_eq!(extension.code, sdk::ErrorCode::MethodNotFound);
    }

    #[test]
    fn ended_prompt_releases_turn_and_callbacks_without_disconnect() {
        let sink = crate::session::tests::sink();
        let mut view = sink.view();
        view.connected = true;
        view.turn_id = Some("acp:7".into());
        sink.update(NativeUpdate::View { view }).unwrap();
        let (sender, mut receiver) = oneshot::channel();
        let callback = Callback {
            sink: sink.clone(),
            provider: "cursor".into(),
            waiting: Mutex::new(BTreeMap::from([("permission".into(), sender)])),
            rows: Mutex::new(Rows {
                prompt_user_id: Some("acp-prompt:7".into()),
                ..Rows::default()
            }),
        };
        callback.finish_prompt(7).unwrap();
        assert!(sink.view().connected);
        assert!(sink.view().turn_id.is_none());
        assert!(callback.rows.lock().prompt_user_id.is_none());
        assert_eq!(receiver.try_recv().unwrap()["cancelled"], true);
        assert!(!sink.begin_prompt("acp:8".into()).unwrap());
        assert_eq!(sink.view().turn_id.as_deref(), Some("acp:8"));
    }
    #[test]
    fn loaded_history_and_prompt_without_user_echo_have_distinct_rows() {
        let callback = Callback {
            sink: crate::session::tests::sink(),
            provider: "custom".into(),
            waiting: Mutex::new(BTreeMap::new()),
            rows: Mutex::new(Rows::default()),
        };
        callback
            .chunk(TranscriptRole::Assistant, "historical".into())
            .unwrap();
        callback.prompt_entry(7, "new user").unwrap();
        callback
            .chunk(TranscriptRole::Assistant, "fresh reply".into())
            .unwrap();
        callback.flush_text().unwrap();
        let backlog = callback.sink.0.backlog.lock();
        let final_rows: Vec<_> = backlog
            .rows
            .iter()
            .filter_map(|(output, _)| {
                let crate::session::Output::Update(NativeUpdate::Transcript { entry }) = output
                else {
                    return None;
                };
                let row =
                    phux_client_core::session::agent_transcript::TranscriptEntry::from_data(entry)?;
                row.is_final.then_some(row)
            })
            .collect();
        assert_eq!(
            final_rows
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>(),
            ["historical", "new user", "fresh reply"]
        );
        assert_ne!(final_rows[0].id, final_rows[2].id);
        drop(backlog);
    }

    #[test]
    fn chunks_preserve_row_identity_until_role_changes() {
        let callback = Callback {
            sink: crate::session::tests::sink(),
            provider: "cursor".into(),
            waiting: Mutex::new(BTreeMap::new()),
            rows: Mutex::new(Rows {
                prompt_user_id: Some("acp-prompt:7".into()),
                ..Rows::default()
            }),
        };
        callback
            .chunk(TranscriptRole::User, "first".into())
            .unwrap();
        callback
            .chunk(TranscriptRole::User, " second".into())
            .unwrap();
        callback
            .chunk(TranscriptRole::Assistant, "reply".into())
            .unwrap();
        callback.flush_text().unwrap();
        let backlog = callback.sink.0.backlog.lock();
        let rows: Vec<_> = backlog
            .rows
            .iter()
            .filter_map(|(output, _)| {
                let crate::session::Output::Update(NativeUpdate::Transcript { entry }) = output
                else {
                    return None;
                };
                phux_client_core::session::agent_transcript::TranscriptEntry::from_data(entry)
            })
            .collect();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].id, "acp-prompt:7");
        assert_eq!(rows[1].id, rows[0].id);
        assert_eq!(rows[1].text, "first second");
        assert!(!rows[1].is_final);
        assert_eq!(rows[2].id, rows[0].id);
        assert_eq!(rows[2].text, "first second");
        assert!(rows[2].is_final);
        assert_ne!(rows[3].id, rows[0].id);
        assert_eq!(rows[3].role, TranscriptRole::Assistant);
        assert_eq!(rows[4].id, rows[3].id);
        assert_eq!(rows[4].text, "reply");
        assert!(rows[4].is_final);
        drop(backlog);
    }

    #[tokio::test]
    async fn question_wait_requires_matching_explicit_answer() {
        let callback = Callback {
            sink: crate::session::tests::sink(),
            provider: "cursor".into(),
            waiting: Mutex::new(BTreeMap::new()),
            rows: Mutex::new(Rows::default()),
        };
        let question = NativeQuestion {
            id: "permission".into(),
            kind: "permission".into(),
            title: "Tool permission".into(),
            message: String::new(),
            options: vec![NativeOption {
                id: "allow-once".into(),
                label: "Allow once".into(),
                description: None,
            }],
            questions: Vec::new(),
            fields: json!({}),
        };
        let wait = callback.ask(question);
        let (answer, result) = tokio::join!(wait, async {
            tokio::task::yield_now().await;
            let refused = callback.startup_command(NativeCommand {
                operation_id: 1,
                activation: "a".into(),
                intent: NativeIntent::Answer {
                    question_id: "other-permission".into(),
                    response: json!({"option_id":"allow-once"}),
                },
            });
            assert_eq!(refused.disposition, NativeDisposition::Refused);
            assert!(callback.waiting.lock().contains_key("acp-question:1"));
            callback.startup_command(NativeCommand {
                operation_id: 2,
                activation: "a".into(),
                intent: NativeIntent::Answer {
                    question_id: "acp-question:1".into(),
                    response: json!({"option_id":"allow-once"}),
                },
            })
        });
        assert_eq!(answer.unwrap(), json!({"option_id":"allow-once"}));
        assert_eq!(result.operation_id, 2);
        assert_eq!(result.disposition, NativeDisposition::Queued);
        assert!(callback.waiting.lock().is_empty());
        assert!(callback.sink.view().questions.is_empty());
    }
}
