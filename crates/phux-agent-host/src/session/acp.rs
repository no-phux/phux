//! Official ACP v1 connection, with optional surfaces explicitly unadvertised.
//! Cursor blocking extensions follow <https://cursor.com/docs/cli/acp>, not text
//! inference. Filesystem/terminal/auth callbacks are never implicitly approved.
use super::{HostError, HostSpec, Sink, common};
use agent_client_protocol::{
    self as acp,
    schema::{ProtocolVersion, v1 as sdk},
};
use parking_lot::Mutex;
use phux_client_core::session::agent_transcript::{ToolStatus, TranscriptRole};
use phux_protocol::native_agent::{
    NativeCapabilities, NativeCommand, NativeDisposition, NativeIntent, NativeState, NativeUpdate,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

mod callbacks;
use callbacks::host_to_acp;

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
    fn new_message(&mut self, role: TranscriptRole) -> (String, TranscriptRole, String) {
        self.serial += 1;
        let id = if role == TranscriptRole::User {
            self.prompt_user_id
                .clone()
                .unwrap_or_else(|| format!("acp-message:{}", self.serial))
        } else {
            format!("acp-message:{}", self.serial)
        };
        (id, role, String::new())
    }

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
    // SDK-owned tasks can await explicit answers while notifications keep flowing.
    waiting: Mutex<BTreeMap<String, oneshot::Sender<Value>>>,
    rows: Mutex<Rows>,
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
    runtime.block_on(run_local(spec, sink, commands, shutdown))
}

fn sdk_error(error: &sdk::Error) -> HostError {
    HostError::Sdk(error.to_string())
}

type Connection = acp::ConnectionTo<acp::Agent>;

fn client_builder(
    callback: Arc<Callback>,
) -> acp::Builder<acp::Client, impl acp::HandleDispatchFrom<acp::Agent>> {
    let requests = callback.clone();
    acp::Client
        .builder()
        .on_receive_request(
            async move |request: sdk::AgentRequest, responder, connection| {
                let callback = requests.clone();
                // Blocking questions must not hold the SDK dispatch loop. These
                // tasks are owned/cancelled by the connection, including at EOF.
                connection.spawn(async move {
                    responder
                        .cast::<sdk::ClientResponse>()
                        .respond_with_result(callback.request(request).await)
                })
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: sdk::AgentNotification, _connection| match notification {
                sdk::AgentNotification::SessionNotification(notification) => {
                    callback.session_notification(notification)
                }
                sdk::AgentNotification::ExtNotification(notification) => {
                    callback.ext_notification(&notification)
                }
                _ => Ok(()),
            },
            acp::on_receive_notification!(),
        )
}

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
    let callback = Arc::new(Callback {
        sink: sink.clone(),
        provider: spec.provider.clone(),
        waiting: Mutex::new(BTreeMap::new()),
        rows: Mutex::new(Rows::default()),
    });
    let mut stopped = shutdown.clone();
    let driver = client_builder(callback.clone()).connect_with(
        acp::ByteStreams::new(outgoing, incoming),
        async |connection| {
            serve_connection(&spec, connection, &callback, &mut commands, &mut shutdown)
                .await
                .map_err(|error| host_to_acp(&error))
        },
    );
    // A peer that stops reading must not hold shutdown hostage in the SDK's
    // successful-foreground output drain. Drop all connection-owned work first.
    let outcome = tokio::select! {
        result = driver => result.map_err(|error| sdk_error(&error)),
        _ = stopped.changed() => Ok(()),
    };
    callback.cancel_questions();
    let _ = child.kill().await;
    let _ = child.wait().await;
    outcome
}

async fn serve_connection(
    spec: &HostSpec,
    connection: Connection,
    callback: &Arc<Callback>,
    commands: &mut mpsc::Receiver<NativeCommand>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), HostError> {
    let Some(id) = startup(spec, &connection, callback, commands, shutdown).await? else {
        return Ok(());
    };
    Session {
        connection,
        callback: callback.clone(),
        id,
    }
    .serve(commands, shutdown)
    .await
}

async fn initialize(spec: &HostSpec, connection: &Connection) -> Result<sdk::SessionId, HostError> {
    // Default capabilities explicitly exclude client filesystem and terminal execution.
    // The request handler rejects them even if a peer ignores negotiation.
    let initialized = connection
        .send_request(
            sdk::InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(sdk::ClientCapabilities::default())
                .client_info(sdk::Implementation::new(
                    "phux-agent-host",
                    env!("CARGO_PKG_VERSION"),
                )),
        )
        .block_task()
        .await
        .map_err(|error| sdk_error(&error))?;
    if initialized.protocol_version != ProtocolVersion::V1 {
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
            .send_request(sdk::LoadSessionRequest::new(id.clone(), spec.cwd.clone()))
            .block_task()
            .await
            .map_err(|error| sdk_error(&error))?;
        Ok(id)
    } else {
        connection
            .send_request(sdk::NewSessionRequest::new(spec.cwd.clone()))
            .block_task()
            .await
            .map(|session| session.session_id)
            .map_err(|error| HostError::Sdk(format!("ACP session initialization failed (authenticate externally if required): {error}")))
    }
}

async fn startup(
    spec: &HostSpec,
    connection: &Connection,
    callback: &Callback,
    commands: &mut mpsc::Receiver<NativeCommand>,
    shutdown: &mut watch::Receiver<bool>,
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
            () = connection.incoming_closed() => return Err(transport_closed()),
            _ = shutdown.changed() => return Ok(None),
            command = commands.recv() => {
                let Some(command) = command else { return Ok(None); };
                callback.sink.result(callback.startup_command(command))?;
            }
        }
    }
}

fn transport_closed() -> HostError {
    HostError::Sdk("ACP transport closed".into())
}

struct Session {
    connection: Connection,
    callback: Arc<Callback>,
    id: sdk::SessionId,
}
impl Session {
    async fn serve(
        &self,
        commands: &mut mpsc::Receiver<NativeCommand>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), HostError> {
        loop {
            tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                () = self.connection.incoming_closed() => return Err(transport_closed()),
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    self.dispatch(command)?;
                }
            }
        }
    }
    fn dispatch(&self, command: NativeCommand) -> Result<(), HostError> {
        let id = command.operation_id;
        match command.intent {
            NativeIntent::Prompt { text } => self.prompt(id, text),
            NativeIntent::Interrupt { turn_id } => self.interrupt(id, &turn_id),
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
        self.connection
            .spawn(async move {
                let result = connection
                    .send_request(sdk::PromptRequest::new(session_id, vec![text.into()]))
                    .block_task()
                    .await;
                let _ = callback.finish_prompt(id);
                let receipt = match result {
                    Ok(response) => common::result(
                        id,
                        NativeDisposition::Completed,
                        &format!("ACP stop reason: {:?}", response.stop_reason),
                    ),
                    Err(error) => {
                        common::result(id, NativeDisposition::Unknown, &error.to_string())
                    }
                };
                let _ = callback.sink.result(receipt);
                Ok(())
            })
            .map_err(|error| sdk_error(&error))
    }
    fn interrupt(&self, id: u64, turn_id: &str) -> Result<(), HostError> {
        let sink = &self.callback.sink;
        if sink.view().turn_id.as_deref() != Some(turn_id) {
            return sink.result(common::result(
                id,
                NativeDisposition::Refused,
                "ACP turn already ended",
            ));
        }
        self.callback.cancel_questions();
        let result = self
            .connection
            .send_notification(sdk::CancelNotification::new(self.id.clone()));
        let receipt = match result {
            Ok(()) => common::result(
                id,
                NativeDisposition::Queued,
                "ACP cancel notification queued; awaiting actual stop reason",
            ),
            Err(error) => common::result(id, NativeDisposition::Unknown, &error.to_string()),
        };
        sink.result(receipt)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_tests;
