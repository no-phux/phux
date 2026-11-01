//! The background half: Neovim, its grids and one pane per window. After
//! each Neovim flush, `reconcile` gives new windows panes and hangs up panes
//! whose windows closed; then every pane is drawn.
//!
//! Everything runs on this one thread, fed by a channel: a reader thread for
//! Neovim, two threads per pane, and one per `phux spawn`.

mod io;
mod typing;

use std::collections::{HashMap, HashSet};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use rmpv::Value;

use crate::grid::{Grids, Kind};
use crate::input::{Event as Input, Parser};
use crate::render::{Rect, View};
use crate::rpc::{Msg, Nvim, handle, read_loop};
use io::{spawn_acceptor, spawn_pane, spawn_writer};

const LUA: &str = include_str!("windows.lua");

/// The launcher's pane, which exists before Neovim does.
const STARTUP_PANE: u32 = 0;

enum Event {
    Nvim(Msg),
    NvimExited,
    Connected {
        pane: u32,
        terminal: u32,
        cols: usize,
        rows: usize,
        stream: UnixStream,
    },
    Input(u32, Vec<u8>),
    Resized(u32, usize, usize),
    Gone(u32),
    SpawnFailed(u32, String),
}

struct Pane {
    id: u32,
    /// phux's id for it, once its helper says hello.
    terminal: Option<u32>,
    /// Null while a replacement window opens.
    win: Option<i64>,
    surface: Option<Surface>,
    last_asked: (u32, usize, usize),
}

struct Surface {
    view: View,
    parser: Parser,
    /// A float a button went down on: its drags and release go there too.
    grab: Option<Rect>,
    frames: Sender<Vec<u8>>,
    stream: UnixStream,
}

/// Where Neovim put a new window.
#[derive(Clone, Copy, Debug)]
struct Split {
    side_by_side: bool,
    before: bool,
    from: i64,
}

struct Daemon {
    nvim: Nvim,
    events: Sender<Event>,
    grids: Grids,
    panes: Vec<Pane>,
    next_pane: u32,
    socket: PathBuf,
    /// Neovim's current window, as the Lua last said; 0 before setup.
    current: i64,
    /// A `nvim_set_current_win` of ours not answered yet: reports sent
    /// before it ran are stale.
    current_req: Option<u32>,
    /// Input waiting for `current_req`'s answer.
    held: Vec<(u32, Vec<Input>)>,
    /// The pane last typed in. phux's focus is the client's, so a plugin
    /// can't move it: after `<C-w>l` the keys keep arriving here and must
    /// keep following Neovim. See [`Daemon::on_input`].
    typing: Option<u32>,
    chan_req: u32,
    setup_req: u32,
    ready: bool,
    /// Windows asked to become external and not seen external since.
    asked: HashSet<i64>,
    /// Windows we asked Neovim to close because their pane went away.
    closing: HashSet<i64>,
    /// Windows closing to reopen: their panes wait for the new window.
    reopening: HashSet<i64>,
    splits: HashMap<i64, Split>,
    editor_size: (usize, usize),
    done: bool,
}

/// Runs until Neovim exits or the last pane goes away.
pub fn run(socket: &Path, args: &[String]) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let (tx, rx) = channel();
    spawn_acceptor(listener, tx.clone());
    let first = wait_for_launcher(&rx);
    let (nvim, stdout) = Nvim::spawn(args)?;
    let reader = tx.clone();
    std::thread::spawn(move || {
        read_loop(stdout, |m| {
            let _ = reader.send(Event::Nvim(m));
        });
        let _ = reader.send(Event::NvimExited);
    });
    let mut d = Daemon {
        nvim,
        events: tx,
        grids: Grids::default(),
        panes: vec![Pane {
            id: STARTUP_PANE,
            terminal: None,
            win: None,
            surface: None,
            last_asked: (0, 0, 0),
        }],
        next_pane: 1,
        socket: socket.to_owned(),
        current: 0,
        current_req: None,
        held: Vec::new(),
        typing: None,
        chan_req: 0,
        setup_req: 0,
        ready: false,
        asked: HashSet::new(),
        closing: HashSet::new(),
        reopening: HashSet::new(),
        splits: HashMap::new(),
        editor_size: (0, 0),
        done: false,
    };
    if let Some(event) = first {
        d.handle(event);
        d.attach();
        while !d.done {
            let Ok(event) = rx.recv() else { break };
            d.handle(event);
        }
    }
    d.shutdown();
    let _ = std::fs::remove_file(socket);
    // a clean end needs no log
    let _ = std::fs::remove_file(socket.with_extension("log"));
    Ok(())
}

/// The launcher connects right after starting us, or something went wrong.
fn wait_for_launcher(rx: &Receiver<Event>) -> Option<Event> {
    rx.recv_timeout(Duration::from_secs(10)).ok()
}

fn int(n: usize) -> Value {
    Value::from(n as u64)
}

impl Daemon {
    /// Attaches as Neovim's UI at the launcher's size and asks for our channel.
    fn attach(&mut self) {
        let (cols, rows) = self
            .pane(STARTUP_PANE)
            .and_then(|p| p.surface.as_ref())
            .map_or((80, 24), |s| (s.view.cols, s.view.rows));
        self.editor_size = (cols, rows);
        let opts = Value::Map(
            ["rgb", "ext_linegrid", "ext_multigrid", "ext_termcolors"]
                .into_iter()
                .map(|k| (k.into(), true.into()))
                .collect(),
        );
        self.nvim
            .request("nvim_ui_attach", vec![int(cols), int(rows), opts]);
        self.chan_req = self.nvim.request("nvim_get_api_info", vec![]);
        self.nvim.flush();
    }

    fn shutdown(&mut self) {
        for p in &self.panes {
            if let Some(s) = &p.surface {
                let _ = s.stream.shutdown(Shutdown::Both);
            }
        }
        if let Ok(pid) = i32::try_from(self.nvim.child.id()) {
            // SAFETY: `pid` is our own child, not yet reaped. SIGHUP makes
            // Neovim preserve swap files and exit; if it exited already the
            // zombie still holds the pid.
            unsafe { libc::kill(pid, libc::SIGHUP) };
        }
        let _ = self.nvim.child.wait();
    }

    fn pane(&self, id: u32) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    fn pane_mut(&mut self, id: u32) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn pane_of_win(&self, win: i64) -> Option<&Pane> {
        (win != 0)
            .then(|| self.panes.iter().find(|p| p.win == Some(win)))
            .flatten()
    }

    fn focused(&self) -> Option<u32> {
        if self.current == 0 {
            return Some(STARTUP_PANE);
        }
        self.pane_of_win(self.current).map(|p| p.id)
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Nvim(m) => self.on_nvim(m),
            Event::NvimExited => self.done = true,
            Event::Connected {
                pane,
                terminal,
                cols,
                rows,
                stream,
            } => self.on_connected(pane, terminal, (cols, rows), stream),
            Event::Input(pane, bytes) => self.on_input(pane, &bytes),
            Event::Resized(pane, cols, rows) => self.on_resized(pane, cols, rows),
            Event::Gone(pane) => self.on_gone(pane),
            Event::SpawnFailed(pane, why) => {
                eprintln!("phux-nvim: a pane failed: {why}");
                self.on_gone(pane);
            }
        }
    }

    fn on_nvim(&mut self, m: Msg) {
        match m {
            Msg::Notify { method, params } => self.on_notify(&method, &params),
            Msg::Request { id } => {
                self.nvim.respond_error(id);
                self.nvim.flush();
            }
            Msg::Response { id, err, result } => {
                if Some(id) == self.current_req {
                    self.current_req = None;
                    self.release_held();
                }
                if let Some(e) = err {
                    eprintln!("phux-nvim: neovim: {e}");
                } else if id == self.chan_req {
                    self.setup(&result);
                } else if id == self.setup_req {
                    self.on_setup(&result);
                }
            }
        }
    }

    fn setup(&mut self, api_info: &Value) {
        let chan = api_info
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let size = self
            .pane(STARTUP_PANE)
            .and_then(|p| p.surface.as_ref())
            .map_or((80, 23), |s| s.view.window_size());
        self.setup_req = self
            .nvim
            .lua(LUA, vec![chan.into(), int(size.0), int(size.1)]);
        self.nvim.flush();
    }

    fn on_setup(&mut self, result: &Value) {
        let first = result.as_array().and_then(|a| a.first()).and_then(handle);
        if let (Some(win), Some(p)) = (first, self.pane_mut(STARTUP_PANE)) {
            p.win = Some(win);
        }
        self.ready = true;
        self.reconcile();
        self.draw();
    }

    fn on_notify(&mut self, method: &str, params: &[Value]) {
        let win = params.first().and_then(handle).unwrap_or(0);
        match method {
            "redraw" => {
                if self.grids.apply_redraw(params) {
                    self.reconcile();
                    self.draw();
                }
            }
            // from before our own change ran; the report after it is coming
            "phux_nvim_win" if self.current_req.is_none() => {
                self.current = win;
                self.follow_focus();
                self.draw();
            }
            "phux_nvim_reopen" => {
                if self.pane_of_win(win).is_some() {
                    self.reopening.insert(win);
                }
            }
            "phux_nvim_split" if self.pane_of_win(win).is_none() => {
                let flag = |i: usize| params.get(i).and_then(Value::as_bool).unwrap_or(false);
                let from = params.get(3).and_then(handle).unwrap_or(0);
                self.splits.insert(
                    win,
                    Split {
                        side_by_side: flag(1),
                        before: flag(2),
                        from,
                    },
                );
            }
            _ => {}
        }
    }

    /// Editor floats and messages fit the focused pane: grid 1 is its size.
    fn follow_focus(&mut self) {
        let Some(id) = self.focused() else { return };
        let Some(s) = self.pane(id).and_then(|p| p.surface.as_ref()) else {
            return;
        };
        let size = (s.view.cols, s.view.rows);
        if size != self.editor_size {
            self.editor_size = size;
            self.nvim
                .request("nvim_ui_try_resize", vec![int(size.0), int(size.1)]);
            self.nvim.flush();
        }
    }

    /// Matches panes to windows: new external windows get panes, ordinary
    /// ones are made external, and panes whose windows closed hang up.
    fn reconcile(&mut self) {
        if !self.ready {
            return;
        }
        let mut gone = Vec::new();
        for p in &mut self.panes {
            let Some(win) = p.win else { continue };
            if self.grids.grid_of(win).is_some() {
                continue;
            }
            if self.reopening.remove(&win) {
                p.win = None;
            } else {
                gone.push(p.id);
            }
        }
        for id in gone {
            self.hang_up(id);
        }
        if self.panes.is_empty() {
            self.done = true;
            return;
        }
        let grids = &self.grids;
        for set in [&mut self.asked, &mut self.closing, &mut self.reopening] {
            set.retain(|&w| grids.grid_of(w).is_some());
        }
        self.splits.retain(|&w, _| grids.grid_of(w).is_some());

        let wins: Vec<_> = self.grids.wins.values().map(|w| (w.win, w.kind)).collect();
        for (win, kind) in wins {
            match kind {
                Kind::Ordinary if !self.asked.contains(&win) => self.adopt(win),
                Kind::External => self.place_external(win),
                _ => {}
            }
        }
        let ids: Vec<u32> = self.panes.iter().map(|p| p.id).collect();
        for id in ids {
            self.size_window(id);
        }
        self.nvim.flush();
    }

    fn adopt(&mut self, win: i64) {
        self.asked.insert(win);
        let size = self
            .pane_of_win(win)
            .and_then(|p| p.surface.as_ref())
            .map_or_else(|| self.new_window_size(win), |s| s.view.window_size());
        self.nvim.lua(
            "phux_nvim.adopt(...)",
            vec![win.into(), int(size.0), int(size.1)],
        );
    }

    fn place_external(&mut self, win: i64) {
        self.asked.remove(&win);
        if self.pane_of_win(win).is_some() || self.closing.contains(&win) {
            return;
        }
        // a pane kept for a reopened window
        if let Some(p) = self
            .panes
            .iter_mut()
            .find(|p| p.win.is_none() && p.surface.is_some())
        {
            p.win = Some(win);
            return;
        }
        self.new_pane(win);
    }

    /// A guess until the new pane says its size: half the pane it splits.
    fn new_window_size(&self, win: i64) -> (usize, usize) {
        let split = self.splits.get(&win).copied();
        let from = split
            .map(|s| s.from)
            .and_then(|f| self.pane_of_win(f))
            .or_else(|| self.focused().and_then(|id| self.pane(id)));
        let Some(s) = from.and_then(|p| p.surface.as_ref()) else {
            return (80, 23);
        };
        let (cols, rows) = s.view.window_size();
        if split.is_none_or(|s| s.side_by_side) {
            ((cols / 2).max(1), rows)
        } else {
            (cols, (rows / 2).max(1))
        }
    }

    /// Opens a pane beside the window this one split from, running a helper
    /// that connects back.
    fn new_pane(&mut self, win: i64) {
        let split = self.splits.get(&win).copied();
        let from = split
            .map(|s| s.from)
            .and_then(|f| self.pane_of_win(f))
            .or_else(|| self.focused().and_then(|id| self.pane(id)))
            .and_then(|p| p.terminal)
            .or_else(|| self.panes.iter().find_map(|p| p.terminal));
        // no anchor yet: the next flush tries again
        let Some(anchor) = from else { return };
        let id = self.next_pane;
        self.next_pane += 1;
        self.panes.push(Pane {
            id,
            terminal: None,
            win: Some(win),
            surface: None,
            last_asked: (0, 0, 0),
        });
        self.splits.remove(&win);
        let events = self.events.clone();
        let socket = self.socket.clone();
        let (side_by_side, before) = split.map_or((true, false), |s| (s.side_by_side, s.before));
        std::thread::spawn(move || {
            if let Err(why) = spawn_pane(anchor, side_by_side, before, &socket, id) {
                let _ = events.send(Event::SpawnFailed(id, why));
            }
        });
    }

    fn on_connected(
        &mut self,
        id: u32,
        terminal: u32,
        (cols, rows): (usize, usize),
        stream: UnixStream,
    ) {
        let Ok(writer) = stream.try_clone() else {
            return;
        };
        let frames = spawn_writer(writer);
        let Some(p) = self.pane_mut(id) else {
            // its window closed before it arrived
            let _ = stream.shutdown(Shutdown::Both);
            return;
        };
        p.terminal = Some(terminal);
        p.surface = Some(Surface {
            view: View::new(cols, rows),
            parser: Parser::default(),
            grab: None,
            frames,
            stream,
        });
        self.size_window(id);
        self.nvim.flush();
        self.reconcile();
        self.draw();
    }

    fn on_resized(&mut self, id: u32, cols: usize, rows: usize) {
        let Some(s) = self.pane_mut(id).and_then(|p| p.surface.as_mut()) else {
            return;
        };
        s.view.resize(cols, rows);
        self.size_window(id);
        if self.focused() == Some(id) {
            self.follow_focus();
        }
        self.nvim.flush();
        self.draw();
    }

    /// Asks Neovim for the window to fit the pane, unless it already was.
    fn size_window(&mut self, id: u32) {
        let Some(p) = self.pane(id) else { return };
        let (Some(s), Some(win)) = (&p.surface, p.win) else {
            return;
        };
        let Some(grid) = self.grids.grid_of(win) else {
            return;
        };
        let (cols, rows) = s.view.window_size();
        let asked = (grid, cols, rows);
        if p.last_asked == asked {
            return;
        }
        if let Some(p) = self.pane_mut(id) {
            p.last_asked = asked;
        }
        self.nvim.lua(
            "phux_nvim.size(...)",
            vec![win.into(), int(cols), int(rows)],
        );
    }

    /// Hangs up a pane: its helper restores the terminal and exits.
    fn hang_up(&mut self, id: u32) {
        let Some(i) = self.panes.iter().position(|p| p.id == id) else {
            return;
        };
        let p = self.panes.remove(i);
        if let Some(s) = p.surface {
            let _ = s.stream.shutdown(Shutdown::Both);
        }
    }

    /// The pane went away: close its window, or end if it was the last.
    fn on_gone(&mut self, id: u32) {
        let Some(i) = self.panes.iter().position(|p| p.id == id) else {
            return;
        };
        let win = self.panes[i].win;
        self.panes.remove(i);
        if self.typing == Some(id) {
            self.typing = None;
        }
        if self.panes.is_empty() {
            self.done = true;
            return;
        }
        if let Some(win) = win {
            self.closing.insert(win);
            self.nvim.lua("phux_nvim.close(...)", vec![win.into()]);
            self.nvim.flush();
        }
    }

    /// Draws every pane that changed.
    fn draw(&mut self) {
        let focused = self.focused();
        for p in &mut self.panes {
            let Some(s) = &mut p.surface else { continue };
            let grid = p.win.and_then(|w| self.grids.grid_of(w));
            let frame = s.view.frame(&self.grids, grid, focused == Some(p.id));
            if !frame.is_empty() {
                let _ = s.frames.send(frame);
            }
        }
    }
}
