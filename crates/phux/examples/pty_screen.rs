//! `pty_screen` — see what phux paints, with NO tmux in the loop.
//!
//! phux is a tmux *replacement*, so driving it inside tmux is circular —
//! and tmux reinterprets phux's byte stream, masking the very rendering
//! bugs we care about. Instead this allocates a raw pseudoterminal
//! ourselves, runs `phux attach` on it, and parses the bytes phux paints
//! through a libghostty `Terminal` — the same parse a real terminal does.
//! The resulting grid IS what a human would see on screen.
//!
//! Usage:
//!   cargo run -p phux --example pty_screen [-- COLSxROWS] [keystrokes...]
//! Each trailing arg is fed to the PTY as input, with "\r" for Enter and
//! "\x01" available for C-a (the prefix). Example:
//!   cargo run -p phux --example pty_screen -- 80x24 "echo hi\r" "\x01" "-"

#![allow(
    clippy::print_stdout,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::doc_markdown,
    reason = "standalone diagnostic harness, not library code"
)]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn render(bytes: &[u8], cols: u16, rows: u16) -> (Vec<String>, Option<(u16, u16)>) {
    let mut term = {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("terminal");
        terminal
            .set_scrollback_max_lines(Some(200))
            .expect("terminal");
        terminal
    };
    term.vt_write(bytes);
    let mut state = RenderState::new().expect("rs");
    let mut row_iter = RowIterator::new().expect("rows");
    let mut cell_iter = CellIterator::new().expect("cells");
    let Ok(snap) = state.update(&term) else {
        return (vec![], None);
    };
    let cursor = snap.cursor_viewport().ok().flatten().map(|c| (c.x, c.y));
    let total = snap.rows().unwrap_or(rows);
    let Ok(mut ri) = row_iter.update(&snap) else {
        return (vec![], cursor);
    };
    let mut out = Vec::new();
    let mut idx = 0u16;
    while let Some(row) = ri.next() {
        if idx >= total {
            break;
        }
        let mut buf = String::new();
        if let Ok(mut ci) = cell_iter.update(row) {
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
    (out, cursor)
}

/// Decode "\r", "\x01", "\n", "\t" escapes in a keystroke arg.
fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('x') => {
                let hi = chars.next().unwrap_or('0');
                let lo = chars.next().unwrap_or('0');
                let hex: String = [hi, lo].iter().collect();
                out.push(u8::from_str_radix(&hex, 16).unwrap_or(0));
            }
            Some(other) => out.push(other as u8),
            None => out.push(b'\\'),
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cols = 80u16;
    let mut rows = 24u16;
    let mut keys: Vec<String> = Vec::new();
    for a in args {
        if let Some((c, r)) = a.split_once('x')
            && let (Ok(c), Ok(r)) = (c.parse(), r.parse())
        {
            cols = c;
            rows = r;
        } else {
            keys.push(a);
        }
    }

    let repo = env!("CARGO_MANIFEST_DIR");
    let phux_bin = format!("{repo}/../../target/debug/phux");
    let sock = "/tmp/phux-screen/phux.sock".to_string();
    let _ = std::fs::create_dir_all("/tmp/phux-screen");
    let _ = std::fs::remove_file(&sock);
    let _ = std::process::Command::new("pkill")
        .args(["-f", &sock])
        .status();

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(&phux_bin);
    cmd.args(["attach", "--socket", &sock, "screen"]);
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
    for k in &keys {
        writer.write_all(&unescape(k)).expect("write key");
        writer.flush().ok();
        std::thread::sleep(Duration::from_millis(700));
    }
    std::thread::sleep(Duration::from_millis(400));

    let bytes = buf.lock().unwrap().clone();
    let _ = child.kill();
    let _ = std::process::Command::new("pkill")
        .args(["-f", &sock])
        .status();

    let (grid, cursor) = render(&bytes, cols, rows);
    let bar = "─".repeat(usize::from(cols));
    println!("┌{bar}┐");
    for r in &grid {
        let pad = usize::from(cols).saturating_sub(r.chars().count());
        println!("│{r}{}│", " ".repeat(pad));
    }
    println!("└{bar}┘");
    println!("{cols}x{rows}  cursor={cursor:?}  (no tmux — raw PTY + libghostty)");
}
