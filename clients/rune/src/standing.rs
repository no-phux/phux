//! Standing mirrors on macOS (`docs/resource-kinds.md#standing-mirrors`).
//!
//! A macOS server's children cannot hold Screen Recording access, so the
//! server only keeps each `[[mirrors]]` session and Rune keeps `phux agent
//! surface standing` running as its child: the mirrors are Rune's, and so
//! are the grant prompts. One runner per server; a second Rune's runner
//! exits at once and retries after a backoff.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The CLI and local server socket the runner serves; the newest wins.
static TARGET: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);

/// Run the mirrors for the local server at `socket` with `phux`, on macOS.
/// Tests hand in fake CLIs, which must not be run as the mirrors.
pub fn follow(phux: Option<&PathBuf>, socket: &Path) {
    if !cfg!(target_os = "macos") || cfg!(test) {
        return;
    }
    let Some(phux) = phux else { return };
    if let Ok(mut target) = TARGET.lock() {
        *target = Some((phux.clone(), socket.to_path_buf()));
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(supervise);
    });
}

fn current() -> Option<(PathBuf, PathBuf)> {
    TARGET.lock().ok().and_then(|target| target.clone())
}

fn supervise() {
    let mut delay = Duration::from_secs(1);
    loop {
        let Some(target) = current() else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let (phux, socket) = &target;
        let started = Instant::now();
        let child = Command::new(phux)
            .arg("--socket")
            .arg(socket)
            .args(["agent", "surface", "standing", "--parent-pid"])
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            while matches!(child.try_wait(), Ok(None)) {
                if current().as_ref() != Some(&target) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        if started.elapsed() >= Duration::from_secs(60) {
            delay = Duration::from_secs(1);
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}
