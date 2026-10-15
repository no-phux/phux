//! Official ACP v1 connection, with optional surfaces explicitly unadvertised.
//! Cursor blocking extensions follow <https://cursor.com/docs/cli/acp>, not text
//! inference. Filesystem/terminal/auth callbacks are never implicitly approved.
use crate::{HostError, HostSpec, Sink, common};
use agent_client_protocol::{self as sdk, Agent as _};
use phux_client_core::session::agent_transcript::{ToolStatus, TranscriptRole};
use parking_lot::Mutex;
use phux_protocol::native_agent::{
    NativeCapabilities, NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent,
    NativeOption, NativeQuestion, NativeQuestionItem, NativeState, NativeUpdate, validate_payload,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, rc::Rc};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Default)]
struct Rows {
    serial: u64,
    current: Option<(String, TranscriptRole, String)>,
    prompt_user_id: Option<String>,
    tools: BTreeMap<String, (String, String, String, ToolStatus)>,
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
    async fn ask(&self, question: NativeQuestion) -> sdk::Result<Value> {
        common_bounds(&question)?;
        if !self.sink.view().connected {
            let mut view = self.sink.view();
            view.connected = true;
            view.capabilities.questions = true.into();
            self.sink
                .update(NativeUpdate::View { view })
                .map_err(|error| host_to_acp(&error))?;
        }
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
    fn flush_text(&self) -> Result<(), HostError> {
        if let Some((id, role, text)) = self.rows.lock().current.take() {
            common::text(&self.sink, &self.provider, id, role, text, true)?;
        }
        Ok(())
    }
    fn chunk(&self, role: TranscriptRole, content: sdk::ContentBlock) -> Result<(), HostError> {
        let text =
            match content {
                sdk::ContentBlock::Text(text) => text.text,
                sdk::ContentBlock::ResourceLink(resource) => resource.uri,
                _ => return self.sink.notice(
                    "Provider emitted non-text content; native attachment rendering unavailable",
                ),
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
        common::text(
            &self.sink,
            &self.provider,
            id.clone(),
            *role,
            current.clone(),
            false,
        )
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
        if !rows.tools.contains_key(&id) && rows.tools.len() >= 128 {
            let retired = rows.tools.iter().find_map(|(id, (_, _, _, status))| {
                (*status != ToolStatus::Running).then(|| id.clone())
            });
            if let Some(retired) = retired {
                rows.tools.remove(&retired);
            } else {
                return Err(HostError::Sdk("too many active ACP tool calls".into()));
            }
        }
        let row = rows.tools.entry(id.clone()).or_insert_with(|| {
            (
                String::new(),
                String::new(),
                String::new(),
                ToolStatus::Running,
            )
        });
        if let Some(title) = title {
            row.0 = common::head(&title, 256);
        }
        if let Some(input) = input {
            row.1 = common::summarize_args(&input);
        }
        if self.sink.0.transcript_mode == common::TranscriptMode::Full
            && let Some(output) = output
        {
            row.2 = common::head(&output.to_string(), 4096);
        }
        if let Some(status) = status {
            row.3 = match status {
                sdk::ToolCallStatus::Completed => ToolStatus::Ok,
                sdk::ToolCallStatus::Failed => ToolStatus::Error,
                _ => ToolStatus::Running,
            };
        }
        if status == Some(sdk::ToolCallStatus::InProgress) && self.waiting.lock().is_empty() {
            self.sink.state(NativeState::Working)?;
        }
        common::tool(
            &self.sink,
            &self.provider,
            id,
            row.0.clone(),
            row.1.clone(),
            row.2.clone(),
            row.3,
        )
    }
    fn startup_command(&self, command: NativeCommand) -> NativeCommandResult {
        let id = command.operation_id;
        match command.intent {
            NativeIntent::Answer { question_id, response } => {
                let sender = self.waiting.lock().remove(&question_id);
                sender.map_or_else(
                    || common::result(id, NativeDisposition::Refused, "ACP startup dialog is no longer pending"),
                    |sender| match sender.send(response) {
                        Ok(()) => common::result(id, NativeDisposition::Queued, "explicit answer queued during ACP startup"),
                        Err(_) => common::result(id, NativeDisposition::Refused, "ACP startup dialog is no longer pending"),
                    },
                )
            }
            _ => common::result(id, NativeDisposition::Refused, "ACP session initialization is not complete"),
        }
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
            sdk::SessionUpdate::ToolCallUpdate(tool) => self.tool(
                tool.tool_call_id.to_string(),
                tool.fields.title,
                tool.fields.raw_input,
                tool.fields.raw_output.or_else(|| {
                    tool.fields
                        .content
                        .and_then(|content| serde_json::to_value(content).ok())
                }),
                tool.fields.status,
            ),
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
        let mut fields: Value =
            serde_json::from_str(request.params.get()).map_err(|_| sdk::Error::invalid_params())?;
        let response = match request.method.as_ref() {
            "cursor/ask_question" => {
                let ask: CursorAsk = serde_json::from_value(fields.clone())
                    .map_err(|_| sdk::Error::invalid_params())?;
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
                if answer["cancelled"].as_bool() == Some(true) {
                    json!({"outcome":{"outcome":"cancelled"}})
                } else {
                    json!({"outcome":{"outcome":"answered","answers":answer["answers"].as_array().ok_or_else(sdk::Error::invalid_params)?.iter().map(|answer| json!({"questionId":answer["id"],"selectedOptionIds":answer["selected_options"]})).collect::<Vec<_>>()}})
                }
            }
            "cursor/create_plan" => {
                let plan: CursorPlan = serde_json::from_value(fields.clone())
                    .map_err(|_| sdk::Error::invalid_params())?;
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
                json!({"outcome":{"outcome":outcome}})
            }
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

pub fn run(
    spec: HostSpec,
    sink: Sink,
    mut commands: mpsc::Receiver<NativeCommand>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), HostError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async move {
        let mut process = std::process::Command::new(&spec.command[0]); process.args(&spec.command[1..]); common::configure(&mut process, &spec);
        let mut command = tokio::process::Command::from(process); command.kill_on_drop(true);
        let mut child = command.spawn()?;
        let outgoing = child.stdin.take().ok_or_else(|| HostError::Sdk("missing ACP stdin".into()))?.compat_write();
        let incoming = child.stdout.take().ok_or_else(|| HostError::Sdk("missing ACP stdout".into()))?.compat();
        let callback = Rc::new(Callback { sink: sink.clone(), provider: spec.provider.clone(), waiting: Mutex::new(BTreeMap::new()), rows: Mutex::new(Rows::default()) });
        let (connection, io) = sdk::ClientSideConnection::new(callback.clone(), outgoing, incoming, |future| { tokio::task::spawn_local(future); });
        let connection = Rc::new(connection);
        let mut io = tokio::task::spawn_local(io);
        let startup = async {
            let initialized = connection.initialize(sdk::InitializeRequest::new(sdk::ProtocolVersion::V1).client_info(sdk::Implementation::new("phux-agent-host", env!("CARGO_PKG_VERSION")))).await.map_err(|error| HostError::Sdk(error.to_string()))?;
            if initialized.protocol_version != sdk::ProtocolVersion::V1 { return Err(HostError::Sdk("ACP protocol version unsupported".into())); }
            let id = if let Some(id) = &spec.native_id {
                if !initialized.agent_capabilities.load_session { return Err(HostError::Sdk("provider does not advertise session/load".into())); }
                let id = sdk::SessionId::new(id.clone());
                connection.load_session(sdk::LoadSessionRequest::new(id.clone(), spec.cwd.clone())).await.map_err(|error| HostError::Sdk(error.to_string()))?;
                id
            } else { connection.new_session(sdk::NewSessionRequest::new(spec.cwd.clone())).await.map_err(|error| HostError::Sdk(format!("ACP session initialization failed (authenticate externally if required): {error}")))?.session_id };
            let mut view = sink.view(); view.connected = true; view.provider_session_id = Some(id.to_string());
            view.capabilities = NativeCapabilities { prompt: true.into(), interrupt: true.into(), questions: true.into(), follow_up: false.into() };
            sink.update(NativeUpdate::View { view })?;
            Ok::<_, HostError>(id)
        };
        tokio::pin!(startup);
        let startup = loop {
            tokio::select! {
                result = &mut startup => break result,
                _ = shutdown.changed() => { child.kill().await?; child.wait().await?; return Ok(()); }
                command = commands.recv() => {
                    let Some(command) = command else { child.kill().await?; child.wait().await?; return Ok(()); };
                    sink.result(callback.startup_command(command))?;
                }
            }
        };
        let session_id = match startup { Ok(id) => id, Err(error) => { let _ = child.kill().await; let _ = child.wait().await; return Err(error); } };
        let result = loop {
            tokio::select! {
                _ = shutdown.changed() => break Ok(()),
                result = &mut io => break result.map_err(|error| HostError::Sdk(error.to_string())).and_then(|result| result.map_err(|error| HostError::Sdk(error.to_string()))),
                command = commands.recv() => {
                    let Some(command) = command else { break Ok(()); };
                    let id = command.operation_id;
                    match command.intent {
                        NativeIntent::Prompt { text } => {
                            if sink.view().turn_id.is_some() { sink.result(common::result(id, NativeDisposition::Refused, "ACP turn already active"))?; continue; }
                            let mut view = sink.view(); view.turn_id = Some(format!("acp:{id}")); sink.update(NativeUpdate::View { view })?;
                            common::text(&sink, &spec.provider, format!("acp-prompt:{id}"), TranscriptRole::User, text.clone(), true)?;
                            callback.rows.lock().prompt_user_id = Some(format!("acp-prompt:{id}"));
                            let connection = connection.clone(); let session_id = session_id.clone(); let sink = sink.clone(); let callback = callback.clone();
                            tokio::task::spawn_local(async move {
                                // ACP response is end-of-turn, not prompt admission. Sending a
                                // request future is reported queued; never fabricate admission.
                                let response = connection.prompt(sdk::PromptRequest::new(session_id, vec![text.into()]));
                                let _ = sink.result(common::result(id, NativeDisposition::Queued, "ACP prompt request scheduled; no admission receipt in ACP v1"));
                                match response.await {
                                    Ok(_) => {
                                        let _ = callback.finish_prompt(id);
                                        let _ = sink.result(common::result(id, NativeDisposition::Completed, "ACP provider returned actual prompt stop reason"));
                                    }
                                    Err(error) => {
                                        // The prompt RPC ended, not the transport. Its outcome can
                                        // remain uncertain without fencing the next request forever.
                                        let _ = callback.finish_prompt(id);
                                        let _ = sink.result(common::result(id, NativeDisposition::Unknown, &error.to_string()));
                                        let _ = sink.notice(&error.to_string());
                                    }
                                }
                            });
                        }
                        NativeIntent::Interrupt { turn_id } => {
                            if sink.view().turn_id.as_ref() != Some(&turn_id) { sink.result(common::result(id, NativeDisposition::Refused, "ACP turn already ended"))?; continue; }
                            callback.cancel_questions();
                            let result = connection.cancel(sdk::CancelNotification::new(session_id.clone())).await;
                            sink.result(match result { Ok(()) => common::result(id, NativeDisposition::Queued, "ACP cancellation notification queued; session remains open"), Err(error) => common::result(id, NativeDisposition::Unknown, &error.to_string()) })?;
                        }
                        NativeIntent::Answer { question_id, response } => {
                            let sender = callback.waiting.lock().remove(&question_id);
                            let result = sender.map_or_else(
                                || common::result(id, NativeDisposition::Refused, "ACP question cancelled before dispatch"),
                                |sender| match sender.send(response) {
                                    Ok(()) => common::result(id, NativeDisposition::Queued, "explicit response queued to ACP callback; not provider-consumption receipt"),
                                    Err(_) => common::result(id, NativeDisposition::Refused, "ACP callback no longer pending"),
                                },
                            );
                            sink.result(result)?;
                        }
                    }
                }
            }
        };
        callback.cancel_questions();
        let _ = child.kill().await; let _ = child.wait().await;
        io.abort();
        result
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_schema_keeps_actual_option_ids() {
        let ask: CursorAsk = serde_json::from_value(json!({"toolCallId":"call_123","questions":[{"id":"q1","prompt":"Mode?","options":[{"id":"agent","label":"Agent"}]}]})).unwrap();
        assert_eq!(ask.tool_call_id, "call_123");
        assert_eq!(ask.questions[0].options[0].id, "agent");
        assert!(!ask.questions[0].allow_multiple);
    }
    #[test]
    fn ended_prompt_releases_turn_and_callbacks_without_disconnect() {
        let sink = crate::tests::sink();
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
    fn chunks_preserve_row_identity_until_role_changes() {
        let callback = Callback {
            sink: crate::tests::sink(),
            provider: "cursor".into(),
            waiting: Mutex::new(BTreeMap::new()),
            rows: Mutex::new(Rows {
                prompt_user_id: Some("acp-prompt:7".into()),
                ..Rows::default()
            }),
        };
        callback.chunk(TranscriptRole::User, "first".into()).unwrap();
        callback.chunk(TranscriptRole::User, " second".into()).unwrap();
        assert_eq!(
            callback.rows.lock().current,
            Some(("acp-prompt:7".into(), TranscriptRole::User, "first second".into()))
        );
        callback.chunk(TranscriptRole::Assistant, "reply".into()).unwrap();
        assert_eq!(
            callback.rows.lock().current,
            Some(("acp-message:2".into(), TranscriptRole::Assistant, "reply".into()))
        );
        callback.flush_text().unwrap();
        assert!(callback.rows.lock().current.is_none());
    }

    #[tokio::test]
    async fn question_wait_is_send_and_requires_matching_explicit_answer() {
        fn assert_send<T: Send>(_: &T) {}

        let callback = Callback {
            sink: crate::tests::sink(),
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
        assert_send(&wait);
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
            assert!(callback.waiting.lock().contains_key("permission"));
            callback.startup_command(NativeCommand {
                operation_id: 2,
                activation: "a".into(),
                intent: NativeIntent::Answer {
                    question_id: "permission".into(),
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
