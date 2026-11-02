//! Process-level `[[startup]]` checks: upgrade restarts the sidecar, and a
//! server killed with SIGKILL is reaped by the next start. A leader that
//! honors `SIGTERM` dies with the server and must not leave a grandchild.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(
    clippy::panic,
    reason = "the shared server harness panics on a failed wait"
)]

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use common::{ServerGuard, XdgDirs};

const POLL: Duration = Duration::from_millis(50);

fn alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) == 0 }
}

fn poll_until(limit: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        thread::sleep(POLL);
    }
    pred()
}

// Shell redirection creates the file before echo publishes its PID.
fn pid_ready(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.ends_with('\n') && text.trim().parse::<i32>().is_ok_and(|pid| pid > 0)
}

#[test]
fn pid_readiness_requires_a_complete_positive_pid_record() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pid");
    assert!(!pid_ready(&path));
    for (text, ready) in [
        ("", false),
        ("42", false),
        ("\n", false),
        ("0\n", false),
        ("-42\n", false),
        ("not-a-pid\n", false),
        ("42\n", true),
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(path.is_file());
        assert_eq!(pid_ready(&path), ready, "{text:?}");
    }
}

// The fixture writes fds last, after env, cwd and umask are complete.
fn wait_for_sidecar_files(root: &Path, pid: i32) {
    let fds = root.join(format!("fds.{pid}"));
    assert!(
        poll_until(Duration::from_secs(20), || {
            std::fs::read_to_string(&fds)
                .is_ok_and(|text| text.ends_with('\n') && !text.trim().is_empty())
        }),
        "sidecar wrote its pid before fds.{pid}"
    );
}

fn write_plugin(root: &Path, script: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("run.sh"), script).unwrap();
    let manifest = root.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        format!(
            "id = \"example.startup\"\nname = \"Startup\"\nversion = \"1.2.3\"\n\
             min_phux_version = \"0.0.2\"\n\
             [[startup]]\nid = \"web\"\ncommand = [\"/bin/sh\", \"{}\"]\n",
            root.join("run.sh").display()
        ),
    )
    .unwrap();
    manifest
}

fn install_config(xdg: &XdgDirs, manifest: &Path) {
    let dir = xdg.config.join("phux");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
            manifest.display()
        ),
    )
    .unwrap();
}

fn start_server(xdg: &XdgDirs, tag: &str) -> ServerGuard {
    ServerGuard::builder(tag)
        .env("XDG_CONFIG_HOME", &xdg.config)
        .env("XDG_STATE_HOME", &xdg.state)
        .env("PHUX_TERMINAL_ID", "99")
        .env("PHUX_SESSION", "from-pane")
        .env("PHUX_PANE", "leftover")
        .idle_secs(600)
        .start()
}

struct Reap(i32);
impl Drop for Reap {
    fn drop(&mut self) {
        if self.0 > 0 {
            unsafe { libc::kill(self.0, libc::SIGKILL) };
        }
    }
}

#[test]
fn upgrade_restarts_the_sidecar_with_a_scrubbed_environment() {
    let root = tempfile::tempdir().unwrap();
    let manifest = write_plugin(
        root.path(),
        "echo $$ >> \"$PHUX_PLUGIN_ROOT/pids\"\n\
         env | sort > \"$PHUX_PLUGIN_ROOT/env.$$\"\n\
         pwd > \"$PHUX_PLUGIN_ROOT/cwd.$$\"\n\
         umask > \"$PHUX_PLUGIN_ROOT/umask.$$\"\n\
         bad=0\n\
         for f in /proc/$$/fd/*; do\n\
           t=$(readlink \"$f\" 2>/dev/null || true)\n\
           case \"$t\" in socket:*) bad=1 ;; esac\n\
         done\n\
         echo \"$bad\" > \"$PHUX_PLUGIN_ROOT/fds.$$\"\n\
         sleep 1000\n",
    );
    let xdg = XdgDirs::fresh("startup-upgrade");
    install_config(&xdg, &manifest);
    let server = start_server(&xdg, "stup");
    let pids = root.path().join("pids");
    assert!(
        poll_until(Duration::from_secs(20), || pid_ready(&pids)),
        "sidecar did not start"
    );
    let first: i32 = std::fs::read_to_string(&pids)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut reap = Reap(first);
    wait_for_sidecar_files(root.path(), first);
    let env = std::fs::read_to_string(root.path().join(format!("env.{first}"))).unwrap();
    assert!(env.contains("PHUX_PLUGIN_ID=example.startup"), "{env}");
    assert!(env.contains("PHUX_PLUGIN_NAME=Startup"), "{env}");
    assert!(env.contains("PHUX_PLUGIN_VERSION=1.2.3"), "{env}");
    assert!(
        env.contains(&format!("PHUX_SOCKET={}", server.socket.display())),
        "{env}"
    );
    assert!(
        !env.lines()
            .any(|line| line.starts_with("PHUX_TERMINAL_ID=")),
        "{env}"
    );
    assert!(
        !env.lines().any(|line| line.starts_with("PHUX_SESSION=")),
        "{env}"
    );
    assert!(
        !env.lines().any(|line| line.starts_with("PHUX_PANE=")),
        "{env}"
    );
    let umask = std::fs::read_to_string(root.path().join(format!("umask.{first}"))).unwrap();
    assert!(umask.contains("0077"), "{umask}");
    let cwd = std::fs::read_to_string(root.path().join(format!("cwd.{first}"))).unwrap();
    assert_eq!(
        std::fs::canonicalize(cwd.trim()).unwrap(),
        std::fs::canonicalize(root.path()).unwrap()
    );
    let fds = std::fs::read_to_string(root.path().join(format!("fds.{first}"))).unwrap();
    assert_eq!(fds.trim(), "0", "sidecar inherited a socket fd: {fds}");

    let upgrade = server
        .cmd(&["upgrade"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run upgrade");
    assert_eq!(
        upgrade.status.code(),
        Some(0),
        "upgrade should ack: {}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    assert!(
        poll_until(Duration::from_secs(30), || {
            let text = std::fs::read_to_string(&pids).unwrap_or_default();
            text.ends_with('\n')
                && text.lines().filter(|line| !line.is_empty()).count() >= 2
                && !alive(first)
        }),
        "upgrade should replace the sidecar; pids={:?} first alive={}",
        std::fs::read_to_string(&pids),
        alive(first)
    );
    let second: i32 = std::fs::read_to_string(&pids)
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    reap.0 = second;
    assert!(alive(second));
    assert_ne!(first, second);
    wait_for_sidecar_files(root.path(), second);
    let env = std::fs::read_to_string(root.path().join(format!("env.{second}"))).unwrap();
    assert!(
        env.contains(&format!("PHUX_SOCKET={}", server.socket.display())),
        "the new sidecar should dial the resumed server\n{env}"
    );
    drop(reap);
}

#[test]
fn sigkill_of_the_server_is_reaped_by_the_next_start() {
    let root = tempfile::tempdir().unwrap();
    let manifest = write_plugin(
        root.path(),
        "trap '' TERM\n\
         echo $$ > \"$PHUX_PLUGIN_ROOT/pid\"\n\
         sleep 1000\n",
    );
    let xdg = XdgDirs::fresh("startup-reap");
    install_config(&xdg, &manifest);
    let mut server = start_server(&xdg, "sreap");
    let pid_path = root.path().join("pid");
    assert!(poll_until(Duration::from_secs(20), || pid_ready(&pid_path)));
    let pid: i32 = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let reap = Reap(pid);
    server.sigkill();
    assert!(
        alive(pid),
        "a SIGTERM-ignoring sidecar must survive SIGKILL of the server"
    );
    let next = start_server(&xdg, "sreap2");
    assert!(
        poll_until(Duration::from_secs(20), || !alive(pid)),
        "the next server start should reap the orphan"
    );
    drop(next);
    drop(reap);
}

#[test]
fn sigkill_reaps_a_grandchild_when_the_leader_honors_term() {
    let root = tempfile::tempdir().unwrap();
    let manifest = write_plugin(
        root.path(),
        "echo $$ > \"$PHUX_PLUGIN_ROOT/pid\"\n\
         sleep 0.25\n\
         sleep 1000 &\n\
         echo $! > \"$PHUX_PLUGIN_ROOT/grand\"\n\
         wait\n",
    );
    let xdg = XdgDirs::fresh("startup-grand");
    install_config(&xdg, &manifest);
    let mut server = start_server(&xdg, "sgrand");
    let grand_path = root.path().join("grand");
    assert!(
        poll_until(Duration::from_secs(20), || pid_ready(&grand_path)),
        "sidecar did not fork a grandchild"
    );
    let leader: i32 = std::fs::read_to_string(root.path().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let grand: i32 = std::fs::read_to_string(&grand_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let reap_leader = Reap(leader);
    let reap_grand = Reap(grand);
    server.sigkill();
    // Only Linux delivers PDEATHSIG. On macOS both processes remain for
    // the next start's orphan reaper, which is the cross-platform contract.
    #[cfg(target_os = "linux")]
    {
        assert!(
            poll_until(Duration::from_secs(20), || !alive(leader)),
            "PDEATHSIG should terminate a leader that honors SIGTERM"
        );
        assert!(
            alive(grand),
            "the grandchild should survive the leader's SIGTERM"
        );
    }
    let next = start_server(&xdg, "sgrand2");
    assert!(
        poll_until(Duration::from_secs(20), || !alive(leader) && !alive(grand)),
        "the next server start should reap the leftover grandchild"
    );
    drop(next);
    drop(reap_grand);
    drop(reap_leader);
}

#[test]
fn sigkill_reaps_a_grandchild_forked_in_the_leaders_start_tick() {
    let root = tempfile::tempdir().unwrap();
    let manifest = write_plugin(
        root.path(),
        "echo $$ > \"$PHUX_PLUGIN_ROOT/pid\"\n\
         sleep 1000 &\n\
         echo $! > \"$PHUX_PLUGIN_ROOT/grand\"\n\
         wait\n",
    );
    let xdg = XdgDirs::fresh("startup-same-tick");
    install_config(&xdg, &manifest);
    let mut server = start_server(&xdg, "stick");
    let grand_path = root.path().join("grand");
    assert!(
        poll_until(Duration::from_secs(20), || pid_ready(&grand_path)),
        "sidecar did not fork a grandchild"
    );
    let leader: i32 = std::fs::read_to_string(root.path().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let grand: i32 = std::fs::read_to_string(&grand_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let reap_leader = Reap(leader);
    let reap_grand = Reap(grand);
    server.sigkill();
    #[cfg(target_os = "linux")]
    {
        assert!(
            poll_until(Duration::from_secs(20), || !alive(leader)),
            "PDEATHSIG should terminate a leader that honors SIGTERM"
        );
        assert!(
            alive(grand),
            "an immediately forked grandchild should survive the leader"
        );
    }
    let next = start_server(&xdg, "stick2");
    assert!(
        poll_until(Duration::from_secs(20), || !alive(leader) && !alive(grand)),
        "the next start should reap a same-tick grandchild"
    );
    drop(next);
    drop(reap_grand);
    drop(reap_leader);
}
