//! Client callbacks: transcript updates, explicit permission answers, and Cursor extensions.
use super::{Callback, HostError, ToolRow, common, sdk};
use phux_client_core::session::agent_transcript::TranscriptRole;
use phux_protocol::native_agent::{
    NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent, NativeOption,
    NativeQuestion, NativeQuestionItem, NativeState, NativeUpdate, validate_payload,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;

impl Callback {
    pub(super) async fn ask(&self, mut question: NativeQuestion) -> sdk::Result<Value> {
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

    pub(super) fn cancel_questions(&self) {
        let waiting = std::mem::take(&mut *self.waiting.lock());
        for (_, sender) in waiting {
            let _ = sender.send(json!({"cancelled":true}));
        }
    }
    pub(super) fn finish_prompt(&self, id: u64) -> Result<(), HostError> {
        self.cancel_questions();
        self.flush_text()?;
        self.rows.lock().prompt_user_id = None;
        self.sink.reject_pending_turn(&format!("acp:{id}"))?;
        self.sink.state(NativeState::Done)
    }
    pub(super) fn prompt_entry(&self, id: u64, text: &str) -> Result<(), HostError> {
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
    pub(super) fn flush_text(&self) -> Result<(), HostError> {
        let mut rows = self.rows.lock();
        if let Some((id, role, text)) = rows.current.take() {
            common::text(&self.sink, &self.provider, id, role, text, true)?;
        }
        drop(rows);
        Ok(())
    }
    pub(super) fn chunk(
        &self,
        role: TranscriptRole,
        content: sdk::ContentBlock,
    ) -> Result<(), HostError> {
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
        let (id, role, mut current) = current.unwrap_or_else(|| rows.new_message(role));
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

    pub(super) fn answer(
        &self,
        id: u64,
        question_id: &str,
        response: Value,
    ) -> NativeCommandResult {
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
    pub(super) fn startup_command(&self, command: NativeCommand) -> NativeCommandResult {
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

pub(super) fn host_to_acp(error: &HostError) -> sdk::Error {
    sdk::Error::internal_error().data(json!({"message":error.to_string()}))
}
fn common_bounds(question: &NativeQuestion) -> sdk::Result<()> {
    validate_payload(question).map_err(|_| sdk::Error::invalid_params())
}
impl Callback {
    pub(super) async fn request(
        &self,
        request: sdk::AgentRequest,
    ) -> sdk::Result<sdk::ClientResponse> {
        match request {
            sdk::AgentRequest::RequestPermissionRequest(request) => self
                .request_permission(request)
                .await
                .map(sdk::ClientResponse::RequestPermissionResponse),
            sdk::AgentRequest::ExtMethodRequest(request) => self
                .ext_method(request)
                .await
                .map(sdk::ClientResponse::ExtMethodResponse),
            // No filesystem, terminal, authentication, or elicitation execution.
            _ => Err(sdk::Error::method_not_found()),
        }
    }

    fn validate_session(&self, id: &sdk::SessionId) -> sdk::Result<()> {
        if self
            .sink
            .view()
            .provider_session_id
            .as_deref()
            .is_some_and(|expected| expected != id.to_string())
        {
            return Err(sdk::Error::invalid_params());
        }
        Ok(())
    }
    async fn request_permission(
        &self,
        request: sdk::RequestPermissionRequest,
    ) -> sdk::Result<sdk::RequestPermissionResponse> {
        self.validate_session(&request.session_id)?;
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
    pub(super) fn session_notification(
        &self,
        notification: sdk::SessionNotification,
    ) -> sdk::Result<()> {
        self.validate_session(&notification.session_id)?;
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
    pub(super) async fn ext_method(
        &self,
        request: sdk::ExtRequest,
    ) -> sdk::Result<sdk::ExtResponse> {
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
    pub(super) fn ext_notification(&self, notification: &sdk::ExtNotification) -> sdk::Result<()> {
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
