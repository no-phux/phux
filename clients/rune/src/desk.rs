//! The window's state: one runtime client, the terminal it shows, and the
//! user's commands against them.
//!
//! The runtime does the terminal (replica, reconnect, acknowledged input,
//! history, selection); this owns only which terminal is on screen, its
//! size, and turning Rune events into runtime calls.

use std::cell::Cell;
use std::sync::Arc;

use phux_client_runtime::control::{
    ControlOptions, Event, ProjectionFence, SpawnRequest, Status, Topology,
};
use phux_client_runtime::engine::{Scroll, SelectionGestureEvent};
use phux_client_runtime::{Client, ClientOptions, Listener, Runtime, Target, ViewId};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AttachTarget, RolePolicy};
use rune::{Phase, Tx, Waker};

use crate::grid::{self, Metrics};
use crate::keys::{self, Shortcut, Typed};
use crate::launch::Resolved;

/// Wakes Rune's loop from the runtime's driver thread. Only posts; never
/// re-enters the client.
struct Wake(Waker);

impl Listener for Wake {
    fn on_activity(&self) {
        (self.0)();
    }
}

/// The terminal on screen.
pub struct Shown {
    pub terminal: ResourceId,
    pub view: ViewId,
}

pub struct Desk {
    pub client: Option<Arc<Client>>,
    /// Why there is no client, or the last failure the user should see.
    pub error: Option<String>,
    pub status: Status,
    pub topology: Option<Topology>,
    pub server: Option<String>,
    pub shown: Option<Shown>,
    /// The terminal the user asked for, until its view opens.
    pub wanted: Option<ResourceId>,
    /// Spawns this window sent, to show the terminal when it lands.
    pending_spawns: Vec<u32>,
    /// Bumped by every frame change of the shown terminal, so the view runs.
    pub generation: u64,
    pub font_size: f32,
    /// The window's device pixels per point, as the last view build saw it.
    pub scale: Cell<f32>,
    /// The grid area in points, from its last layout.
    pub area: [f32; 2],
    /// The size this window last asked the server for, per terminal.
    fitted: Option<(ResourceId, (u16, u16))>,
    pub focused: bool,
    gesture: u64,
    wheel_rows: f32,
    sessions_seq: u32,
    /// The unknown-delivery fence of the frame the last view build painted;
    /// cleared once Rune reports that frame drawn.
    pub painted_fence: Cell<Option<(ResourceId, ProjectionFence)>>,
}

impl Desk {
    pub fn new(resolved: &Resolved) -> Self {
        let mut desk = Self {
            client: None,
            error: None,
            status: Status::Idle,
            topology: None,
            server: None,
            shown: None,
            wanted: None,
            pending_spawns: Vec::new(),
            generation: 0,
            font_size: grid::DEFAULT_FONT_SIZE,
            scale: Cell::new(2.0),
            area: [0.0, 0.0],
            fitted: None,
            focused: true,
            gesture: 0,
            wheel_rows: 0.0,
            sessions_seq: 0,
            painted_fence: Cell::new(None),
        };
        match &resolved.socket {
            Ok(socket) => match connect(socket, resolved.session.clone()) {
                Ok(client) => desk.client = Some(Arc::new(client)),
                Err(error) => desk.error = Some(error),
            },
            Err(error) => desk.error = Some(error.clone()),
        }
        desk
    }

    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.font_size, self.scale.get())
    }

    /// Rune's setup hook: route runtime wakes into Rune's loop.
    pub fn listen(&mut self, waker: Waker) {
        if let Some(client) = &self.client {
            client.set_listener(Arc::new(Wake(waker)));
        }
    }

    /// Drain the runtime once. True when anything on screen may have changed.
    pub fn poll(&mut self, tx: &mut Tx) -> bool {
        let Some(client) = self.client.clone() else {
            return false;
        };
        let observation = client.take_activity();
        let mut changed = observation.status != self.status;
        self.status = observation.status;
        if let Some(error) = observation.last_error.filter(|_| self.status.is_terminal()) {
            changed |= self.error.as_ref() != Some(&error);
            self.error = Some(error);
        }
        if let Some(topology) = client.topology()
            && self.topology.as_ref() != Some(&topology)
        {
            self.topology = Some(topology);
            changed = true;
        }
        if self.server.is_none() {
            self.server = client.server().map(|info| format!("{info:?}"));
        }
        for event in observation.events {
            changed |= self.apply(&client, event, tx);
        }
        changed |= self.open_wanted(&client);
        if changed {
            self.fit();
        }
        changed
    }

    fn apply(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        let shown = |id: &ResourceId| self.shown.as_ref().is_some_and(|s| &s.terminal == id);
        match event {
            Event::TerminalChanged { terminal_id } if shown(&terminal_id) => {
                self.generation += 1;
                true
            }
            Event::TerminalSpawned {
                request_id,
                terminal_id,
                error,
            } if self.pending_spawns.contains(&request_id) => {
                self.pending_spawns.retain(|id| *id != request_id);
                match (terminal_id, error) {
                    (Some(id), _) => self.wanted = Some(id),
                    (None, error) => self.error = error,
                }
                true
            }
            Event::TerminalClosed { terminal_id, .. } if shown(&terminal_id) => {
                self.close_shown(client);
                true
            }
            // OSC 52 only from the terminal on screen, while the window has
            // focus; never retained for later.
            Event::ClipboardWrite { terminal_id, text } if shown(&terminal_id) && self.focused => {
                tx.copy(text.0);
                false
            }
            Event::StatusChanged(_) | Event::TopologyChanged | Event::Attached { .. } => true,
            _ => false,
        }
    }

    fn close_shown(&mut self, client: &Client) {
        if let Some(shown) = self.shown.take() {
            let _ = client.destroy_view(shown.view);
        }
        self.fitted = None;
        self.painted_fence.set(None);
        // Fall back to whatever the server now focuses.
        self.wanted = None;
    }

    /// Open the wanted terminal, else the server's focus, once its stream
    /// is ready. Retried on every drain until it succeeds.
    fn open_wanted(&mut self, client: &Client) -> bool {
        let Some(topology) = &self.topology else {
            return false;
        };
        let target = self.wanted.clone().or_else(|| {
            self.shown
                .is_none()
                .then(|| default_pane(topology))
                .flatten()
        });
        let Some(target) = target else {
            return false;
        };
        if self.shown.as_ref().is_some_and(|s| s.terminal == target) {
            self.wanted = None;
            return false;
        }
        let Ok(view) = client.create_view(&target) else {
            // Not streaming yet: ask for it; the next drain retries.
            let _ = client.ensure_stream(&target);
            return false;
        };
        if let Some(old) = self.shown.replace(Shown {
            terminal: target,
            view,
        }) {
            let _ = client.destroy_view(old.view);
        }
        self.wanted = None;
        self.fitted = None;
        self.generation += 1;
        true
    }

    /// The grid area moved or resized.
    pub fn laid_out(&mut self, area: [f32; 2]) {
        self.area = area;
        self.fit();
    }

    /// Propose the shown terminal's size from the grid area. The runtime
    /// refuses it for observers and before the terminal is ready; a later
    /// drain retries.
    fn fit(&mut self) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        if self.area[0] < 1.0 || self.area[1] < 1.0 {
            return;
        }
        let metrics = self.metrics();
        let size = metrics.fit(self.area);
        if self
            .fitted
            .as_ref()
            .is_some_and(|(id, fitted)| id == &shown.terminal && *fitted == size)
        {
            return;
        }
        let outcome = client.with_control(|control| {
            control.resize_terminal_with_cell(
                &shown.terminal,
                size.0.into(),
                size.1.into(),
                Some(metrics.cell_px()),
            )
        });
        if outcome == phux_client_runtime::control::TerminalResizeOutcome::Queued {
            self.fitted = Some((shown.terminal.clone(), size));
        }
    }

    /// Clear the delivery fence of the frame Rune just drew. This is the
    /// next-frame receipt; it never clears a newer fence.
    pub fn drawn(&self) {
        let Some((terminal, fence)) = self.painted_fence.take() else {
            return;
        };
        if let Some(client) = &self.client {
            client.with_control(|control| control.acknowledge_projection_if(&terminal, fence));
        }
    }

    pub fn show(&mut self, terminal: ResourceId, session_name: &str) {
        let attached = self.selected_session_name();
        if attached.as_deref() != Some(session_name)
            && let Some(client) = &self.client
        {
            client.attach_session(AttachTarget::ByName(session_name.to_owned()));
        }
        self.wanted = Some(terminal);
        if let Some(client) = self.client.clone() {
            self.open_wanted(&client);
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
            name,
            command: None,
            cwd: None,
        });
        // The new session's seed pane becomes the server's focus.
        if let Some(shown) = self.shown.take() {
            let _ = client.destroy_view(shown.view);
        }
        self.wanted = None;
        self.fitted = None;
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

    pub fn zoom(&mut self, size: Option<f32>) {
        self.font_size = size.map_or(grid::DEFAULT_FONT_SIZE, |s| {
            s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
        });
        self.fitted = None;
        self.fit();
    }

    /// Every key nothing else took. True consumes it.
    pub fn key(&mut self, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
        if let Some(action) = keys::shortcut(&event.key, event.mods)
            .or_else(|| keys::scroll_shortcut(&event.key, event.mods))
        {
            self.shortcut(action, tx);
            return true;
        }
        let Some(typed) = keys::typed(&event.key, event.mods, tx.key_base()) else {
            return false;
        };
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return true;
        };
        let terminal = shown.terminal.clone();
        let view = shown.view;
        client.with_control(|control| match typed {
            Typed::Text(text) => control.send_text(&terminal, &text),
            Typed::Key(atom) => control.send_key(&terminal, atom),
        });
        // Typing returns to the live tail and clears a stale highlight.
        if let Some(engine) = client.engine() {
            let _ = engine.clear_view_selection(view);
        }
        let _ = client.with_control(|control| control.scroll_view(view, Scroll::Bottom));
        true
    }

    fn shortcut(&mut self, action: Shortcut, tx: &mut Tx) {
        match action {
            Shortcut::Copy => self.copy(tx),
            Shortcut::Paste => tx.read_clipboard(|desk: &mut Desk, content, _| {
                desk.paste(content.text());
            }),
            Shortcut::NewTerminal => self.new_terminal(),
            Shortcut::NewSession => self.new_session(),
            Shortcut::NextTerminal => self.cycle(1),
            Shortcut::PreviousTerminal => self.cycle(-1),
            Shortcut::ZoomIn => self.zoom(Some(self.font_size + 1.0)),
            Shortcut::ZoomOut => self.zoom(Some(self.font_size - 1.0)),
            Shortcut::ZoomReset => self.zoom(None),
            Shortcut::ScrollTop => self.scroll(Scroll::Top),
            Shortcut::ScrollBottom => self.scroll(Scroll::Bottom),
        }
    }

    fn copy(&self, tx: &mut Tx) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let Some(engine) = client.engine() else {
            return;
        };
        if let Ok(bytes) = engine.view_selection_text(shown.view)
            && !bytes.is_empty()
        {
            tx.copy(String::from_utf8_lossy(&bytes).into_owned());
        }
    }

    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        // The server brackets it per DEC 2004 and classifies it.
        client.with_control(|control| control.apply_user_paste(&shown.terminal, text));
        client.wake();
    }

    fn scroll(&mut self, scroll: Scroll) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let view = shown.view;
        let _ = client.with_control(|control| control.scroll_view(view, scroll));
        self.generation += 1;
    }

    /// Wheel over the grid: whole rows through the view's history.
    pub fn wheel(&mut self, dy: f32) {
        let cell_h = self.metrics().cell_h;
        self.wheel_rows += dy / cell_h;
        let rows = self.wheel_rows.trunc();
        if rows == 0.0 {
            return;
        }
        self.wheel_rows -= rows;
        // Content follows the fingers: positive dy reveals older rows.
        self.scroll(Scroll::Delta(-(rows as i64)));
    }

    /// A press, drag, or release over the grid selects through the engine.
    pub fn select(&mut self, phase: Phase, x: f32, y: f32, clicks: u32) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let Some(frame) = client.acquire_view(shown.view) else {
            return;
        };
        let Some(engine) = client.engine() else {
            return;
        };
        let metrics = self.metrics();
        let (col, row) = metrics.cell_at(x, y, frame.cols, frame.rows);
        let (cell_w, cell_h) = metrics.cell_px();
        let phase = match phase {
            Phase::Down => 0,
            Phase::Move => 1,
            Phase::Up => 2,
        };
        let event = SelectionGestureEvent {
            phase,
            clicks: clicks.clamp(1, 3),
            handle: self.gesture,
            column: col,
            rectangle: false,
            row: u32::from(row),
            x: f64::from(x * metrics.scale),
            y: f64::from(y * metrics.scale),
            columns: u32::from(frame.cols),
            cell_width: u32::from(cell_w),
            screen_height: (u32::from(frame.rows) * u32::from(cell_h)).max(1),
            padding_left: 0,
        };
        if let Ok(result) = engine.view_selection_gesture(shown.view, event) {
            self.gesture = if phase == 2 { 0 } else { result.handle };
        }
        self.generation += 1;
    }

    pub fn focus_changed(&mut self, focused: bool) {
        self.focused = focused;
        self.generation += 1;
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        if let (Some(client), Some(shown)) = (&self.client, self.shown.take()) {
            let _ = client.destroy_view(shown.view);
        }
    }
}

fn default_pane(topology: &Topology) -> Option<ResourceId> {
    let focused = &topology.focused_pane;
    topology
        .panes
        .iter()
        .find(|p| &p.terminal_id == focused)
        .or_else(|| topology.panes.iter().find(|p| p.is_focused))
        .or_else(|| topology.panes.first())
        .map(|p| p.terminal_id.clone())
}

fn connect(socket: &std::path::Path, session: Option<String>) -> Result<Client, String> {
    let attach = session.map_or(AttachTarget::Last, AttachTarget::ByName);
    Runtime::connect(
        Target::uds(socket.to_owned()),
        ClientOptions {
            control: ControlOptions {
                client_name: "phux-rune".to_owned(),
                viewport: (0, 0),
                attach: Some(attach),
                attach_role: Some(RolePolicy::PRIMARY),
                ..ControlOptions::default()
            },
            ..ClientOptions::default()
        },
    )
    .map_err(|error| format!("could not connect to {}: {error}", socket.display()))
}
