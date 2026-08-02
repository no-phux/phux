//! phux-foz.11 regression: rapid window switching / control spam must never
//! leave doubled or ghosted text on screen.
//!
//! Root cause: phux-7ubw made `paint_full_frame` / `paint_focused_pane`
//! letterbox-centre an undersized mirror inside its render rect, but the
//! bootstrap and live-output paint paths still painted the same mirror pinned
//! at the rect origin. Whenever the server-authoritative mirror grid lags the
//! client rect (any resize handshake in flight: sidebar toggle, zoom, split,
//! attach from a larger terminal) the two paint families put the SAME
//! content at TWO different origins — and because the incremental painter
//! only touches dirty rows, neither copy clears the other. The user sees
//! doubled/ghosted text until something forces a full repaint.
//!
//! The tests here drive the REAL paint pipeline (`handle_server_frame`,
//! `paint_full_frame`) in the driver's own sequencing, feed every emitted
//! byte into a "glass" libghostty terminal (exactly what a real terminal
//! would parse), and diff the glass grid against a reference compose of the
//! pane mirrors. Two deterministic unit tests pin the compose invariant per
//! fixed paint path; the stress test replays the dogfood scenario — rapid
//! window cycling, sidebar toggling, palette open/close, synchronized-output
//! bursts, and snapshot resyncs, all while panes emit continuous output.

use std::collections::HashMap;

use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_client_core::engine::ghostty::GhosttyAdapter;
use phux_client_core::session::{EffectBuffer as KernelEffectBuffer, KernelInput, SessionKernel};
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::{
    BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, StreamId,
};
use phux_protocol::wire::info::{LayoutNode, SplitDir};

use super::driver::PaneSlot;
use super::paint::{SidebarEdge, SidebarReservation, content_rect, paint_full_frame};
use super::server_frame::{AgentMetaIndex, handle_server_frame};
use crate::layout::{LayoutState, WindowState, Workspace};
use crate::predict::{Overlay, PredictionState, PredictiveConfig};

fn tid(id: u32) -> TerminalId {
    TerminalId::local(id)
}

/// Read a terminal's visible grid as one `String` per row (blank cells as
/// spaces; ASCII-only fixtures, so no wide-glyph spacer handling needed).
/// Uses its own iterators so reading never disturbs a pane renderer's
/// incremental dirty state.
fn term_grid(term: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut state = RenderState::new().expect("RenderState");
    let mut rows_it = RowIterator::new().expect("RowIterator");
    let mut cells_it = CellIterator::new().expect("CellIterator");
    let snap = state.update(term).expect("snapshot");
    let total_rows = snap.rows().expect("rows");
    let total_cols = snap.cols().expect("cols");
    let mut grid = Vec::new();
    let mut ri = rows_it.update(&snap).expect("row iter");
    let mut r: u16 = 0;
    while let Some(row) = ri.next() {
        if r >= total_rows {
            break;
        }
        let mut line = String::new();
        let mut ci = cells_it.update(row).expect("cell iter");
        let mut c: u16 = 0;
        while let Some(cell) = ci.next() {
            if c >= total_cols {
                break;
            }
            let graphemes = cell.graphemes().expect("graphemes");
            if graphemes.is_empty() {
                line.push(' ');
            } else {
                for ch in graphemes {
                    line.push(ch);
                }
            }
            c += 1;
        }
        grid.push(line);
        r += 1;
    }
    grid
}

fn dump(grid: &[String]) -> String {
    grid.iter()
        .enumerate()
        .map(|(i, l)| format!("{i:2}|{l}|"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A driver-shaped rig: the attach loop's paint-relevant state, plus a
/// "glass" terminal that parses every emitted byte the way a real terminal
/// would. Methods mirror the `main_loop` arms one-to-one:
/// server frames go through the real `handle_server_frame`, and every
/// layout-changing control (window switch, sidebar toggle, overlay dismiss)
/// triggers the same `paint_full_frame` the driver runs on
/// `layout_changed`.
struct Rig {
    panes: HashMap<TerminalId, PaneSlot>,
    workspace: Workspace,
    focused: Option<TerminalId>,
    zoomed: Option<TerminalId>,
    session_name: String,
    sidebar: Option<SidebarReservation>,
    overlay_active: bool,
    predict: PredictionState,
    pending_splits: HashMap<u32, super::actions::PendingSplit>,
    pending_windows: HashMap<u32, super::actions::PendingWindow>,
    agent_meta: AgentMetaIndex,
    glass: GhosttyTerminal<'static, 'static>,
    viewport: (u16, u16),
    seq: HashMap<TerminalId, u64>,
    kernel: SessionKernel<GhosttyAdapter>,
    kernel_effects: KernelEffectBuffer,
    bootstraps: HashMap<TerminalId, BootstrapId>,
    next_bootstrap: u64,
    attach_released: bool,
}

impl Rig {
    fn new(workspace: Workspace, viewport: (u16, u16)) -> Self {
        let focused = workspace.active_window().and_then(|ls| ls.focus.clone());
        let terminals = workspace
            .windows
            .iter()
            .filter_map(|window| window.state.tree.as_ref())
            .flat_map(crate::layout::leaves)
            .collect::<Vec<_>>();
        let mut kernel = SessionKernel::new(
            GhosttyAdapter::new(BootstrapLimits::default()),
            BootstrapProfile::SynthesizedVtRaw,
        );
        let mut kernel_effects = KernelEffectBuffer::new();
        kernel
            .update(
                KernelInput::AttachStarted {
                    attach_id: 1,
                    terminals: &terminals,
                },
                &mut kernel_effects,
            )
            .expect("attach started");
        Self {
            panes: HashMap::new(),
            workspace,
            focused,
            zoomed: None,
            session_name: "stress".to_owned(),
            sidebar: None,
            overlay_active: false,
            predict: PredictionState::new(PredictiveConfig::disabled(), viewport.0, viewport.1),
            pending_splits: HashMap::new(),
            pending_windows: HashMap::new(),
            agent_meta: AgentMetaIndex::default(),
            glass: GhosttyTerminal::new(TerminalOptions {
                cols: viewport.0,
                rows: viewport.1,
                max_scrollback: 200,
            })
            .expect("glass terminal"),
            viewport,
            seq: HashMap::new(),
            kernel,
            kernel_effects,
            bootstraps: HashMap::new(),
            next_bootstrap: 1,
            attach_released: false,
        }
    }

    /// Seed a pane slot at an explicit (server-authoritative) mirror size
    /// with initial content — the "resize handshake in flight" fixture when
    /// the size differs from the pane's layout rect.
    fn seed_pane(&mut self, id: &TerminalId, cols: u16, rows: u16, content: &[u8]) {
        self.panes
            .insert(id.clone(), PaneSlot::new_with_size(cols, rows).expect("pane slot"));
        self.snapshot(id, cols, rows, content);
    }

    /// Run one inbound server frame through the REAL dispatcher and parse
    /// whatever it painted into the glass.
    fn drive(&mut self, frame: FrameKind) -> super::server_frame::FrameOutcome {
        let mut out: Vec<u8> = Vec::new();
        let overlay = Overlay;
        let outcome = handle_server_frame(
            &mut self.kernel,
            &mut self.kernel_effects,
            &mut out,
            frame,
            &mut self.panes,
            &mut self.workspace,
            &mut self.focused,
            &mut self.zoomed,
            &mut self.session_name,
            None,
            self.sidebar,
            self.viewport,
            &mut self.predict,
            &overlay,
            None,
            &mut self.pending_splits,
            &mut self.pending_windows,
            &mut std::collections::HashSet::new(),
            &mut self.agent_meta,
            self.overlay_active,
            false,
        )
        .expect("handle_server_frame");
        self.glass.vt_write(&out);
        outcome
    }

    /// `TERMINAL_OUTPUT` for `pane` — the hot path.
    fn output(&mut self, pane: &TerminalId, bytes: &[u8]) {
        let seq = self.seq.entry(pane.clone()).or_insert(0);
        *seq += 1;
        let seq = *seq;
        let bootstrap_id = *self.bootstraps.get(pane).expect("published bootstrap");
        self.drive(FrameKind::TerminalOutput {
            terminal_id: pane.clone(),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id,
            seq,
            bytes: bytes::Bytes::copy_from_slice(bytes),
        });
    }

    /// Negotiated synthesized-VT replacement bootstrap for one pane.
    fn snapshot(&mut self, pane: &TerminalId, cols: u16, rows: u16, replay: &[u8]) {
        let bootstrap_id = BootstrapId::new(self.next_bootstrap).expect("bootstrap");
        self.next_bootstrap += 1;
        let stream_id = StreamId::new(1).expect("stream");
        self.drive(FrameKind::BootstrapBegin {
            terminal_id: pane.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq: self.seq.get(pane).copied().unwrap_or(0),
        });
        self.drive(FrameKind::BootstrapChunk {
            terminal_id: pane.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(replay),
        });
        let outcome = self.drive(FrameKind::BootstrapReady {
            terminal_id: pane.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        });
        if outcome.layout_replaced {
            self.full_repaint();
        }
        self.bootstraps.insert(pane.clone(), bootstrap_id);
    }

    /// The driver's `layout_changed` repaint: full frame of the render
    /// window (ED2 + every pane + cursor), exactly as `main_loop` runs it.
    fn full_repaint(&mut self) {
        if !self.attach_released {
            self.drive(FrameKind::AttachReady { attach_id: 1 });
            self.attach_released = true;
        }
        let mut out: Vec<u8> = Vec::new();
        if let Some(ls) = self.workspace.render_window(self.zoomed.as_ref()) {
            paint_full_frame(
                &mut out,
                ls.as_ref(),
                &mut self.panes,
                &self.kernel,
                self.focused.as_ref(),
                self.viewport,
                None,
                self.sidebar,
                None,
                &self.session_name,
            );
        }
        self.glass.vt_write(&out);
    }

    /// `next-window` (`C-a n`): advance the active window, re-anchor focus,
    /// repaint — the `switch_window` + `layout_changed` sequence.
    fn switch_next(&mut self) {
        self.workspace.next();
        self.focused = self
            .workspace
            .active_window()
            .and_then(|ls| ls.focus.clone());
        self.full_repaint();
    }

    /// `toggle-sidebar`: flip the reservation and repaint into the new
    /// content rect, as the dispatch arm does after re-folding the flag.
    fn toggle_sidebar(&mut self) {
        self.sidebar = match self.sidebar {
            Some(_) => None,
            None => Some(SidebarReservation {
                edge: SidebarEdge::Left,
                width: 20,
            }),
        };
        self.full_repaint();
    }

    /// Open a floating modal: base full frame, then the modal's box painted
    /// over pane cells (what `paint_active_overlay` + `paint_clipped` do).
    /// While open, `handle_server_frame` suppresses pane paints.
    fn overlay_open(&mut self) {
        self.overlay_active = true;
        self.full_repaint();
        // The modal box scribbles over pane rows — content the dismiss
        // repaint must fully clear.
        self.glass
            .vt_write(b"\x1b[8;15H################ PALETTE ################");
        self.glass
            .vt_write(b"\x1b[9;15H#  ghost stress: modal over the panes   #");
        self.glass
            .vt_write(b"\x1b[10;15H##########################################");
    }

    /// Dismiss the modal: the dispatcher sets `layout_changed`, which full
    /// repaints over the scribbles.
    fn overlay_close(&mut self) {
        self.overlay_active = false;
        self.full_repaint();
    }

    /// Whether any pane is mid synchronized-output transaction (paints
    /// suppressed; the screen legitimately lags the mirror).
    fn sync_active(&self) -> bool {
        self.panes.values().any(|s| s.sync_output_since.is_some())
    }

    /// The compose invariant: at a settled point (no overlay, no open
    /// synchronized-output transaction) the glass must equal the reference
    /// compose of the active window — each visible pane's mirror grid
    /// letterbox-placed in its rect, blank elsewhere in the rect. Cells
    /// outside pane rects (dividers, sidebar strip) are not checked.
    fn assert_consistent(&self, label: &str) {
        assert!(!self.overlay_active, "{label}: assert while overlay active");
        assert!(!self.sync_active(), "{label}: assert while sync suppressed");
        let (cols, rows) = self.viewport;
        let content = content_rect(self.viewport, None, self.sidebar);
        let ls = self
            .workspace
            .render_window(self.zoomed.as_ref())
            .expect("active window");
        let rects = super::multi_pane::compute_layout_in(ls.as_ref(), content, self.viewport).rects;

        let mut expected: Vec<Vec<Option<char>>> =
            vec![vec![None; usize::from(cols)]; usize::from(rows)];
        for (id, rect) in &rects {
            let terminal = super::driver::published_terminal(&self.kernel, id)
                .expect("published pane terminal");
            let mgrid = term_grid(terminal);
            let mcols = terminal.cols().expect("mirror cols");
            let mrows = terminal.rows().expect("mirror rows");
            // The letterbox placement contract: undersized mirror centred
            // with a floor split (extra cell on the trailing edge); mirror
            // >= rect clips at the rect origin.
            let inner_c = mcols.min(rect.w);
            let inner_r = mrows.min(rect.h);
            let pad_x = rect.w.saturating_sub(mcols) / 2;
            let pad_y = rect.h.saturating_sub(mrows) / 2;
            for ry in 0..rect.h {
                for rx in 0..rect.w {
                    let gy = usize::from(rect.y + ry);
                    let gx = usize::from(rect.x + rx);
                    let inside =
                        ry >= pad_y && ry < pad_y + inner_r && rx >= pad_x && rx < pad_x + inner_c;
                    let ch = if inside {
                        mgrid
                            .get(usize::from(ry - pad_y))
                            .and_then(|l| l.chars().nth(usize::from(rx - pad_x)))
                            .unwrap_or(' ')
                    } else {
                        ' '
                    };
                    expected[gy][gx] = Some(ch);
                }
            }
        }

        let glass = term_grid(&self.glass);
        let mut mismatches = Vec::new();
        for (y, row) in expected.iter().enumerate() {
            for (x, want) in row.iter().enumerate() {
                let Some(want) = want else { continue };
                let got = glass.get(y).and_then(|l| l.chars().nth(x)).unwrap_or(' ');
                if got != *want {
                    mismatches.push(format!("({y},{x}): want {want:?}, got {got:?}"));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{label}: glass diverged from the mirror compose ({} cells)\nfirst: {}\nglass:\n{}\n",
            mismatches.len(),
            mismatches.first().map_or("", |s| s.as_str()),
            dump(&glass),
        );
    }
}

/// Window 0: a single full-content pane. Window 1: a side-by-side split.
fn two_window_workspace(p: &TerminalId, q: &TerminalId, r: &TerminalId) -> Workspace {
    Workspace {
        windows: vec![
            WindowState {
                name: "one".to_owned(),
                state: LayoutState::single(p.clone()),
            },
            WindowState {
                name: "two".to_owned(),
                state: LayoutState {
                    tree: Some(LayoutNode::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.5,
                        left: Box::new(LayoutNode::Leaf(q.clone())),
                        right: Box::new(LayoutNode::Leaf(r.clone())),
                    }),
                    focus: Some(q.clone()),
                },
            },
        ],
        active: 0,
    }
}

/// The focused-pane snapshot resync must place an undersized mirror at the
/// SAME letterboxed origin `paint_full_frame` uses — not pinned at the rect
/// origin. Before the phux-foz.11 fix the resync painted at the origin,
/// leaving the full frame's centred copy in place: the same text visible at
/// two offsets (the doubling).
#[test]
fn snapshot_resync_of_undersized_mirror_letterboxes_like_the_full_frame() {
    let p = tid(1);
    let mut rig = Rig::new(
        Workspace {
            windows: vec![WindowState {
                name: "one".to_owned(),
                state: LayoutState::single(p.clone()),
            }],
            active: 0,
        },
        (80, 24),
    );
    // Server grid 40x24 vs client rect 80x24: a resize handshake in flight.
    rig.seed_pane(&p, 40, 24, b"ALPHA-CONTENT");
    rig.full_repaint();
    rig.assert_consistent("baseline full frame");

    // The server's resync snapshot at its (still-lagging) 40x24 size.
    rig.snapshot(&p, 40, 24, b"\x1b[2J\x1b[HALPHA-CONTENT");
    rig.assert_consistent("after focused snapshot resync");
}

/// The non-focused snapshot resync path (multi-pane window) must letterbox
/// identically. Symmetric to the focused case; hits the second
/// `render_at_full` site.
#[test]
fn non_focused_snapshot_resync_letterboxes_like_the_full_frame() {
    let q = tid(2);
    let r = tid(3);
    let mut rig = Rig::new(
        Workspace {
            windows: vec![WindowState {
                name: "two".to_owned(),
                state: LayoutState {
                    tree: Some(LayoutNode::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.5,
                        left: Box::new(LayoutNode::Leaf(q.clone())),
                        right: Box::new(LayoutNode::Leaf(r.clone())),
                    }),
                    focus: Some(q.clone()),
                },
            }],
            active: 0,
        },
        (80, 24),
    );
    rig.seed_pane(&q, 40, 24, b"FOCUSED-LEFT");
    // Right pane's mirror lags well behind its ~39-col rect.
    rig.seed_pane(&r, 21, 24, b"RIGHT-PANE-ROW");
    rig.full_repaint();
    rig.assert_consistent("baseline split full frame");

    rig.snapshot(&r, 21, 24, b"\x1b[2J\x1b[HRIGHT-PANE-ROW");
    rig.assert_consistent("after non-focused snapshot resync");
}

/// Incremental output into a NON-focused undersized pane must paint its
/// dirty rows at the letterboxed origin. Before the fix, dirty rows landed
/// at the rect origin while the full frame's rows sat centred — adjacent
/// rows of one pane at two different x offsets.
#[test]
fn non_focused_output_letterboxes_like_the_full_frame() {
    let q = tid(2);
    let r = tid(3);
    let mut rig = Rig::new(
        Workspace {
            windows: vec![WindowState {
                name: "two".to_owned(),
                state: LayoutState {
                    tree: Some(LayoutNode::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.5,
                        left: Box::new(LayoutNode::Leaf(q.clone())),
                        right: Box::new(LayoutNode::Leaf(r.clone())),
                    }),
                    focus: Some(q.clone()),
                },
            }],
            active: 0,
        },
        (80, 24),
    );
    rig.seed_pane(&q, 40, 24, b"FOCUSED-LEFT");
    rig.seed_pane(&r, 21, 24, b"ROW-A");
    rig.full_repaint();
    rig.assert_consistent("baseline split full frame");

    // New output dirties row 1 only; the incremental paint must land it at
    // the centred origin, in line with ROW-A above it.
    rig.output(&r, b"\r\nROW-B");
    rig.assert_consistent("after non-focused incremental output");
}

/// The dogfood stress (phux-foz.11): continuous output + synchronized-output
/// bursts on every pane while the control plane is spammed — window cycling,
/// sidebar toggling, palette open/close — with server snapshot resyncs
/// landing mid-spam, including resyncs whose grid lags the rect (the resize
/// race). After every settled step the glass must equal the mirror compose:
/// any origin disagreement between the full-frame and incremental/snapshot
/// paints shows up as doubled text and fails the diff.
#[test]
fn rapid_switch_and_control_spam_leaves_no_doubled_text() {
    let p = tid(1);
    let q = tid(2);
    let r = tid(3);
    let mut rig = Rig::new(two_window_workspace(&p, &q, &r), (80, 24));
    rig.seed_pane(&p, 80, 24, b"p: boot\r\n");
    rig.seed_pane(&q, 40, 24, b"q: boot\r\n");
    // R's mirror lags its rect for the whole run (resize race fixture).
    rig.seed_pane(&r, 21, 24, b"r: boot\r\n");
    rig.full_repaint();
    rig.assert_consistent("bootstrap");

    for i in 0..12u32 {
        // Continuous output on every pane; off-screen panes keep their
        // mirrors warm (off-screen invariant), on-screen ones repaint.
        rig.output(&p, format!("p: line {i} aaaaaaaaaaaaaaaa\r\n").as_bytes());
        // Q wraps its redraw in DEC 2026 split across two frames, the way
        // a TUI's synchronized redraw arrives mid-burst.
        rig.output(&q, format!("\x1b[?2026hq: begin {i}").as_bytes());
        rig.output(&q, format!(" ... q: end {i}\r\n\x1b[?2026l").as_bytes());
        rig.output(&r, format!("r: {i}\r\n").as_bytes());
        rig.assert_consistent(&format!("iter {i}: after output"));

        // Control spam.
        match i % 4 {
            0 => rig.switch_next(),
            1 => rig.toggle_sidebar(),
            2 => {
                rig.overlay_open();
                // Output keeps flowing while the modal suppresses paints.
                rig.output(&p, format!("p: modal {i}\r\n").as_bytes());
                rig.output(&r, format!("r: modal {i}\r\n").as_bytes());
                rig.overlay_close();
            }
            _ => {
                rig.switch_next();
                rig.switch_next();
            }
        }
        rig.assert_consistent(&format!("iter {i}: after control spam"));

        // Server resyncs racing the spam: R's snapshot still carries the
        // lagging 21x24 grid; every third iteration P resyncs at a lagging
        // 40x24 grid too (the attach-from-a-bigger-terminal shape), then
        // catches back up to the full 80x24.
        rig.snapshot(&r, 21, 24, format!("\x1b[2J\x1b[Hr: resync {i}").as_bytes());
        rig.assert_consistent(&format!("iter {i}: after r resync"));
        if i % 3 == 0 {
            rig.snapshot(
                &p,
                40,
                24,
                format!("\x1b[2J\x1b[Hp: small resync {i}").as_bytes(),
            );
            rig.assert_consistent(&format!("iter {i}: after p lagging resync"));
            rig.snapshot(
                &p,
                80,
                24,
                format!("\x1b[2J\x1b[Hp: full resync {i}").as_bytes(),
            );
            rig.assert_consistent(&format!("iter {i}: after p full resync"));
        }
    }
}
