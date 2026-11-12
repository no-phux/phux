//! Choosing what is on screen across sessions: show a terminal, make a
//! terminal or session, step through them.

use phux_client_runtime::control::{
    ControlOptions, PredictionStyle, SessionRecovery, SpawnRequest, Status, Topology,
};
use phux_client_runtime::{Client, ClientOptions, Runtime, Target};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{AttachTarget, RolePolicy};
use rune::Tx;

use super::Desk;

impl Desk {
    pub fn show(&mut self, terminal: ResourceId, session_name: &str) {
        let attached = self.selected_session_name();
        // A hub's satellite resource is in none of its sessions: it is
        // viewed from whichever one is attached.
        if terminal.host().is_none()
            && !session_name.is_empty()
            && attached.as_deref() != Some(session_name)
            && let Some(client) = &self.client
        {
            client.attach_session(AttachTarget::ByName(session_name.to_owned()));
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.drop_dead_views(&client);
        self.pending_session = None;
        self.want(terminal);
        self.open_wanted(&client);
    }

    /// The selected session's terminals a server restart ended, which
    /// `phux restore` would relaunch: the keys to relaunch them by.
    pub fn ended_by_restart(&self) -> Vec<phux_protocol::ids::ResourceKey> {
        use phux_protocol::ResourceKind;
        use phux_protocol::wire::frame::{ExecState, Outcome};
        let Some(client) = &self.client else {
            return Vec::new();
        };
        let Some(key) = client.selected_session().and_then(|id| {
            let topology = self.topology.as_ref()?;
            topology.sessions.iter().find(|s| s.id == id)?.key
        }) else {
            return Vec::new();
        };
        client
            .organization()
            .members(&key)
            .filter(|r| r.kind == ResourceKind::Terminal && r.live.is_none())
            .filter(|r| {
                r.activation.as_ref().is_some_and(|a| {
                    a.state == ExecState::Completed && a.outcome == Some(Outcome::Unobserved)
                })
            })
            .map(|r| r.key)
            .collect()
    }

    /// Relaunch what a server restart ended in the selected session.
    pub fn restore_session(&mut self) {
        let Some(client) = &self.client else { return };
        for key in self.ended_by_restart() {
            if let Err(error) = client.relaunch_resource(key) {
                self.error = Some(format!("Couldn't restore a terminal: {error}"));
            }
        }
    }

    pub fn selected_session_name(&self) -> Option<String> {
        let id = self.client.as_ref()?.selected_session()?;
        let topology = self.topology.as_ref()?;
        topology
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
    }

    pub fn new_terminal(&mut self) {
        let Some(client) = &self.client else { return };
        let session_id = client.selected_session();
        let request = client.spawn_terminal(SpawnRequest {
            command: None,
            cwd: self.focused_cwd(),
            env: None,
            session_id,
            initial_size: None,
        });
        self.pending_spawns.push(request);
    }

    /// Where the focused terminal is, for what starts beside it. A pane
    /// that hasn't said yet (the split made a moment ago) defers to the
    /// panes around it.
    pub(super) fn focused_cwd(&self) -> Option<String> {
        let shown = self.shown.as_ref()?;
        let topology = self.topology.as_ref()?;
        let cwd = |id: &ResourceId| {
            let cwd = topology.pane(id)?.cwd.clone();
            cwd.filter(|cwd| !cwd.is_empty())
        };
        cwd(&shown.resource).or_else(|| self.on_screen().iter().find_map(cwd))
    }

    pub fn new_session(&mut self) {
        let taken: Vec<String> = self
            .topology
            .iter()
            .flat_map(|t| t.sessions.iter().map(|s| s.name.clone()))
            .collect();
        let name = loop {
            self.sessions_seq += 1;
            let name = format!("rune-{}", self.sessions_seq);
            if !taken.contains(&name) {
                break name;
            }
        };
        self.create_session(name);
    }

    /// A create is one attach across a redial, which only the runtime's
    /// automatic recovery carries; `settle_home` takes it back. The session
    /// is someone's to keep, so `settle_home` makes it persistent once it is
    /// listed: closing its last tab leaves it empty.
    pub(super) fn create_session(&mut self, name: String) {
        self.create_session_running(name.clone(), None);
        self.link.keep = Some(name);
    }

    /// As [`Self::create_session`], its seed pane running `command` rather
    /// than the shell, and ephemeral: it ends with its last terminal.
    pub(super) fn create_session_running(&mut self, name: String, command: Option<Vec<String>>) {
        self.link.keep = None;
        let cwd = self.focused_cwd();
        let Some(client) = &self.client else { return };
        client.with_control(|control| control.set_session_recovery(SessionRecovery::Automatic));
        client.attach_session(AttachTarget::CreateIfMissing {
            name: name.clone(),
            command,
            cwd,
        });
        // Show its seed pane once the topology lists it as selected; the
        // current terminal stays until then.
        self.link.creating = true;
        self.pending_session = Some(name);
        self.wanted = None;
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
        if self.focus_told == now {
            return false;
        }
        if let Some(old) = self.focus_told.take() {
            let _ = client.send_focus(&old, FocusEvent::Lost);
        }
        let Some(new) = now else { return false };
        if client.send_focus(&new, FocusEvent::Gained) {
            self.focus_told = Some(new);
            return false;
        }
        self.status == Status::Attached
    }

    /// Step through everything this window can show, every session's.
    pub fn cycle(&mut self, step: isize) {
        let all = self.addressable();
        if all.is_empty() {
            return;
        }
        let current = self
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
            .tabs
            .list
            .iter()
            .flat_map(|t| t.resources.clone())
            .collect();
        let Some(session) = self.selected_session_name().filter(|_| !all.is_empty()) else {
            return;
        };
        let current = self
            .shown
            .as_ref()
            .and_then(|s| all.iter().position(|r| *r == s.resource))
            .unwrap_or(0);
        let next = all[stepped(current, step, all.len())].clone();
        self.zoomed = None;
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
            .shown
            .as_ref()
            .and_then(|s| self.addressable().into_iter().find(|r| r.id == s.resource))
            .map_or_else(
                || "phux".to_owned(),
                |r| format!("{} — {}", r.label, r.session),
            );
        if self.window_title != title {
            tx.set_title(title.clone());
            self.window_title = title;
        }
    }

    pub fn focus_changed(&mut self, focused: bool) {
        self.focused = focused;
        // Back at the window: what's on screen has now been seen, and a
        // laptop's socket may have died while it was away.
        if focused {
            for id in self.on_screen() {
                self.activities.seen(&id);
            }
            self.hurry_link();
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
                // ACP questions arrive as AgentSession views.
                subscribe_agent_sessions: true,
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )
    .map_err(|error| format!("could not connect: {error}"))
}
