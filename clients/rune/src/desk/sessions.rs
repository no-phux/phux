//! Choosing what is on screen across sessions: show a terminal, make a
//! terminal or session, step through them, jump to what needs you.

use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::AttachTarget;

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

    /// Show the next terminal, after the shown one, that needs a person.
    pub fn jump_to_attention(&mut self) {
        let Some(topology) = &self.topology else {
            return;
        };
        let panes = &topology.panes;
        let start = self
            .shown
            .as_ref()
            .and_then(|s| panes.iter().position(|p| p.terminal_id == s.terminal))
            .map_or(0, |i| i + 1);
        let next = (0..panes.len())
            .map(|k| &panes[(start + k) % panes.len()])
            .find(|p| self.activities.mark(&p.terminal_id).needs_you())
            .cloned();
        if let Some(pane) = next {
            self.show(pane.terminal_id, &pane.session_name);
        }
    }
}
