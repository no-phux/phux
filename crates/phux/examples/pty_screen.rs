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
    clippy::panic,
    clippy::too_many_lines,
    clippy::doc_markdown,
    reason = "standalone diagnostic harness, not library code"
)]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
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

/// Own the private server even if the diagnostic panics midway through capture.
struct OwnedServer(Child);

impl Drop for OwnedServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_server(server: &mut Child, socket: &Path) {
    for _ in 0..200 {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return;
        }
        assert!(
            server.try_wait().expect("server status").is_none(),
            "private server exited before binding"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("private server did not bind {}", socket.display());
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

    let repo = std::env::var_os("CARGO_MANIFEST_DIR").expect("run through cargo");
    let phux_bin = std::env::var_os("PHUX").map_or_else(
        || std::path::PathBuf::from(repo).join("../../target/debug/phux"),
        std::path::PathBuf::from,
    );
    let work = tempfile::tempdir().expect("private diagnostic directory");
    let sock = work.path().join("sock");
    std::fs::create_dir(work.path().join("home")).expect("private home");
    let env = [
        ("HOME", work.path().join("home").into_os_string()),
        (
            "XDG_CONFIG_HOME",
            work.path().join("config").into_os_string(),
        ),
        ("XDG_STATE_HOME", work.path().join("state").into_os_string()),
        ("XDG_DATA_HOME", work.path().join("data").into_os_string()),
        ("PHUX_SOCKET", sock.clone().into_os_string()),
        ("PHUX_PROFILE", "pty-screen".into()),
        ("PHUX_NO_UPDATE_CHECK", "1".into()),
        ("PATH", "/usr/bin:/bin".into()),
        ("SHELL", "/bin/sh".into()),
        ("TERM", "xterm-256color".into()),
    ];
    let mut server = OwnedServer(
        std::process::Command::new(&phux_bin)
            .args(["server", "--session", "screen", "--exit-after-idle", "60"])
            .arg("--socket")
            .arg(&sock)
            .env_clear()
            .envs(env.iter().map(|(key, value)| (*key, value)))
            .current_dir(work.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn private server"),
    );
    wait_for_server(&mut server.0, &sock);

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
    cmd.args(["attach", "--socket"]);
    cmd.arg(&sock);
    cmd.arg("screen");
    cmd.cwd(work.path());
    cmd.env_clear();
    for (key, value) in &env {
        cmd.env(key, value);
    }
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
    let _ = child.wait();

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
