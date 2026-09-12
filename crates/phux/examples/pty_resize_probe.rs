//! Raw-PTY resize probe — reproduces / verifies phux-8v1 (duplicated
//! characters on real-terminal resize) WITHOUT tmux.
//!
//! Why not tmux: the `scripts/tui-probe.sh` harness drives `phux attach`
//! inside a tmux pane, but tmux reflows its own grid on resize and
//! reinterprets phux's byte stream, so it masks exactly the artifact
//! phux-8v1 is about. This harness instead allocates a real pseudoterminal,
//! runs `phux attach` in it, and feeds the bytes phux paints into a
//! *persistent* libghostty oracle that we resize at the same logical
//! instant phux receives SIGWINCH — faithfully modelling a real terminal's
//! reflow-then-repaint sequence. If phux's full-frame repaint fails to
//! clear what the reflow left behind, the oracle's visible grid shows a
//! marker line more than once.
//!
//! Run:  cargo run -p phux --example pty_resize_probe
//! Exit code 0 = no duplication observed, 1 = duplication (bug reproduced).

// Standalone diagnostic harness: it prints a human-readable report to
// stdout and uses test-grade `expect`/`unwrap` on setup failures.
#![allow(
    clippy::print_stdout,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    reason = "standalone diagnostic harness, not library code"
)]

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// A persistent libghostty grid oracle: the stand-in for the host
/// terminal. Feed it the bytes phux paints; resize it when the PTY
/// resizes; read its visible rows back as plain text.
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
            state: RenderState::new().expect("render state"),
            rows: RowIterator::new().expect("row iter"),
            cells: CellIterator::new().expect("cell iter"),
            cols,
            n_rows,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Mimic a host terminal reflowing its grid on resize. A single
    /// `resize()` mirrors `terminal_actor::handle_resize`: the both-axes
    /// shrink that once overflowed libghostty's `resizeCols` (phux-y06) is
    /// fixed by the `libghostty-vt` 0.2.0 engine.
    fn resize(&mut self, cols: u16, n_rows: u16) {
        let _ = self.terminal.resize(cols, n_rows, 0, 0);
        self.cols = cols;
        self.n_rows = n_rows;
    }

    fn visible_rows(&mut self) -> Vec<String> {
        let Ok(snapshot) = self.state.update(&self.terminal) else {
            return vec![];
        };
        let total = snapshot.rows().unwrap_or(self.n_rows);
        let Ok(mut row_iter) = self.rows.update(&snapshot) else {
            return vec![];
        };
        let mut out = Vec::new();
        let mut idx = 0u16;
        while let Some(row) = row_iter.next() {
            if idx >= total {
                break;
            }
            let mut buf = String::new();
            if let Ok(mut cells) = self.cells.update(row) {
                while let Some(cell) = cells.next() {
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

/// Count, per `ROWnn-` marker, how many visible rows *start* a fresh
/// occurrence of it. A marker that wrapped is still one logical line, so
/// we collapse the grid into a single string and count substring hits —
/// duplication shows up as a count > the number we actually printed.
fn marker_counts(rows: &[String], markers: &[String]) -> Vec<(String, usize)> {
    let joined = rows.join("\n");
    markers
        .iter()
        .map(|m| (m.clone(), joined.matches(m.as_str()).count()))
        .collect()
}

fn main() {
    let repo = env!("CARGO_MANIFEST_DIR"); // crates/phux
    let phux_bin = format!("{repo}/../../target/debug/phux");
    let sock_dir = "/tmp/phux-pty-probe";
    let sock = format!("{sock_dir}/phux.sock");
    let _ = std::fs::create_dir_all(sock_dir);
    let _ = std::fs::remove_file(&sock);
    // Best-effort: kill any stale server on this socket.
    let _ = std::process::Command::new("pkill")
        .args(["-f", &sock])
        .status();

    let (init_cols, init_rows) = (100u16, 24u16);

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: init_rows,
            cols: init_cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(&phux_bin);
    cmd.args(["attach", "--socket", &sock, "probe"]);
    cmd.cwd("/tmp");
    cmd.env("SHELL", "/bin/sh");
    cmd.env("TERM", "xterm-256color");
    cmd.env("RUST_LOG", "off");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn phux attach");
    drop(pair.slave);

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut reader = pair.master.try_clone_reader().expect("reader");
    {
        let buf = Arc::clone(&buf);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.lock().unwrap().extend_from_slice(&chunk[..n]),
                }
            }
        });
    }
    let mut writer = pair.master.take_writer().expect("writer");

    // Let auto-spawn + attach + first paint settle.
    std::thread::sleep(Duration::from_millis(2500));

    // Print 5 marker lines, each ~71 cols — wraps at 50, not at 100/110.
    let markers: Vec<String> = (1..=5).map(|i| format!("ROW{i:02}-")).collect();
    let payload = "printf 'ROW%02d-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\\n' 1 2 3 4 5\r";
    writer.write_all(payload.as_bytes()).expect("write payload");
    writer.flush().ok();
    std::thread::sleep(Duration::from_millis(1200));

    // Build the oracle and replay everything painted so far.
    let mut oracle = Oracle::new(init_cols, init_rows);
    let cut1 = {
        let b = buf.lock().unwrap();
        oracle.write(&b);
        b.len()
    };
    let before = oracle.visible_rows();

    // --- NARROW to 50x24: reflow the oracle, then let phux repaint. ----
    let narrow = 50u16;
    pair.master
        .resize(PtySize {
            rows: init_rows,
            cols: narrow,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize narrow");
    oracle.resize(narrow, init_rows);
    std::thread::sleep(Duration::from_millis(1500));
    let (seg_narrow, cut2) = {
        let b = buf.lock().unwrap();
        (b[cut1..].to_vec(), b.len())
    };
    oracle.write(&seg_narrow);
    let after_narrow = oracle.visible_rows();
    let narrow_has_ed3 = contains_seq(&seg_narrow, b"\x1b[3J");
    let _ = std::fs::write("/tmp/phux-seg-narrow.esc", escape(&seg_narrow));

    // --- WIDEN to 110x24. --------------------------------------------
    let wide = 110u16;
    pair.master
        .resize(PtySize {
            rows: init_rows,
            cols: wide,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize wide");
    oracle.resize(wide, init_rows);
    std::thread::sleep(Duration::from_millis(1500));
    let seg_wide = {
        let b = buf.lock().unwrap();
        b[cut2..].to_vec()
    };
    oracle.write(&seg_wide);
    let after_wide = oracle.visible_rows();

    // --- teardown ----------------------------------------------------
    let _ = child.kill();
    let _ = std::process::Command::new("pkill")
        .args(["-f", &sock])
        .status();

    // --- report ------------------------------------------------------
    let dump = |label: &str, rows: &[String]| {
        println!("\n===== {label} ({} rows) =====", rows.len());
        for (i, r) in rows.iter().enumerate() {
            if !r.is_empty() {
                println!("{i:2}| {r}");
            }
        }
    };
    dump("BEFORE RESIZE (100x24)", &before);
    dump("AFTER NARROW (50x24)", &after_narrow);
    dump("AFTER WIDEN (110x24)", &after_wide);

    println!("\n===== marker duplication check =====");
    println!("(printed each ROWnn- marker exactly ONCE)");
    let mut bug = false;
    for (label, rows) in [
        ("after-narrow", &after_narrow),
        ("after-widen", &after_wide),
    ] {
        for (m, c) in marker_counts(rows, &markers) {
            let flag = if c > 1 { " <-- DUPLICATED" } else { "" };
            if c > 1 {
                bug = true;
            }
            println!("  [{label}] {m} x{c}{flag}");
        }
    }
    println!(
        "\nphux's narrow-repaint segment contained ED3 (\\x1b[3J, clear scrollback): {narrow_has_ed3}"
    );

    if bug {
        println!("\nRESULT: DUPLICATION REPRODUCED (phux-8v1 present).");
        std::process::exit(1);
    } else {
        println!("\nRESULT: no duplication — frame coherent across resize.");
    }
}

fn contains_seq(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Render bytes with ESC shown as `\e` and other control bytes as `\xNN`
/// so a repaint segment is human-readable.
fn escape(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            0x1b => s.push_str("\\e"),
            b'\n' => s.push_str("\\n\n"),
            b'\r' => s.push_str("\\r"),
            0x20..=0x7e => s.push(b as char),
            _ => {
                let _ = write!(s, "\\x{b:02x}");
            }
        }
    }
    s
}
