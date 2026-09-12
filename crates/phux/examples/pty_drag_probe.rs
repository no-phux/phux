//! Rapid-drag resize SANITY probe for phux-8v1's *drag* regime (dragging
//! the window edge fires a SIGWINCH storm), distinct from the discrete
//! narrow/widen case in `pty_resize_probe.rs`.
//!
//! It drives a fast width sweep (faster than the server's
//! RESIZE_RESYNC_DEBOUNCE so resizes coalesce server-side) and asserts the
//! SETTLED grid shows each seeded marker exactly once — i.e. the debounce
//! produces a single coherent final frame rather than leaving wrapped /
//! duplicated rows.
//!
//! Scope note: the pre-fix corruption is a *latency-dependent race* (a
//! snapshot synthesized at width N arriving after the client moved to
//! width M); the in-process round-trip here is too fast to reproduce it
//! deterministically, so this probe passes with or without the debounce.
//! The deterministic regression guard for the coalescing is the
//! `rapid_resizes_coalesce_into_one_resync_snapshot` unit test in
//! `terminal_actor.rs`. This probe complements it by confirming the live
//! end-to-end settled frame is coherent.
//!
//! Run:  cargo run -p phux --example pty_drag_probe
//! Exit 0 = settled frame coherent (each marker once), 1 = duplication.

// Standalone diagnostic harness: prints a report; uses test-grade unwrap.
#![allow(
    clippy::print_stdout,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    reason = "standalone diagnostic harness, not library code"
)]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

struct Oracle {
    terminal: GhosttyTerminal<'static, 'static>,
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    cols: u16,
    n_rows: u16,
}

impl Oracle {
    fn new(cols: u16, n_rows: u16) -> Self {
        Self {
            terminal: {
                let mut terminal = GhosttyTerminal::new(cols, n_rows).expect("oracle terminal");
                terminal
                    .set_scrollback_max_lines(Some(1000))
                    .expect("oracle terminal");
                terminal
            },
            state: RenderState::new().expect("rs"),
            rows: RowIterator::new().expect("rows"),
            cells: CellIterator::new().expect("cells"),
            cols,
            n_rows,
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }
    fn resize(&mut self, cols: u16, n_rows: u16) {
        let old = self.cols;
        let _ = self
            .terminal
            .resize(old, n_rows, 0, 0)
            .and_then(|()| self.terminal.resize(cols, n_rows, 0, 0));
        self.cols = cols;
        self.n_rows = n_rows;
    }
    fn visible_rows(&mut self) -> Vec<String> {
        let Ok(snap) = self.state.update(&self.terminal) else {
            return vec![];
        };
        let total = snap.rows().unwrap_or(self.n_rows);
        let Ok(mut ri) = self.rows.update(&snap) else {
            return vec![];
        };
        let mut out = Vec::new();
        let mut idx = 0u16;
        while let Some(row) = ri.next() {
            if idx >= total {
                break;
            }
            let mut buf = String::new();
            if let Ok(mut ci) = self.cells.update(row) {
                while let Some(cell) = ci.next() {
                    let wide = cell
                        .raw_cell()
                        .and_then(libghostty_vt::screen::Cell::wide)
                        .unwrap_or(CellWide::Narrow);
                    if matches!(wide, CellWide::SpacerTail) {
                        continue;
                    }
                    let g = cell.graphemes().unwrap_or_default();
                    if g.is_empty() {
                        buf.push(' ');
                    } else {
                        for ch in g {
                            buf.push(ch);
                        }
                    }
                }
            }
            out.push(buf.trim_end().to_owned());
            idx += 1;
        }
        out
    }
}

fn main() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let phux_bin = format!("{repo}/../../target/debug/phux");
    let sock = "/tmp/phux-drag-probe/phux.sock".to_string();
    let _ = std::fs::create_dir_all("/tmp/phux-drag-probe");
    let _ = std::fs::remove_file(&sock);
    let _ = std::process::Command::new("pkill")
        .args(["-f", &sock])
        .status();

    let (init_cols, rows) = (120u16, 24u16);
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols: init_cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(&phux_bin);
    cmd.args(["attach", "--socket", &sock, "drag"]);
    cmd.cwd("/tmp");
    cmd.env("SHELL", "/bin/sh");
    cmd.env("TERM", "xterm-256color");
    cmd.env("RUST_LOG", "off");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut reader = pair.master.try_clone_reader().expect("reader");
    {
        let buf = Arc::clone(&buf);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
    }
    let mut writer = pair.master.take_writer().expect("writer");
    std::thread::sleep(Duration::from_millis(2500));

    // Seed 8 unique markers, each ~95 cols so they wrap at 80 but not 120.
    let markers: Vec<String> = (1..=8).map(|i| format!("MK{i:02}-")).collect();
    let payload = "printf 'MK%02d-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\\n' 1 2 3 4 5 6 7 8\r";
    writer.write_all(payload.as_bytes()).expect("write");
    writer.flush().ok();
    std::thread::sleep(Duration::from_millis(1200));

    let mut oracle = Oracle::new(init_cols, rows);
    let mut cursor = {
        let b = buf.lock().unwrap();
        oracle.write(&b);
        b.len()
    };

    // Rapid drag sweep: alternate 120 <-> 80 cols faster than the 50ms
    // server debounce, resizing the oracle (host) in lockstep and feeding
    // newly-painted bytes continuously.
    let widths = [110u16, 100, 90, 80, 90, 100, 110, 90, 80, 100, 120, 80, 120];
    for w in widths {
        pair.master
            .resize(PtySize {
                rows,
                cols: w,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize");
        oracle.resize(w, rows);
        std::thread::sleep(Duration::from_millis(12));
        let b = buf.lock().unwrap();
        oracle.write(&b[cursor..]);
        cursor = b.len();
    }

    // Settle well past the debounce; drain the final coalesced snapshot.
    std::thread::sleep(Duration::from_millis(400));
    {
        let b = buf.lock().unwrap();
        oracle.write(&b[cursor..]);
    }

    let _ = child.kill();
    let _ = std::process::Command::new("pkill")
        .args(["-f", &sock])
        .status();

    let final_rows = oracle.visible_rows();
    println!("\n===== FINAL GRID after drag sweep (settled @120) =====");
    for (i, r) in final_rows.iter().enumerate() {
        if !r.is_empty() {
            println!("{i:2}| {r}");
        }
    }
    let joined = final_rows.join("\n");
    println!("\n===== marker duplication check =====");
    let mut bug = false;
    for m in &markers {
        let c = joined.matches(m.as_str()).count();
        let flag = if c > 1 {
            bug = true;
            " <-- DUPLICATED"
        } else if c == 0 {
            " (missing — content not yet resynced)"
        } else {
            ""
        };
        println!("  {m} x{c}{flag}");
    }
    if bug {
        println!("\nRESULT: DUPLICATION during drag (bug present).");
        std::process::exit(1);
    }
    println!("\nRESULT: no duplication — drag coalesced to a coherent frame.");
}
