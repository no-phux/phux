//! Choosing what is on screen across sessions: show a terminal, make a
//! terminal or session, step through them.

use phux_client_runtime::control::{
    ControlOptions, PredictionStyle, SessionDescriptor, SessionRecovery, SpawnRequest, Status,
    Topology,
};
use phux_client_runtime::{Client, ClientOptions, Runtime, Target};
use phux_protocol::ids::{ResourceId, SessionKey};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::RolePolicy;
use rune::Tx;

use super::Desk;

impl Desk {
    pub(super) fn wait_for_session(&mut self, name: String) {
        self.connection.view_choice += 1;
        self.connection.pending_session_origin = self.connection.callback_fence();
        self.connection.pending_session = Some(name);
        self.connection.wanted = None;
    }

    pub fn show(&mut self, terminal: ResourceId, session_name: &str) {
        // Choosing a pane is where the user is going: a page a child was
        // opened from does not come back over it.
        if let Some(phone) = &mut self.phone {
            phone.return_to = None;
        }
        self.link.creating = None;
        self.link.created = None;
        self.link.uncertain_session = None;
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        self.drop_dead_views(&client);
        self.connection.pending_session = None;
        self.connection.pending_session_origin = None;
        self.want(terminal.clone());
        if terminal.host().is_none()
            && !session_name.is_empty()
            && let Some(intent) = self.connection.view_intents.get(&terminal)
        {
            intent.select_terminal_session(&client, &terminal);
        }
        self.open_wanted(&client);
    }

    /// The selected session's terminals a server restart ended, which
    /// `phux restore` would relaunch: the keys to relaunch them by.
    pub fn ended_by_restart(&self) -> Vec<phux_protocol::ids::ResourceKey> {
        use phux_protocol::ResourceKind;
        let Some(client) = &self.connection.client else {
            return Vec::new();
        };
        let Some(key) = self.selected_entry(client).and_then(|s| s.key) else {
            return Vec::new();
        };
        client
            .organization()
            .members(&key)
            .filter(|r| r.kind == ResourceKind::Terminal && r.live.is_none())
            .filter(|r| r.ended_by_restart())
            .map(|r| r.key)
            .collect()
    }

    /// Relaunch what a server restart ended in the selected session.
    pub fn restore_session(&mut self) {
        let Some(client) = &self.connection.client else {
            return;
        };
        for key in self.ended_by_restart() {
            if let Err(error) = client.relaunch_resource(key) {
                self.connection.error = Some(format!("Couldn't restore a terminal: {error}"));
            }
        }
    }

    /// The topology entry of the session `client` has selected.
    pub(super) fn selected_entry(&self, client: &Client) -> Option<&SessionDescriptor> {
        self.connection
            .topology
            .as_ref()?
            .session(client.selected_session()?)
    }

    /// The selected session's key, once the server has keyed it.
    pub(super) fn selected_key(&self) -> Option<SessionKey> {
        self.selected_entry(self.connection.client.as_ref()?)?.key
    }

    pub fn selected_session_name(&self) -> Option<String> {
        let client = self.connection.client.as_ref()?;
        Some(self.selected_entry(client)?.name.clone())
    }

    pub fn new_terminal(&mut self) {
        self.new_terminal_at(self.focused_cwd());
    }

    pub(super) fn new_terminal_at(&mut self, cwd: Option<String>) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let session_id = client.selected_session();
        let Some(request) = self.spawn_request(
            &client,
            SpawnRequest {
                command: None,
                cwd,
                env: None,
                session_id,
                initial_size: self.tab_spawn_size(),
            },
        ) else {
            return;
        };
        self.connection.pending_spawns.push(request);
    }

    /// Platform-local pending-selection projection of canonical acceptance.
    pub(super) fn spawn_request(&mut self, client: &Client, request: SpawnRequest) -> Option<u32> {
        let server = self.connection.view_server.as_deref();
        let submission = client.with_control(|control| {
            (control.connection_epoch() == self.connection.view_epoch
                && control.server().map(|info| info.id.as_slice()) == server)
                .then(|| control.submit_spawn(request))
        });
        let Some(submission) = submission else {
            self.connection.error =
                Some("Terminal creation cancelled: its observed server changed.".to_owned());
            return None;
        };
        self.accept_spawn(submission)
    }

    pub(super) fn accept_spawn(
        &mut self,
        submission: phux_client_runtime::control::WorkflowSubmission,
    ) -> Option<u32> {
        use phux_client_runtime::control::WorkflowSubmission;
        match submission {
            WorkflowSubmission::Queued { request_id }
            | WorkflowSubmission::Journaled {
                request_id: Some(request_id),
                ..
            } => {
                self.connection.view_choice += 1;
                if let Some(client) = self.connection.client.as_ref() {
                    self.connection.spawn_servers.insert(
                        request_id,
                        super::connection::SpawnOrigin::new(
                            client,
                            self.connection.view_server.clone(),
                            self.connection.view_epoch,
                            self.connection.view_choice,
                        ),
                    );
                }
                Some(request_id)
            }
            WorkflowSubmission::NotQueued { reason } => {
                self.connection.error = Some(format!("Terminal creation was not queued: {reason}"));
                None
            }
            WorkflowSubmission::Journaled {
                request_id: None, ..
            } => unreachable!("spawn retains a request id"),
        }
    }

    /// Where the focused terminal is, for what starts beside it. A pane
    /// that hasn't said yet (the split made a moment ago) defers to the
    /// panes around it.
    pub(super) fn focused_cwd(&self) -> Option<String> {
        let shown = self.connection.shown.as_ref()?;
        let topology = self.connection.topology.as_ref()?;
        let cwd = |id: &ResourceId| {
            let cwd = topology.pane(id)?.cwd.clone();
            cwd.filter(|cwd| !cwd.is_empty())
        };
        cwd(&shown.resource).or_else(|| self.on_screen().iter().find_map(cwd))
    }

    pub fn new_session(&mut self) {
        let taken: Vec<String> = self
            .connection
            .topology
            .iter()
            .flat_map(|t| t.sessions.iter().map(|s| s.name.clone()))
            .collect();
        let name = loop {
            self.connection.sessions_seq += 1;
            let name = format!("rune-{}", self.connection.sessions_seq);
            if !taken.contains(&name) {
                break name;
            }
        };
        self.create_session(name);
    }

    /// Create a new durable session; a name collision is a host refusal.
    pub(super) fn create_session(&mut self, name: String) {
        self.create_persistent_session_running(name, None);
    }

    /// As [`Self::create_session`], its seed pane running `command` rather
    /// than the shell, and ephemeral: it ends with its last terminal.
    pub(super) fn create_session_running(&mut self, name: String, command: Option<Vec<String>>) {
        self.create_session_with_lifetime(
            name,
            command,
            phux_protocol::wire::frame::SessionLifetime::Ephemeral,
        );
    }

    pub(super) fn create_persistent_session_running(
        &mut self,
        name: String,
        command: Option<Vec<String>>,
    ) {
        self.create_session_with_lifetime(
            name,
            command,
            phux_protocol::wire::frame::SessionLifetime::Persistent,
        );
    }

    fn create_session_with_lifetime(
        &mut self,
        name: String,
        command: Option<Vec<String>>,
        lifetime: phux_protocol::wire::frame::SessionLifetime,
    ) {
        let cwd = self.focused_cwd();
        let Some(client) = &self.connection.client else {
            return;
        };
        let launch =
            phux_protocol::wire::frame::LaunchSpec::new(command.unwrap_or_default()).with_cwd(cwd);
        let submission = client.create_session(name.clone(), lifetime, Some(Box::new(launch)));
        self.select_submitted_session(name, submission);
    }

    /// The host resolves existence; the replica never decides whether to create.
    pub(super) fn ensure_session(&mut self, name: String) {
        let launch =
            phux_protocol::wire::frame::LaunchSpec::new(Vec::new()).with_cwd(self.focused_cwd());
        let Some(client) = &self.connection.client else {
            return;
        };
        let submission = client.ensure_session(
            name.clone(),
            phux_protocol::wire::frame::SessionLifetime::Persistent,
            Some(Box::new(launch)),
        );
        self.select_submitted_session(name, submission);
    }

    fn select_submitted_session(
        &mut self,
        name: String,
        submission: phux_client_runtime::control::WorkflowSubmission,
    ) {
        match submission {
            phux_client_runtime::control::WorkflowSubmission::Journaled { operation, .. } => {
                self.link.creating = Some(operation);
                self.link.created = None;
                self.link.uncertain_session = None;
            }
            phux_client_runtime::control::WorkflowSubmission::NotQueued { reason } => {
                self.connection.error = Some(format!("Couldn't create session: {reason}"));
                return;
            }
            phux_client_runtime::control::WorkflowSubmission::Queued { .. } => {
                unreachable!("session creates are journaled")
            }
        }
        // Show its seed pane once the topology lists it as selected; the
        // current terminal stays until then.
        self.wait_for_session(name);
    }

    /// Reconcile only the operation this window submitted. A lost receipt stays
    /// unknown; it never schedules another create or attaches a colliding name.
    pub(super) fn session_creation_outcome(
        &mut self,
        client: &Client,
        operation: phux_protocol::ids::OperationId,
        outcome: &phux_client_core::org_ops::OrgOutcome,
    ) -> bool {
        if self.link.creating != Some(operation) {
            return false;
        }
        self.link.creating = None;
        match outcome {
            phux_client_core::org_ops::OrgOutcome::Committed(receipt) => {
                self.link.created = receipt.session;
                if let Some(bare) = &mut self.bare
                    && self.connection.pending_session.as_ref() == Some(&bare.session)
                {
                    bare.key = receipt.session;
                }
                if self.link.created.is_none() {
                    self.link.uncertain_session = Some(operation);
                    self.connection.pending_session = None;
                    self.connection.error =
                        Some("Create committed without a session receipt".into());
                }
                let _ = client.refresh_topology();
            }
            other => {
                if matches!(other, phux_client_core::org_ops::OrgOutcome::Unknown { .. }) {
                    self.link.uncertain_session = Some(operation);
                }
                if self.bare.as_ref().is_some_and(|bare| {
                    self.connection.pending_session.as_ref() == Some(&bare.session)
                }) {
                    self.bare = None;
                }
                self.link.created = None;
                self.connection.pending_session = None;
                self.connection.error = Some(format!(
                    "Session operation {operation:?} did not settle: {other:?}"
                ));
            }
        }
        true
    }

    /// The focus a terminal hears about (DEC 1004) is also what admits its
    /// OSC 52 writes, so it follows the keyboard and is said again on a new
    /// connection. True when this should be tried again shortly: an attached
    /// terminal can come ready without news, while a connection that is down
    /// wakes the window itself when it returns.
    pub(super) fn sync_focus(&mut self, client: &Client) -> bool {
        let now = self
            .keyboard()
            .filter(|(_, view)| self.focused && view.is_some());
        let now = now.map(|(terminal, _)| terminal.clone());
        if self.connection.focus_told == now {
            return false;
        }
        if let Some(old) = self.connection.focus_told.take() {
            let _ = client.send_focus(&old, FocusEvent::Lost);
        }
        let Some(new) = now else { return false };
        if client.send_focus(&new, FocusEvent::Gained) {
            self.connection.focus_told = Some(new);
            self.reclaim_fits();
            return false;
        }
        self.connection.status == Status::Attached
    }

    /// Step through everything this window can show, every session's.
    pub fn cycle(&mut self, step: isize) {
        let all = self.addressable();
        if all.is_empty() {
            return;
        }
        let current = self
            .connection
            .shown
            .as_ref()
            .and_then(|s| all.iter().position(|r| r.id == s.resource))
            .unwrap_or(0);
        let next = (current as isize + step).rem_euclid(all.len() as isize) as usize;
        let next = all[next].clone();
        self.show(next.id, &next.session);
    }

    /// Step through the shown session's terminals, tab by tab in pane order.
    pub fn cycle_in_session(&mut self, step: isize) {
        let all: Vec<ResourceId> = self
            .connection
            .tabs
            .list
            .iter()
            .flat_map(|t| t.resources.clone())
            .collect();
        let Some(session) = self.selected_session_name().filter(|_| !all.is_empty()) else {
            return;
        };
        let current = self
            .connection
            .shown
            .as_ref()
            .and_then(|s| all.iter().position(|r| *r == s.resource))
            .unwrap_or(0);
        let next = all[stepped(current, step, all.len())].clone();
        self.connection.zoomed = None;
        self.show(next, &session);
    }

    /// Step through the sessions in the sidebar's order.
    pub fn step_session(&mut self, step: isize) {
        let names: Vec<String> = self
            .ordered_sessions()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        if names.is_empty() {
            return;
        }
        let current = self
            .selected_session_name()
            .and_then(|name| names.iter().position(|n| *n == name))
            .unwrap_or(0);
        self.show_session(&names[stepped(current, step, names.len())]);
    }

    /// The window title: the focused resource's label and its session, set
    /// only when it changes.
    pub fn window_title(&self) -> &str {
        &self.window_title
    }

    pub(super) fn title(&mut self, tx: &mut Tx) {
        let title = self
            .connection
            .shown
            .as_ref()
            .and_then(|s| self.addressable_of(&s.resource))
            .map_or_else(
                || "phux".to_owned(),
                |r| format!("{} — {}", r.label, r.session),
            );
        if self.window_title != title {
            tx.set_title(title.clone());
            self.window_title = title;
        }
    }

    /// Offer the pane in front to the person's phone. Only the window in
    /// front speaks: another window's poll must not withdraw its offer.
    pub(super) fn handoff(&self) {
        if !self.focused {
            return;
        }
        let machine = self.machines.current.as_deref();
        crate::handoff::offer(
            self.connection
                .shown
                .as_ref()
                .map(|shown| crate::handoff::payload(machine, &shown.resource)),
        );
    }

    pub fn focus_changed(&mut self, focused: bool) {
        self.focused = focused;
        self.chief.disarm();
        if !focused {
            crate::handoff::offer(None);
        }
        // A phone app out of the foreground is off screen; a desktop window
        // that lost focus still shows its toasts.
        if self.native.is_some() {
            self.toasts_shown(focused);
        }
        // Back at the window: what's on screen has now been seen, and a
        // laptop's socket may have died while it was away.
        if focused {
            for id in self.on_screen() {
                self.activities.seen(&id);
            }
            self.hurry_link();
            self.recheck_pairing();
        }
        self.generation += 1;
    }
}

/// `step` along from `at` in a list of `len`, wrapping.
const fn stepped(at: usize, step: isize, len: usize) -> usize {
    (at as isize + step).rem_euclid(len as isize) as usize
}

/// The pane to show in `session`: the server's focus when it's there.
pub(super) fn default_pane(topology: &Topology, session: u32) -> Option<ResourceId> {
    let panes = || topology.panes.iter().filter(|p| p.session_id == session);
    let focused = &topology.focused_pane;
    panes()
        .find(|p| &p.terminal_id == focused)
        .or_else(|| panes().find(|p| p.is_focused))
        .or_else(|| panes().next())
        .map(|p| p.terminal_id.clone())
}

/// Lines of history each terminal keeps on hand and asks the server for.
const SCROLLBACK_LINES: u32 = 16_384;

/// Dials on the runtime's thread: a machine that's away shows as connecting.
/// With no attach the connection only reads the inventory, on this one and
/// on every reconnect, until `settle_home` selects a session.
pub(super) fn connect(target: Target) -> Result<Client, String> {
    Runtime::connect(
        target,
        ClientOptions {
            control: ControlOptions {
                client_name: "phux-rune".to_owned(),
                viewport: (0, 0),
                attach: None,
                session_recovery: SessionRecovery::CallerValidated,
                attach_role: Some(RolePolicy::PRIMARY),
                scrollback_lines: SCROLLBACK_LINES,
                // Typed text shows as typed, never underlined while unconfirmed.
                prediction_style: PredictionStyle::Plain,
                // A phone has no prompt for a pane's links.
                open_urls: cfg!(any(target_os = "linux", target_os = "macos")),
                // ACP questions arrive as AgentSession views.
                subscribe_agent_sessions: true,
                // A native checkpoint encodes Kitty pixels as one history
                // record. A HiDPI browser frame can exceed the 1 MiB default.
                bootstrap_limits: phux_protocol::caps::BootstrapLimits::clamped(
                    phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES,
                    phux_protocol::MAX_HISTORY_PAGE_BYTES,
                ),
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )
    .map_err(|error| format!("could not connect: {error}"))
}

#[cfg(test)]
mod tests;
