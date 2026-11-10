//! Choosing what is on screen across sessions: show a terminal, make a
//! terminal or session, step through them.

use std::time::Duration;

use phux_client_runtime::control::{
    ControlOptions, PredictionStyle, SessionRecovery, SpawnRequest, Status, Topology,
};
use phux_client_runtime::{Client, ClientOptions, ConnectOptions, Runtime, Target};
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
        cwd(&shown.terminal).or_else(|| self.on_screen().iter().find_map(cwd))
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
    /// automatic recovery carries; `settle_home` takes it back.
    pub(super) fn create_session(&mut self, name: String) {
        self.create_session_running(name, None);
    }

    /// As [`Self::create_session`], its seed pane running `command` rather
    /// than the shell.
    pub(super) fn create_session_running(&mut self, name: String, command: Option<Vec<String>>) {
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
            .shown
            .as_ref()
            .filter(|s| self.focused && s.view.is_some());
        let now = now.map(|shown| shown.terminal.clone());
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

    /// Step through every terminal the server lists.
    pub fn cycle(&mut self, step: isize) {
        let Some(topology) = &self.topology else {
            return;
        };
        let panes = &topology.panes;
        if panes.is_empty() {
            return;
        }
        let current = self
            .shown
            .as_ref()
            .and_then(|s| panes.iter().position(|p| p.terminal_id == s.terminal))
            .unwrap_or(0);
        let next = (current as isize + step).rem_euclid(panes.len() as isize) as usize;
        let pane = panes[next].clone();
        self.show(pane.terminal_id, &pane.session_name);
    }

    /// The window title: the focused terminal's label and its session, set
    /// only when it changes.
    pub fn window_title(&self) -> &str {
        &self.window_title
    }

    pub(super) fn title(&mut self, tx: &mut Tx) {
        let title = self
            .shown
            .as_ref()
            .and_then(|s| {
                self.topology
                    .as_ref()?
                    .panes
                    .iter()
                    .find(|p| p.terminal_id == s.terminal)
            })
            .map_or_else(
                || "phux".to_owned(),
                |p| format!("{} — {}", crate::palette::pane_label(p), p.session_name),
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

/// How long a dial may take. Someone holding a phone is told their computer
/// can't be reached sooner than a desktop window gives up on a machine.
fn patience(phone: bool) -> ConnectOptions {
    let patient = ConnectOptions::default();
    if !phone {
        return patient;
    }
    ConnectOptions {
        dial_timeout: Duration::from_secs(8),
        initial_budget: Duration::from_secs(10),
        ..patient
    }
}

/// Dials on the runtime's thread: a machine that's away shows as connecting.
/// With no attach the connection only reads the inventory, on this one and
/// on every reconnect, until `settle_home` selects a session.
pub(super) fn connect(target: Target, phone: bool) -> Result<Client, String> {
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
            connect: patience(phone),
        },
    )
    .map_err(|error| format!("could not connect: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_gives_up_on_a_dial_sooner_than_a_desktop() {
        let (desktop, phone) = (patience(false), patience(true));
        assert_eq!(desktop, phux_client_runtime::ConnectOptions::default());
        assert_eq!(phone.dial_timeout, Duration::from_secs(8));
        assert_eq!(phone.initial_budget, Duration::from_secs(10));
        assert!(phone.dial_timeout < desktop.dial_timeout);
        assert!(phone.initial_budget < desktop.initial_budget);
        assert_eq!(phone.ladder, desktop.ladder);
    }
}
