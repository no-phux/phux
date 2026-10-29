//! The window's state: one runtime client, the terminal it shows, and the
//! user's commands against them.
//!
//! The runtime does the terminal (replica, reconnect, acknowledged input,
//! history, selection); this owns only which terminal is on screen, its
//! size, and turning Rune events into runtime calls.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_client_runtime::control::{
    ControlOptions, Event, ProjectionFence, SpawnRequest, Status, Topology,
};
use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, ClientOptions, Listener, Runtime, Target, ViewId};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AttachTarget, RolePolicy};
use rune::{Tx, Waker};

use crate::grid::{self, Metrics};
use crate::launch::Resolved;

mod frame;
mod input;
mod palette;

/// Wakes Rune's loop from the runtime's driver thread. Only posts; never
/// re-enters the client.
struct Wake(Waker);

impl Listener for Wake {
    fn on_activity(&self) {
        (self.0)();
    }
}

/// How long a requested terminal may stay missing from the server's list
/// before the window gives up on it (a spawn's id lands before its listing).
const WANTED_GRACE: Duration = Duration::from_secs(5);

/// The exact frame a view build painted, and the unknown-delivery fence it
/// may clear once Rune reports it drawn.
pub struct Painted {
    pub terminal: ResourceId,
    pub view: ViewId,
    pub fence: ProjectionFence,
    pub frame: Arc<GridFrame>,
    pub epoch: u64,
}

/// A size this window asked for: the terminal, its cells, and the cell's
/// device pixels.
type Fitted = (ResourceId, (u16, u16), (u16, u16));

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
    /// The terminal the user asked for, and since when, until its view opens.
    pub wanted: Option<(ResourceId, Instant)>,
    /// A session this window created: show its terminal once it is selected.
    pending_session: Option<String>,
    /// The connection incarnation the shown view belongs to.
    epoch: u64,
    /// A paste waiting for the user's confirmation.
    pub held_paste: Option<String>,
    /// A command's failure (a spawn), shown until the next terminal opens.
    pub notice: Option<String>,
    /// The command palette, while open, and its field and highlighted row.
    pub palette_open: bool,
    pub palette_query: rune::TextEdit,
    pub palette_picked: usize,
    pub sidebar_open: bool,
    /// What every terminal is doing, for the sidebar's marks.
    pub activities: crate::activity::Activities,
    /// The input method's composition at the terminal cursor.
    pub preedit: Option<String>,
    /// The blink phase a blinking cursor shows now, and when typing last
    /// restarted it (app clock seconds).
    pub blink_on: bool,
    blink_since: f64,
    clock: f64,
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
    fitted: Option<Fitted>,
    pub focused: bool,
    gesture: u64,
    wheel_rows: f32,
    sessions_seq: u32,
    /// The fenced frame the last view build painted, for the drawn receipt.
    pub painted: RefCell<Option<Painted>>,
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
            pending_session: None,
            epoch: 0,
            held_paste: None,
            notice: None,
            palette_open: false,
            palette_query: rune::TextEdit::new(""),
            palette_picked: 0,
            sidebar_open: true,
            preedit: None,
            activities: crate::activity::Activities::default(),
            blink_on: true,
            blink_since: 0.0,
            clock: 0.0,
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
            painted: RefCell::new(None),
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
    pub fn poll(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        let Some(client) = self.client.clone() else {
            return false;
        };
        let observation = client.take_activity();
        let mut changed = observation.status != self.status;
        if observation.connection_epoch != self.epoch {
            self.epoch = observation.connection_epoch;
            changed |= self.reopen_after_reconnect(&client);
        }
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
        // Also catches a display-scale change, which re-snaps the cells.
        self.fit();
        changed | self.blink(&client, tx)
    }

    fn apply(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        let marked = self
            .activities
            .apply(&event, self.shown.as_ref().map(|s| &s.terminal));
        marked | self.apply_view(client, event, tx)
    }

    fn apply_view(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
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
                    (Some(id), _) => self.want(id),
                    (None, error) => {
                        self.notice = error.or_else(|| Some("spawn failed".to_owned()))
                    }
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

    fn want(&mut self, terminal: ResourceId) {
        self.wanted = Some((terminal, Instant::now()));
    }

    fn drop_shown(&mut self, client: &Client) -> Option<ResourceId> {
        let shown = self.shown.take()?;
        let _ = client.destroy_view(shown.view);
        self.fitted = None;
        self.painted.replace(None);
        Some(shown.terminal)
    }

    fn close_shown(&mut self, client: &Client) {
        self.drop_shown(client);
        // Fall back to whatever the server now focuses.
        self.wanted = None;
    }

    /// A new connection may be a new server incarnation, whose engine forgot
    /// every view: open the shown terminal again on this connection.
    fn reopen_after_reconnect(&mut self, client: &Client) -> bool {
        let Some(terminal) = self.drop_shown(client) else {
            return false;
        };
        if self.wanted.is_none() {
            self.want(terminal);
        }
        true
    }

    /// Which terminal should be on screen, if it is not yet: the one the user
    /// asked for, a new session's once it is selected, else the server's
    /// focus. `None` also while waiting on one of those.
    fn target(&mut self, client: &Client) -> Option<ResourceId> {
        let topology = self.topology.as_ref()?;
        if let Some((terminal, since)) = &self.wanted {
            let listed = topology.panes.iter().any(|p| &p.terminal_id == terminal);
            if listed || since.elapsed() < WANTED_GRACE {
                return Some(terminal.clone());
            }
            // Gone (a restarted server, a closed pane): fall back below.
            self.wanted = None;
        }
        if let Some(name) = &self.pending_session {
            let session = topology.sessions.iter().find(|s| &s.name == name)?;
            if client.selected_session() != Some(session.id) {
                return None;
            }
            let panes = || topology.panes.iter().filter(|p| p.session_id == session.id);
            let pane = panes()
                .find(|p| p.terminal_id == topology.focused_pane)
                .or_else(|| panes().next())?;
            let terminal = pane.terminal_id.clone();
            self.pending_session = None;
            return Some(terminal);
        }
        if self.shown.is_some() {
            return None;
        }
        default_pane(topology)
    }

    /// Open the target terminal once its replica is ready. Retried on every
    /// drain until it succeeds; the runtime streams attached terminals on
    /// its own, so nothing here forces a reconnect.
    fn open_wanted(&mut self, client: &Client) -> bool {
        let Some(target) = self.target(client) else {
            return false;
        };
        if self.shown.as_ref().is_some_and(|s| s.terminal == target) {
            self.wanted = None;
            return false;
        }
        let Ok(view) = client.create_view(&target) else {
            self.want(target);
            return false;
        };
        self.drop_shown(client);
        self.activities.seen(&target);
        self.shown = Some(Shown {
            terminal: target,
            view,
        });
        self.wanted = None;
        self.notice = None;
        self.generation += 1;
        true
    }

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

    pub fn zoom(&mut self, size: Option<f32>) {
        self.font_size = size.map_or(grid::DEFAULT_FONT_SIZE, |s| {
            s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
        });
        self.fitted = None;
        self.fit();
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
