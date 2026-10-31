//! Choosing what is on screen across sessions: show a terminal, make a
//! terminal or session, step through them.

use phux_client_runtime::control::{ControlOptions, SessionRecovery, SpawnRequest, Topology};
use phux_client_runtime::{Client, ClientOptions, Runtime, Target};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{AttachTarget, RolePolicy};
use rune::Tx;

use super::Desk;

impl Desk {
    pub fn show(&mut self, terminal: ResourceId, session_name: &str) {
        let attached = self.selected_session_name();
        if attached.as_deref() != Some(session_name)
            && let Some(client) = &self.client
        {
            client.attach_session(AttachTarget::ByName(session_name.to_owned()));
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        // A click on the shown terminal whose view died reopens it.
        if let Some(shown) = &self.shown
            && shown.terminal == terminal
            && client.acquire_view(shown.view).is_none()
        {
            self.drop_shown(&client);
        }
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
        let cwd = self.focused_cwd();
        let Some(client) = &self.client else { return };
        client.with_control(|control| control.set_session_recovery(SessionRecovery::Automatic));
        client.attach_session(AttachTarget::CreateIfMissing {
            name: name.clone(),
            command: None,
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
    /// connection. Retried until the terminal can take input.
    pub(super) fn sync_focus(&mut self, client: &Client) {
        let now = self.shown.as_ref().filter(|_| self.focused);
        let now = now.map(|shown| shown.terminal.clone());
        if self.focus_told == now {
            return;
        }
        if let Some(old) = self.focus_told.take() {
            let _ = client.send_focus(&old, FocusEvent::Lost);
        }
        if let Some(new) = now
            && client.send_focus(&new, FocusEvent::Gained)
        {
            self.focus_told = Some(new);
        }
        client.wake();
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
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )
    .map_err(|error| format!("could not connect: {error}"))
}
