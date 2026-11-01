//! The threads and processes around the daemon: accepting helpers, reading
//! and writing their sockets, and asking `phux` for panes.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{Sender, channel};

use super::Event;
use crate::pane::{HELLO, INPUT, RESIZE, parse_hello, read_frame};

pub(super) fn spawn_acceptor(listener: UnixListener, tx: Sender<Event>) {
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || serve_pane(stream, &tx));
        }
    });
}

/// Reads one helper's frames until it goes.
fn serve_pane(mut stream: UnixStream, tx: &Sender<Event>) {
    let Ok((HELLO, body)) = read_frame(&mut stream) else {
        return;
    };
    let Some([pane, terminal, cols, rows]) = parse_hello(&body) else {
        return;
    };
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let _ = tx.send(Event::Connected {
        pane,
        terminal,
        cols: cols as usize,
        rows: rows as usize,
        stream: clone,
    });
    while let Ok((kind, body)) = read_frame(&mut stream) {
        let event = match kind {
            INPUT => Event::Input(pane, body),
            RESIZE => {
                let text = String::from_utf8_lossy(&body);
                let mut f = text.split(' ').filter_map(|n| n.parse().ok());
                let (Some(c), Some(r)) = (f.next(), f.next()) else {
                    continue;
                };
                Event::Resized(pane, c, r)
            }
            _ => continue,
        };
        let _ = tx.send(event);
    }
    let _ = tx.send(Event::Gone(pane));
}

/// One writer per pane, so a stalled terminal stalls only itself.
pub(super) fn spawn_writer(mut stream: UnixStream) -> Sender<Vec<u8>> {
    let (tx, rx) = channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for frame in rx {
            if stream.write_all(&frame).is_err() {
                return;
            }
        }
    });
    tx
}

/// `phux spawn` beside `anchor`, and `phux swap-pane` for a window Neovim
/// put before it: spawn only opens right of or below.
pub(super) fn spawn_pane(
    anchor: u32,
    side_by_side: bool,
    before: bool,
    socket: &Path,
    id: u32,
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = format!("@{anchor}");
    let output = Command::new("phux")
        .args(["spawn", "--json", "--target", &target, "--split"])
        .arg(if side_by_side {
            "horizontal"
        } else {
            "vertical"
        })
        .arg("--")
        .arg(exe)
        .arg("pane")
        .arg(socket)
        .arg(id.to_string())
        .output()
        .map_err(|e| format!("phux spawn: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    if before {
        let doc: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        let new = doc["terminal_id"]
            .as_u64()
            .ok_or_else(|| format!("no terminal_id in {doc}"))?;
        let swapped = Command::new("phux")
            .args(["swap-pane", &target, &format!("@{new}")])
            .output()
            .map_err(|e| format!("phux swap-pane: {e}"))?;
        if !swapped.status.success() {
            // the window still works; it just sits on the other side
            eprintln!(
                "phux-nvim: swap-pane: {}",
                String::from_utf8_lossy(&swapped.stderr)
            );
        }
    }
    Ok(())
}
