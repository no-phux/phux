//! Choosing what is on screen across sessions: show a terminal, make a
//! terminal or session, step through them.

use phux_client_runtime::control::{ControlOptions, SpawnRequest, Topology};
use phux_client_runtime::{Client, ClientOptions, Runtime, Target};
use phux_protocol::ids::ResourceId;
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
            cwd: None,
            env: None,
            session_id,
            initial_size: None,
        });
        self.pending_spawns.push(request);
    }

    pub fn new_session(&mut self) {
        let Some(client) = &self.client else { return };
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
        client.attach_session(AttachTarget::CreateIfMissing {
            name: name.clone(),
            command: None,
            cwd: None,
        });
        // Show its seed pane once the topology lists it as selected; the
        // current terminal stays until then.
        self.pending_session = Some(name);
        self.wanted = None;
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
        // Back at the window: what's on screen has now been seen.
        if focused {
            for id in self.on_screen() {
                self.activities.seen(&id);
            }
        }
        self.generation += 1;
    }
}

pub(super) fn default_pane(topology: &Topology) -> Option<ResourceId> {
    let focused = &topology.focused_pane;
    topology
        .panes
        .iter()
        .find(|p| &p.terminal_id == focused)
        .or_else(|| topology.panes.iter().find(|p| p.is_focused))
        .or_else(|| topology.panes.first())
        .map(|p| p.terminal_id.clone())
}

/// Lines of history each terminal keeps on hand and asks the server for.
const SCROLLBACK_LINES: u32 = 16_384;

/// Dials on the runtime's thread: a machine that's away shows as connecting.
pub(super) fn connect(target: Target, session: Option<String>) -> Result<Client, String> {
    let attach = session.map_or(AttachTarget::Last, AttachTarget::ByName);
    Runtime::connect(
        target,
        ClientOptions {
            control: ControlOptions {
                client_name: "phux-rune".to_owned(),
                viewport: (0, 0),
                attach: Some(attach),
                attach_role: Some(RolePolicy::PRIMARY),
                scrollback_lines: SCROLLBACK_LINES,
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )
    .map_err(|error| format!("could not connect: {error}"))
}
