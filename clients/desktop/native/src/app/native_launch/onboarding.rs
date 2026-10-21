//! Executing-host context, submit-once requests, and correlated setup receipts.
use super::*;
use std::path::Path;
use std::time::Duration;

impl Flight {
    fn matches(&self, connection: &Connection) -> bool {
        owned_by(&self.owner, &connection.client)
            && self.epoch == connection.connection_epoch()
            && connection.status == Status::Attached
    }
}

pub(super) fn destination(
    topology: &Topology,
    pane: Option<&PaneDescriptor>,
) -> Result<Destination, String> {
    let pane = pane.ok_or("Focus a terminal in the folder where the agent should work.")?;
    if !matches!(pane.terminal_id, ResourceId::Local { .. }) {
        return Err("Open a window on that machine before starting its native agent.".into());
    }
    let session = topology
        .sessions
        .iter()
        .find(|session| session.id == pane.session_id)
        .ok_or("The focused session is no longer on this host.")?;
    let key = session
        .key
        .ok_or("Waiting for this session's persistent identity. Refresh after connecting.")?;
    let cwd = pane.cwd.as_deref().filter(|cwd| Path::new(cwd).is_absolute())
        .ok_or("The focused terminal has no known absolute folder. Open a terminal in the intended checkout first.")?;
    Ok(Destination {
        session: key,
        session_id: session.id,
        name: session.name.clone(),
        cwd: cwd.to_owned(),
    })
}

impl Desk {
    pub(super) fn native_destination(&self) -> Result<Destination, String> {
        let connection = &self.link.as_ref().ok_or("Not connected.")?.connection;
        let topology = connection
            .topology
            .as_ref()
            .ok_or("Waiting for server inventory.")?;
        destination(topology, self.focused_pane())
    }

    pub(super) fn confirmed_native_destination(&self) -> Result<Destination, String> {
        let destination = self.native_destination()?;
        if self.native_launch.destination.as_ref() != Some(&destination) {
            return Err(
                "The focused folder or session changed. Refresh to confirm the new destination."
                    .into(),
            );
        }
        Ok(destination)
    }

    pub(super) fn queue_native_request(&self, request: NativeRequest) -> Result<Flight, String> {
        let connection = &self
            .link
            .as_ref()
            .ok_or("Reconnect before starting an agent.")?
            .connection;
        let payload = NativeRequestPayload::new(request)?;
        let (id, epoch) = connection.client.with_control(|control| {
            if control.status() != Status::Attached || !control.handshake_ready() {
                return Err("Reconnect before starting an agent.".to_owned());
            }
            if !control.server().is_some_and(|server| server.has_ext(ServerFeatureExt::NativeAgentLaunch)) {
                return Err("Update phux on this host to use native agent onboarding. No agent was started locally.".to_owned());
            }
            if control.options().attach_role.is_some_and(|role| role.is_viewer()) {
                return Err("This window is observing only. Open an interactive connection to start an agent.".to_owned());
            }
            let epoch = control.connection_epoch();
            Ok((control.send_command(Command::NativeAgent { request: payload }), epoch))
        })?;
        connection.client.wake();
        Ok(Flight {
            id,
            owner: Arc::downgrade(&connection.client),
            epoch,
        })
    }

    pub(super) fn queue_native_launch(&self, integration: &str) -> Result<Launch, String> {
        let destination = self.confirmed_native_destination()?;
        let flight = self.queue_native_request(NativeRequest::LaunchIntegration {
            integration: integration.to_owned(),
            session: destination.session,
            cwd: destination.cwd.clone(),
        })?;
        Ok(Launch {
            flight,
            integration: integration.to_owned(),
            destination,
        })
    }

    pub(super) fn login_opencode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native_launch.busy() {
            return;
        }
        match self.queue_opencode_login() {
            Ok(flight) => {
                self.native_launch.login = Some(flight);
                self.native_launch.error = None;
                self.close_modal(window, cx);
                self.arm_native_deadline(cx);
            }
            Err(error) => self.native_launch.error = Some(error),
        }
        cx.notify();
    }

    fn queue_opencode_login(&mut self) -> Result<Flight, String> {
        let destination = self.confirmed_native_destination()?;
        let command = self.native_launch.integrations.iter().find(|item| item.integration == "opencode" && item.native_available)
            .and_then(|item| item.auth_command.clone())
            .ok_or("This OpenCode profile has no documented sign-in command. Use its own authentication on this host.")?;
        let link = self.link.as_mut().ok_or("Not connected.")?;
        let epoch = link.connection.connection_epoch();
        let request = SpawnRequest {
            command: Some(command),
            cwd: Some(destination.cwd),
            session_id: Some(destination.session_id),
            ..SpawnRequest::default()
        };
        let id = link
            .workspace
            .launch_setup(&link.connection, request)
            .map_err(|error| error.body)?;
        link.connection.client.wake();
        Ok(Flight {
            id,
            owner: Arc::downgrade(&link.connection.client),
            epoch,
        })
    }

    pub(crate) fn native_launch_receipts(&mut self, events: &[Event], cx: &mut Context<Self>) {
        self.invalidate_native_requests(cx);
        for event in events {
            match event {
                Event::CommandResult { request_id, result } => {
                    self.native_command_receipt(*request_id, result, cx)
                }
                Event::TerminalSpawned {
                    request_id, error, ..
                } => self.native_login_receipt(*request_id, error.as_deref(), cx),
                _ => {}
            }
        }
        self.retire_closed_native_launch(events, cx);
    }

    fn retire_closed_native_launch(&mut self, events: &[Event], cx: &mut Context<Self>) {
        let Some(pending) = self.native_launch.pending.as_ref() else {
            return;
        };
        let closed = events.iter().any(|event| {
            matches!(event,
                Event::TerminalClosed { terminal_id, .. } if terminal_id == &pending.terminal
            )
        });
        let missing = self
            .link
            .as_ref()
            .is_some_and(|link| missing_after_reconnect(pending, &link.connection, events));
        if !closed && !missing {
            return;
        }
        let message = format!(
            "Native agent {:?} ended or is no longer listed after reconnect, before its conversation opened. Inspect the host's notifications for the provider setup error; nothing was relaunched.",
            pending.agent
        );
        self.native_launch.pending = None;
        self.native_launch.error = Some(message.clone());
        self.post(Kind::Error, "Native agent setup stopped", Some(message));
        cx.notify();
    }

    fn invalidate_native_requests(&mut self, cx: &mut Context<Self>) {
        let connection = self.link.as_ref().map(|link| &link.connection);
        let matches =
            |flight: &Flight| connection.is_some_and(|connection| flight.matches(connection));
        if self
            .native_launch
            .discovery
            .as_ref()
            .is_some_and(|flight| !matches(flight))
        {
            self.native_launch.discovery = None;
            self.native_launch.integrations.clear();
            self.native_launch.error =
                Some("Connection changed. Refresh agent discovery on the current host.".into());
        }
        let lost_launch = self
            .native_launch
            .launch
            .as_ref()
            .is_some_and(|launch| !matches(&launch.flight));
        let lost_login = self
            .native_launch
            .login
            .as_ref()
            .is_some_and(|flight| !matches(flight));
        if lost_launch || lost_login {
            self.native_launch.launch = None;
            self.native_launch.login = None;
            self.native_launch.error = Some(UNKNOWN.into());
            self.post(
                Kind::Error,
                "Agent setup outcome unknown",
                Some(UNKNOWN.into()),
            );
            cx.notify();
        }
    }

    fn native_command_receipt(&mut self, id: u32, result: &CommandResult, cx: &mut Context<Self>) {
        if self
            .native_launch
            .discovery
            .as_ref()
            .is_some_and(|flight| flight.id == id)
        {
            self.native_launch.discovery = None;
            match decode_reply::<NativeIntegrationCatalog>(result) {
                Ok(mut catalog) => {
                    self.native_launch.error = None;
                    catalog
                        .integrations
                        .sort_by_key(|item| item.integration != "opencode");
                    self.native_launch.integrations = catalog.integrations;
                }
                Err(error) => self.native_launch.error = Some(error),
            }
        } else if self
            .native_launch
            .launch
            .as_ref()
            .is_some_and(|launch| launch.flight.id == id)
        {
            let launch = self.native_launch.launch.take().expect("correlated above");
            match decode_reply::<SpawnedNativeAgent>(result).and_then(valid_launch) {
                Ok(spawned) => self.accept_native_launch(launch, spawned, cx),
                Err(error) => {
                    self.native_launch.error = Some(error.clone());
                    self.post(Kind::Error, "Native launch did not confirm", Some(error));
                }
            }
        }
    }

    fn accept_native_launch(
        &mut self,
        launch: Launch,
        spawned: SpawnedNativeAgent,
        cx: &mut Context<Self>,
    ) {
        self.native_launch.error = None;
        self.native_launch.pending = Some(Pending {
            owner: launch.flight.owner,
            epoch: launch.flight.epoch,
            session: launch.destination.name,
            session_key: Some(launch.destination.session),
            terminal: spawned.parent,
            agent: spawned.agent,
            activation: spawned.activation,
            revealed: false,
        });
        if let Some(link) = self.link.as_ref() {
            let _ = link.connection.refresh_topology();
        }
        self.post(Kind::Info, "Native agent created", Some(LAUNCHED.into()));
        self.arm_native_deadline(cx);
    }

    fn native_login_receipt(&mut self, id: u32, error: Option<&str>, cx: &mut Context<Self>) {
        if !self
            .native_launch
            .login
            .as_ref()
            .is_some_and(|flight| flight.id == id)
        {
            return;
        }
        self.native_launch.login = None;
        self.native_launch.error = None;
        match error {
            Some(error) => {
                self.native_launch.error = Some(error.to_owned());
                self.post(Kind::Error, "Could not open OpenCode sign-in", Some(error.to_owned()));
            }
            None => self.post(Kind::Info, "OpenCode sign-in opened on this host", Some("Complete OpenCode's own login, then choose New Agent → OpenCode. Opening the terminal does not confirm authentication.".into())),
        }
        cx.notify();
    }

    pub(super) fn arm_native_deadline(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
        let owner = Arc::downgrade(&client);
        let epoch = client.connection_epoch();
        let timer = cx.background_executor().timer(Duration::from_secs(30));
        self.native_launch._deadline = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |desk, cx| {
                let same = desk.client().is_some_and(|client| owned_by(&owner, &client) && client.connection_epoch() == epoch);
                if !same || !desk.native_launch.busy() { return; }
                let message = "Still waiting for native agent setup. Inspect this host's agent panes and notices; no launch or prompt has been retried. You can reconnect to refresh the connection.";
                desk.native_launch.error = Some(message.into());
                desk.post(Kind::Info, "Agent setup is still pending", Some(message.into()));
                cx.notify();
            });
        }));
    }
}

fn missing_after_reconnect(pending: &Pending, connection: &Connection, events: &[Event]) -> bool {
    if !owned_by(&pending.owner, &connection.client)
        || pending.epoch == connection.connection_epoch()
        || connection.status != Status::Attached
    {
        return false;
    }
    let snapshot = events.iter().rev().find_map(|event| match event {
        Event::TopologySnapshot {
            attach_id: None,
            snapshot,
        } => Some(snapshot),
        _ => None,
    });
    snapshot.is_some_and(|snapshot| {
        let topology = Topology::from_snapshot(snapshot);
        !topology
            .panes
            .iter()
            .any(|pane| pane.terminal_id == pending.terminal)
            || !topology.agent_sessions.iter().any(|agent| {
                agent.id == pending.agent && agent.parent.as_ref() == Some(&pending.terminal)
            })
    })
}

pub(super) fn decode_reply<T: serde::de::DeserializeOwned>(
    result: &CommandResult,
) -> Result<T, String> {
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => {
            if json.len() > phux_protocol::native_agent::MAX_NATIVE_PAYLOAD_BYTES {
                return Err("Agent setup receipt exceeds its size limit. Inspect the host's panes before retrying.".into());
            }
            serde_json::from_str(json).map_err(|_| {
                "Invalid agent setup receipt. Inspect the host's panes before retrying.".into()
            })
        }
        CommandResult::Error { message, .. } => Err(message.clone()),
        _ => Err(
            "The host did not return an agent setup receipt. Inspect its panes before retrying."
                .into(),
        ),
    }
}

pub(super) fn valid_launch(spawned: SpawnedNativeAgent) -> Result<SpawnedNativeAgent, String> {
    if spawned.activation.is_empty()
        || spawned.parent == spawned.agent
        || !matches!(spawned.parent, ResourceId::Local { id } if id != 0)
        || !matches!(spawned.agent, ResourceId::Local { id } if id != 0)
    {
        return Err(
            "Invalid native launch identity. Inspect the host's panes before retrying.".into(),
        );
    }
    Ok(spawned)
}
