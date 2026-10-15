use crate::{HostError, HostSpec, Sink, common};
use omp_rpc::{
    Client, ClientOptions, Event, ExtensionUiRequest, ExtensionUiResponse, RpcAgentEvent,
    RpcInbound, RpcNotification,
};
use parking_lot::Mutex;
use phux_client_core::session::agent_transcript::{ToolStatus, TranscriptRole};
use phux_protocol::native_agent::{
    NativeCapabilities, NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent,
    NativeOption, NativeQuestion, NativeQuestionItem, NativeState, NativeUpdate,
};
use serde_json::{Value, json};
use std::{process::Command, sync::Arc, thread, time::Duration};
use tokio::sync::{mpsc, watch};

pub fn run(
    spec: &HostSpec,
    sink: &Sink,
    mut commands: mpsc::Receiver<NativeCommand>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), HostError> {
    let mut process = Command::new(&spec.command[0]);
    process.args(&spec.command[1..]);
    common::configure(&mut process, spec);
    let child = common::WatchedChild::spawn(process, shutdown.clone())?;
    let stdout = child
        .0
        .lock()
        .stdout
        .take()
        .ok_or_else(|| HostError::Sdk("missing OMP stdout".into()))?;
    let stdin = child
        .0
        .lock()
        .stdin
        .take()
        .ok_or_else(|| HostError::Sdk("missing OMP stdin".into()))?;
    // Intentionally not Client::spawn: that API creates a detached process group.
    let (client, events) = Client::from_io(
        stdout,
        stdin,
        ClientOptions {
            ready_timeout: Duration::from_secs(30),
            ..ClientOptions::default()
        },
    )
    .map_err(|error| HostError::Sdk(error.to_string()))?;
    let state = client
        .call(&omp_rpc::GetStateCommand {})
        .map_err(|error| HostError::Sdk(error.to_string()))?;
    if spec
        .native_id
        .as_ref()
        .is_some_and(|expected| expected != &state.session_id)
    {
        return Err(HostError::Sdk(
            "OMP restored a different provider session".into(),
        ));
    }
    client
        .call(&omp_rpc::SetAskDialogCommand { enabled: true })
        .map_err(|error| HostError::Sdk(error.to_string()))?;
    let client = Arc::new(client);
    let mut view = sink.view();
    view.connected = true;
    view.provider_session_id = Some(state.session_id);
    view.capabilities = NativeCapabilities {
        prompt: true.into(),
        interrupt: true.into(),
        questions: true.into(),
        follow_up: true.into(),
    };
    sink.update(NativeUpdate::View { view })?;
    let dialog = Arc::new(Mutex::new(()));
    let event_sink = Sink::clone(sink);
    let provider = spec.provider.clone();
    let event_dialog = dialog.clone();
    let event_thread = thread::spawn(move || -> Result<(), HostError> {
        let mut rows = Rows::default();
        while let Ok(event) = events.recv() {
            project_event(&event_sink, &provider, &event_dialog, event, &mut rows)?;
        }
        Ok(())
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                command = commands.recv() => {
                    let Some(command) = command else { break; };
                    let client = Arc::clone(&client);
                    let sink = Sink::clone(sink);
                    let dialog = Arc::clone(&dialog);
                    tokio::task::spawn_blocking(move || {
                        let result = execute(&client, &sink, &dialog, command);
                        let _ = sink.result(result);
                    });
                }
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    if event_thread.is_finished() || child.0.lock().try_wait()?.is_some() { break; }
                }
            }
        }
        Ok::<(), HostError>(())
    });
    drop(child); // Terminate/reap the real peer, never the guardian's group.
    drop(client);
    runtime.shutdown_timeout(Duration::from_secs(5));
    let event_result = common::join_reader(event_thread, "OMP event projector")?;
    outcome.and(event_result)
}
fn project_event(
    sink: &Sink,
    provider: &str,
    dialog: &Mutex<()>,
    event: Event,
    rows: &mut Rows,
) -> Result<(), HostError> {
    match event {
        Event::Notification(RpcNotification::RpcAgentEvent(event)) => {
            project(sink, provider, event, rows)?;
        }
        Event::Notification(RpcNotification::ExtensionUiRequest(request)) => {
            let _dialog = dialog.lock();
            question(sink, request)?;
        }
        Event::Notification(RpcNotification::SessionSettled(_)) => {
            settle(sink)?;
        }
        Event::Notification(RpcNotification::PromptResult(result)) => {
            if let Some(error) = result.error {
                sink.notice(&error.message)?;
            }
            if result.session_settled {
                settle(sink)?;
            }
        }
        Event::Notification(RpcNotification::CommandOutput(output)) => {
            sink.notice(&output.text)?;
        }
        Event::Notification(RpcNotification::SessionInfoUpdate(info)) => {
            if sink.view().provider_session_id.as_ref() != Some(&info.session_id) {
                return Err(HostError::Sdk(
                    "OMP changed provider session identity; new activation required".into(),
                ));
            }
        }
        Event::Notification(RpcNotification::ExtensionError(error)) => {
            sink.notice(&error.error)?;
        }
        Event::Notification(RpcNotification::RpcFrameError(_)) => {
            return Err(HostError::Sdk(
                "OMP reported dropped oversized event".into(),
            ));
        }
        Event::Undecodable { error, .. } => {
            return Err(HostError::Sdk(format!(
                "OMP event failed official SDK decode: {error}"
            )));
        }
        _ => {}
    }
    Ok(())
}

fn execute(
    client: &Client,
    sink: &Sink,
    dialog: &Mutex<()>,
    command: NativeCommand,
) -> NativeCommandResult {
    let id = command.operation_id;
    match command.intent {
        NativeIntent::Prompt { text } => prompt(client, sink, id, text),
        NativeIntent::Interrupt { turn_id } => {
            if sink.view().turn_id.as_ref() == Some(&turn_id) {
                match client.call(&omp_rpc::AbortCommand {}) {
                    Ok(()) => common::result(
                        id,
                        NativeDisposition::Completed,
                        "OMP typed abort acknowledged; session remains open",
                    ),
                    Err(error) => sdk_error(id, &error),
                }
            } else {
                common::result(id, NativeDisposition::Refused, "turn already ended")
            }
        }
        NativeIntent::Answer { question_id, response } => {
            let _dialog = dialog.lock();
            match answer(sink, &question_id, &response) {
                Err(error) => common::result(id, NativeDisposition::Refused, &error.to_string()),
                Ok(answer) => match client.send(&RpcInbound::ExtensionUiResponse(answer)) {
                    Ok(()) => common::result(
                        id,
                        NativeDisposition::Queued,
                        "queued to OMP SDK writer; provider consumption is not acknowledged",
                    ),
                    Err(error) => common::result(id, NativeDisposition::Unknown, &error.to_string()),
                },
            }
        }
    }
}

fn prompt(client: &Client, sink: &Sink, id: u64, text: String) -> NativeCommandResult {
    let follow_up = match sink.begin_prompt(format!("omp:{id}")) {
        Ok(follow_up) => follow_up,
        Err(error) => {
            return common::result(id, NativeDisposition::Refused, &error.to_string());
        }
    };
    let command = omp_rpc::PromptCommand {
        message: text,
        images: None,
        streaming_behavior: follow_up.then_some(omp_rpc::StreamingBehavior::FollowUp),
    };
    match client.call(&command) {
        Ok(_) => common::result(
            id,
            NativeDisposition::Admitted,
            if follow_up {
                "OMP acknowledged provider follow-up queue admission; turn outcome arrives separately"
            } else {
                "OMP typed prompt acknowledged; turn outcome arrives separately"
            },
        ),
        Err(error) => {
            if !follow_up && matches!(error, omp_rpc::Error::Command { .. }) {
                let _ = sink.reject_pending_turn(&format!("omp:{id}"));
            }
            sdk_error(id, &error)
        }
    }
}

fn sdk_error(id: u64, error: &omp_rpc::Error) -> NativeCommandResult {
    let disposition = if matches!(error, omp_rpc::Error::Command { .. }) {
        NativeDisposition::Refused
    } else {
        NativeDisposition::Unknown
    };
    common::result(id, disposition, &error.to_string())
}
fn settle(sink: &Sink) -> Result<(), HostError> {
    for question in sink.view().questions {
        sink.update(NativeUpdate::QuestionClosed {
            id: question.id,
            reason: "provider session settled".into(),
        })?;
    }
    sink.state(NativeState::Done)
}
#[derive(Default)]
struct Rows {
    serial: u64,
    run_serial: u64,
    current: String,
    tools: std::collections::BTreeMap<String, (String, ToolStatus)>,
}
impl Rows {
    fn tool_summary(
        &mut self,
        id: &str,
        input: Option<&Value>,
        status: ToolStatus,
    ) -> Result<String, HostError> {
        if !self.tools.contains_key(id) && self.tools.len() >= 128 {
            let retired = self
                .tools
                .iter()
                .find_map(|(id, (_, status))| (*status != ToolStatus::Running).then(|| id.clone()));
            if let Some(retired) = retired {
                self.tools.remove(&retired);
            } else {
                return Err(HostError::Sdk("too many active OMP tools".into()));
            }
        }
        let summary = self
            .tools
            .entry(id.to_owned())
            .or_insert_with(|| (String::new(), status));
        if let Some(input) = input {
            summary.0 = common::summarize_args(input);
        }
        summary.1 = status;
        Ok(summary.0.clone())
    }
}
fn project(
    sink: &Sink,
    provider: &str,
    event: RpcAgentEvent,
    rows: &mut Rows,
) -> Result<(), HostError> {
    match event {
        RpcAgentEvent::AgentStart(_) => {
            rows.run_serial += 1;
            sink.replace_turn(format!("omp-run:{}", rows.run_serial))?;
            sink.state(NativeState::Working)
        }
        RpcAgentEvent::TurnStart(_) => sink.state(NativeState::Working),
        RpcAgentEvent::AgentEnd(event) if event.is_terminal == Some(true) => settle(sink),
        RpcAgentEvent::MessageStart(event) => {
            rows.serial += 1;
            rows.current = event
                .message_id
                .clone()
                .unwrap_or_else(|| format!("omp-message:{}", rows.serial));
            message(sink, provider, &rows.current, &event.message, false)
        }
        RpcAgentEvent::MessageUpdate(event) => message(
            sink,
            provider,
            event.message_id.as_deref().unwrap_or(&rows.current),
            &event.message,
            false,
        ),
        RpcAgentEvent::MessageEnd(event) => message(
            sink,
            provider,
            event.message_id.as_deref().unwrap_or(&rows.current),
            &event.message,
            true,
        ),
        RpcAgentEvent::ToolExecutionStart(event) => {
            let summary = rows.tool_summary(
                &event.tool_call_id,
                event.args.as_ref(),
                ToolStatus::Running,
            )?;
            common::tool(
                sink,
                provider,
                event.tool_call_id,
                event.tool_name,
                summary,
                String::new(),
                ToolStatus::Running,
            )
        }
        RpcAgentEvent::ToolExecutionUpdate(event) => {
            let summary = rows.tool_summary(
                &event.tool_call_id,
                event.args.as_ref(),
                ToolStatus::Running,
            )?;
            common::tool(
                sink,
                provider,
                event.tool_call_id,
                event.tool_name,
                summary,
                event.partial_result.map_or_else(String::new, |value| value.to_string()),
                ToolStatus::Running,
            )
        }
        RpcAgentEvent::ToolExecutionEnd(event) => {
            let status = if event.is_error == Some(true) {
                ToolStatus::Error
            } else {
                ToolStatus::Ok
            };
            let summary = rows.tool_summary(&event.tool_call_id, None, status)?;
            common::tool(
                sink,
                provider,
                event.tool_call_id,
                event.tool_name,
                summary,
                event.result.map_or_else(String::new, |value| value.to_string()),
                status,
            )
        }
        RpcAgentEvent::Notice(notice) => sink.notice(&notice.message),
        _ => Ok(()),
    }
}
fn message(
    sink: &Sink,
    provider: &str,
    id: &str,
    message: &omp_rpc::AgentMessage,
    final_row: bool,
) -> Result<(), HostError> {
    let value = serde_json::to_value(message).map_err(|error| HostError::Sdk(error.to_string()))?;
    let role = match value["role"].as_str() {
        Some("user") => TranscriptRole::User,
        Some("assistant") => TranscriptRole::Assistant,
        _ => return Ok(()),
    };
    let content = &value["content"];
    if let Some(text) = content.as_str() {
        return common::text(sink, provider, id.into(), role, text.into(), final_row);
    }
    let Some(blocks) = content.as_array() else {
        return Ok(());
    };
    let mut text = String::new();
    let mut thinking = String::new();
    for block in blocks {
        if let Some(part) = block["text"].as_str()
            && text.len() < 12 * 1024 + 4
        {
            text.push_str(&common::head(part, 12 * 1024 + 4 - text.len()));
        }
        if let Some(part) = block["thinking"].as_str()
            && thinking.len() < 12 * 1024 + 4
        {
            thinking.push_str(&common::head(part, 12 * 1024 + 4 - thinking.len()));
        }
    }
    if !thinking.is_empty() {
        common::text(
            sink,
            provider,
            format!("{id}:thinking"),
            TranscriptRole::Thinking,
            thinking,
            final_row,
        )?;
    }
    if !text.is_empty() {
        common::text(sink, provider, id.into(), role, text, final_row)?;
    }
    Ok(())
}
fn question(sink: &Sink, request: ExtensionUiRequest) -> Result<(), HostError> {
    let fields =
        serde_json::to_value(&request).map_err(|error| HostError::Sdk(error.to_string()))?;
    let mut question = NativeQuestion {
        id: String::new(),
        kind: String::new(),
        title: String::new(),
        message: String::new(),
        options: Vec::new(),
        questions: Vec::new(),
        fields,
    };
    match request {
        ExtensionUiRequest::Confirm(request) => { question.id = request.id; question.kind = "confirm".into(); question.title = request.title; question.message = request.message; }
        ExtensionUiRequest::Select(request) => {
            question.id = request.id; question.kind = "select".into(); question.title = request.title;
            question.options = request.options.into_iter().enumerate().map(|(index, label)| NativeOption { id: label.clone(), label, description: request.option_details.as_ref().and_then(|details| details.get(index)).and_then(|detail| detail.description.clone()) }).collect();
        }
        ExtensionUiRequest::Input(request) => { question.id = request.id; question.kind = "input".into(); question.title = request.title; }
        ExtensionUiRequest::Editor(request) => { question.id = request.id; question.kind = "editor".into(); question.title = request.title; question.fields["initial_value"] = json!(request.prefill.unwrap_or_default()); }
        ExtensionUiRequest::Ask(request) => {
            question.id = request.id; question.kind = "ask".into();
            question.questions = request.questions.into_iter().map(|item| NativeQuestionItem { recommended: item.recommended.and_then(|index| usize::try_from(index).ok()).and_then(|index| item.options.get(index)).map(|option| option.label.clone()), id: item.id, question: item.question, header: item.header.unwrap_or_default(), multi: item.multi, options: item.options.into_iter().map(|option| NativeOption { id: option.label.clone(), label: option.label, description: option.description }).collect() }).collect();
        }
        ExtensionUiRequest::Cancel(request) => return sink.update(NativeUpdate::QuestionClosed { id: request.target_id, reason: "provider cancelled dialog".into() }),
        ExtensionUiRequest::Notify(request) => return sink.notice(&request.message),
        ExtensionUiRequest::OpenUrl(_) => return sink.notice("Provider requested opening an external URL; automatic browser/auth actions unsupported"),
        _ => return Ok(()), // Cosmetic status/title/widget updates are not lifecycle evidence.
    }
    sink.update(NativeUpdate::QuestionOpened { question })?;
    sink.state(NativeState::Blocked)
}
fn answer(sink: &Sink, id: &str, response: &Value) -> Result<ExtensionUiResponse, HostError> {
    let view = sink.view();
    let question = view
        .questions
        .iter()
        .find(|question| question.id == id)
        .ok_or_else(|| HostError::Invalid("OMP dialog cancelled before dispatch".into()))?;
    common::validate_answer(question, response).map_err(HostError::Invalid)?;
    let mut wire = json!({"id": id});
    if response["cancelled"].as_bool() == Some(true) {
        wire["cancelled"] = json!(true);
    }
    // Never timedOut: ask timeout may select recommendations.
    else {
        match question.kind.as_str() {
            "confirm" => wire["confirmed"] = response["confirmed"].clone(),
            "select" => wire["value"] = response["option_id"].clone(),
            "input" | "editor" => wire["value"] = response["text"].clone(),
            "ask" => {
                let answers = response
                    .get("answers")
                    .and_then(Value::as_array)
                    .ok_or_else(|| HostError::Invalid("OMP form answers required".into()))?;
                wire["answers"] = Value::Array(
                    question
                        .questions
                        .iter()
                        .map(|item| {
                            let answer = answers
                                .iter()
                                .find(|answer| answer["id"].as_str() == Some(&item.id))
                                .ok_or_else(|| {
                                    HostError::Invalid(format!(
                                        "OMP form answer missing for question {}",
                                        item.id
                                    ))
                                })?;
                            let mut result =
                                json!({"id":item.id,"selectedOptions":answer["selected_options"]});
                            if let Some(input) = answer.get("custom_input") {
                                result["customInput"] = input.clone();
                            }
                            Ok(result)
                        })
                        .collect::<Result<Vec<_>, HostError>>()?,
                );
            }
            _ => return Err(HostError::Invalid("unsupported OMP question kind".into())),
        }
    }
    ExtensionUiResponse::from_value(wire).map_err(|error| HostError::Sdk(error.to_string()))
}
