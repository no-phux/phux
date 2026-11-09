//! `phux agent surface standing`: run the `[[mirrors]]` entries as this
//! process's children, each into its `mirror-<target>` session's pane.
//!
//! A macOS server's children cannot hold Screen Recording access (TCC), so
//! there the server only keeps each session with a holding pane and Rune
//! runs this command; the mirrors it starts are Rune's descendants and the
//! grant prompts name Rune (`docs/resource-kinds.md#standing-mirrors`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use phux_config::MirrorConfigEntry;
use phux_config::loader as config_loader;
use phux_server::runtime::{MirrorSlot, default_socket_path};

/// How often the config is re-read and exited mirrors restarted.
const TICK: Duration = Duration::from_secs(2);

/// One mirror this process runs.
#[derive(Default)]
struct Run {
    child: Option<Child>,
    slot: MirrorSlot,
}

impl Run {
    fn alive(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) fn run_standing(socket: Option<PathBuf>) -> ExitCode {
    let socket = socket.unwrap_or_else(default_socket_path);
    let Some(_lock) = lock(&socket) else {
        eprintln!("another `phux agent surface standing` runs this server's mirrors");
        return ExitCode::SUCCESS;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("phux agent surface standing: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut entries = Vec::new();
    let mut runs: BTreeMap<String, Run> = BTreeMap::new();
    loop {
        // ponytail: re-reads the config every tick; follow the server's
        // reload doorbell instead if that ever costs.
        match config_loader::load() {
            Ok(config) => entries = config.mirrors,
            Err(err) => eprintln!("[[mirrors]] unreadable, keeping the running mirrors: {err}"),
        }
        tick(&exe, &socket, &entries, &mut runs, Instant::now());
        std::thread::sleep(TICK);
    }
}

fn tick(
    exe: &Path,
    socket: &Path,
    entries: &[MirrorConfigEntry],
    runs: &mut BTreeMap<String, Run>,
    now: Instant,
) {
    let wanted: BTreeMap<String, Vec<String>> = entries
        .iter()
        .map(|entry| (entry.session_name(), argv(entry, socket)))
        .collect();
    runs.retain(|name, run| {
        let keep = wanted.contains_key(name);
        if !keep {
            run.stop();
        }
        keep
    });
    for (name, args) in wanted {
        let run = runs.entry(name.clone()).or_default();
        let mut alive = run.alive();
        if alive && run.slot.changed(&args) {
            run.stop();
            run.slot = MirrorSlot::default();
            alive = false;
        }
        if !run.slot.due(alive, now) {
            continue;
        }
        run.slot.started(now, args.clone());
        match Command::new(exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
        {
            Ok(child) => run.child = Some(child),
            Err(err) => eprintln!("standing mirror {name} failed to start: {err}"),
        }
    }
}

/// `entry`'s mirror bound to its session's pane, exiting with this process.
fn argv(entry: &MirrorConfigEntry, socket: &Path) -> Vec<String> {
    let mut args = vec!["--socket".to_owned(), socket.to_string_lossy().into_owned()];
    let mut mirror = entry.args();
    let flags = mirror.split_off(3);
    args.extend(mirror);
    args.extend([
        "--pane".to_owned(),
        entry.session_name(),
        "--parent-pid".to_owned(),
        std::process::id().to_string(),
    ]);
    args.extend(flags);
    args
}

/// One runner per server: a second Rune leaves the mirrors to the first.
fn lock(socket: &Path) -> Option<std::fs::File> {
    let mut path = socket.as_os_str().to_owned();
    path.push(".mirrors.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(path))
        .ok()?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).ok()?;
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(target: &str) -> MirrorConfigEntry {
        MirrorConfigEntry {
            target: target.to_owned(),
            format: None,
            quality: None,
            max_width: Some(800),
            interval_ms: None,
        }
    }

    #[test]
    fn a_mirror_binds_to_its_session_and_exits_with_the_runner() {
        let args = argv(&entry("display"), Path::new("/s"));
        let pid = std::process::id().to_string();
        assert_eq!(
            args,
            [
                "--socket",
                "/s",
                "agent",
                "surface",
                "mirror",
                "--pane",
                "mirror-display",
                "--parent-pid",
                &pid,
                "--max-width",
                "800",
                "--",
                "display"
            ]
        );
    }

    #[test]
    fn runs_wanted_mirrors_and_stops_removed_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("s");
        assert!(lock(&socket).is_some_and(|_held| lock(&socket).is_none()));
        let mut runs = BTreeMap::new();
        let exe = Path::new("/bin/sleep");
        let now = Instant::now();
        tick(exe, &socket, &[entry("display")], &mut runs, now);
        // `sleep` refuses these arguments and exits; it restarts after a
        // backoff, not at once.
        let pid = |runs: &BTreeMap<String, Run>| runs["mirror-display"].child.as_ref().map(Child::id);
        let first = pid(&runs).expect("started");
        let _ = runs.get_mut("mirror-display").and_then(|r| r.child.as_mut()).map(Child::wait);
        tick(exe, &socket, &[entry("display")], &mut runs, now + TICK);
        assert_eq!(pid(&runs), Some(first), "backing off");
        tick(exe, &socket, &[entry("display")], &mut runs, now + TICK * 2);
        assert_ne!(pid(&runs), Some(first), "restarted");
        tick(exe, &socket, &[], &mut runs, now + TICK * 3);
        assert!(runs.is_empty());
    }
}
