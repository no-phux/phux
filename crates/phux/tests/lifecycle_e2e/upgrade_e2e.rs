//! Graceful server upgrade (ADR-0032): after a real in-place `execve`, the
//! server PID is unchanged, the pane's child is alive, its scrollback
//! survived, and the pane still has its normal kill/reap lifecycle.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SESSION: &str = "work";
const POLL: Duration = Duration::from_millis(50);

struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    /// Spawn `phux server` with a seed pane running `seed_command`, then block
    /// until the socket appears.
    fn start_with_seed(seed_command: &str) -> Self {
        Self(
            common::ServerGuard::builder("upg")
                .seed_command(seed_command)
                .start(),
        )
    }

    fn status(&self, args: &[&str]) -> i32 {
        self.cmd(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run verb")
            .code()
            .expect("exited normally")
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self
            .cmd(args)
            .stderr(Stdio::null())
            .output()
            .expect("verb output");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `pid` is alive iff `kill -0 pid` succeeds (POSIX; no `libc` dependency,
/// which is macOS-gated in the server crate anyway).
fn alive(pid: u32) -> bool {
    common::process_exists(pid)
}

/// The first direct child PID of `parent`, via `pgrep -P`.
fn child_of(parent: u32) -> Option<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
}

fn poll<F: FnMut() -> bool>(deadline: Duration, mut f: F) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn child_and_scrollback_survive_graceful_upgrade() {
    let marker = format!("UPGRADE_SURVIVES_{}", std::process::id());
    // The seed pane prints the marker, then `exec`s a long-lived `sleep` — so
    // the pane's child is a stable, observable process (the shell's pid is
    // preserved across its own exec).
    let server = ServerGuard::start_with_seed(&format!("printf '{marker}\\n'; exec sleep 600"));
    let server_pid = server.pid();

    // The pane child (the sleep) must come up, and the marker must reach the
    // grid (observable via `wait --until`).
    let child_pid = {
        let mut found = None;
        assert!(
            poll(Duration::from_secs(10), || {
                found = child_of(server_pid);
                found.is_some()
            }),
            "the seed pane's child should spawn"
        );
        found.unwrap()
    };
    assert_eq!(
        server.status(&["wait", SESSION, "--until", &marker, "--timeout", "10"]),
        0,
        "the marker should appear on the pane before upgrade"
    );

    // Trigger the graceful upgrade.
    assert_eq!(server.status(&["upgrade"]), 0, "`phux upgrade` should ack");

    // The decisive check: the server re-execed IN PLACE, so its PID survives.
    // A kill+restart would not preserve it.
    assert!(
        poll(Duration::from_secs(10), || alive(server_pid)),
        "the server process (pid {server_pid}) must survive the in-place execve"
    );
    // The pane's child survived the upgrade with its master fd intact.
    assert!(
        alive(child_pid),
        "the pane's child (pid {child_pid}) must survive the upgrade"
    );

    // The resumed server rebuilt the tree + replayed the snapshot: reconnect
    // and confirm the session is back and the marker survived.
    assert!(
        poll(Duration::from_secs(15), || server.status(&["ls"]) == 0),
        "the resumed server should accept connections again"
    );
    let snap = server.stdout(&["snapshot", SESSION]);
    assert!(
        snap.contains(&marker),
        "the pane's scrollback marker should survive the upgrade; got:\n{snap}"
    );

    // A kill of the resumed session leaves the authoritative list at once
    // (its reply follows the committed teardown, L1 §5.2), and its child then
    // dies inside the pane-kill grace, so the child is awaited, not assumed
    // dead the moment `ls` stops listing it. This harness never attaches a
    // client, so the server intentionally stays alive after becoming empty.
    assert_eq!(
        server.status(&["kill", "--yes", SESSION]),
        0,
        "the resumed session should accept a kill"
    );
    assert!(
        poll(Duration::from_secs(10), || !server
            .stdout(&["ls"])
            .contains(SESSION)),
        "the killed resumed session should be reaped from the authoritative list"
    );
    assert!(
        poll(Duration::from_secs(10), || !alive(child_pid)),
        "killing the resumed session should terminate its pane child"
    );
}

/// One `--json` verb's document from `server`.
fn json_of(server: &ServerGuard, args: &[&str]) -> serde_json::Value {
    let out = server.stdout(args);
    serde_json::from_str(&out).unwrap_or_else(|err| panic!("{args:?} JSON ({err}): {out}"))
}

/// phux-xy9y: a pane's program title (OSC 2) never becomes its user-set
/// title across an upgrade. `GET_STATE`'s `title` (via `resource show`)
/// stays unset, and `GET_SCREEN`'s OSC `title` (via `snapshot --json`)
/// still reports what the program set.
#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn program_title_stays_out_of_the_user_title_across_upgrade() {
    let title = format!("OSC_TITLE_{}", std::process::id());
    let marker = format!("TITLE_READY_{}", std::process::id());
    let server = ServerGuard::start_with_seed(&format!(
        "printf '\\033]2;{title}\\007{marker}\\n'; exec sleep 600"
    ));
    assert_eq!(
        server.status(&["wait", SESSION, "--until", &marker, "--timeout", "10"]),
        0,
        "the seed pane should print its marker"
    );
    assert!(
        poll(Duration::from_secs(10), || {
            json_of(&server, &["snapshot", SESSION, "--json"])["title"] == title.as_str()
        }),
        "the program title should be live before the upgrade"
    );
    assert!(
        json_of(&server, &["resource", "show", SESSION, "--json"])["title"].is_null(),
        "no one named the pane before the upgrade"
    );

    assert_eq!(server.status(&["upgrade"]), 0, "`phux upgrade` should ack");
    assert!(
        poll(Duration::from_secs(15), || server.status(&["ls"]) == 0),
        "the resumed server should accept connections again"
    );

    let shown = json_of(&server, &["resource", "show", SESSION, "--json"]);
    assert!(
        shown["title"].is_null(),
        "the upgrade must not promote the program title to a user title; got {shown}"
    );
    let screen = json_of(&server, &["snapshot", SESSION, "--json"]);
    assert_eq!(
        screen["title"],
        title.as_str(),
        "the resumed engine should still report the program title"
    );
}

/// phux-1x9s.3: an `AgentSession` crosses the upgrade under the same `@N`
/// with its record log, so an integration's handle keeps working: the log
/// replays the pre-upgrade record, and the next emit continues its sequence.
#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn agent_session_and_its_log_survive_graceful_upgrade() {
    let marker = format!("AGENT_LOG_SURVIVES_{}", std::process::id());
    let server = ServerGuard::start_with_seed("exec sleep 600");
    let server_pid = server.pid();
    assert!(
        poll(Duration::from_secs(10), || server.status(&["ls"]) == 0),
        "the server should accept connections"
    );

    let agent = server
        .stdout(&[
            "agent",
            "session",
            "open",
            SESSION,
            "--provider",
            "upgrade-e2e",
            "--native-id",
            "native-upg",
        ])
        .trim()
        .to_owned();
    assert!(
        agent.starts_with('@'),
        "open prints the session id: {agent:?}"
    );
    let data = format!("{{\"marker\":\"{marker}\"}}");
    assert_eq!(
        server.status(&["agent", "emit", &agent, "--type", "prompt", "--data", &data]),
        0,
        "the pre-upgrade record is accepted"
    );

    assert_eq!(server.status(&["upgrade"]), 0, "`phux upgrade` should ack");
    assert!(
        poll(Duration::from_secs(10), || alive(server_pid)),
        "the server process must survive the in-place execve"
    );
    assert!(
        poll(Duration::from_secs(15), || server.status(&["ls"]) == 0),
        "the resumed server should accept connections again"
    );

    let log = server.stdout(&["agent", "log", &agent]);
    assert!(
        log.starts_with("1\t") && log.contains(&marker),
        "`agent log {agent}` should replay the pre-upgrade record; got:\n{log}"
    );
    let next = server.stdout(&[
        "agent", "emit", &agent, "--type", "stop", "--data", "{}", "--json",
    ]);
    let next: serde_json::Value = serde_json::from_str(&next)
        .unwrap_or_else(|err| panic!("emit --json prints a document ({err}): {next}"));
    assert_eq!(
        next["seq"], 2,
        "the resumed session continues its sequence: {next}"
    );
}

/// Write an executable fake Claude named `claude` (the detector identifies
/// the kind from the foreground process name) that holds a plain screen
/// until `trigger` exists, then rings the bell and paints the permission
/// dialog only `rules/claude.toml` reads as `blocked`, and holds.
fn write_gated_claude(dir: &std::path::Path, trigger: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let rule = "\\342\\224\\200".repeat(20);
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "printf '\\033[2J\\033[H'\n",
            "echo 'waiting for the upgrade'\n",
            "while [ ! -e '{trigger}' ]; do sleep 0.1; done\n",
            "printf '\\007\\033[2J\\033[H'\n",
            "echo 'some transcript output above the live chrome'\n",
            "echo ''\n",
            "printf '{rule}\\n'\n",
            "echo ' Bash command'\n",
            "echo ''\n",
            "echo '   touch /tmp/probe.txt'\n",
            "echo ''\n",
            "echo ' Do you want to proceed?'\n",
            "printf ' \\342\\235\\257 1. Yes\\n'\n",
            "echo '   2. Yes, and always allow access'\n",
            "echo '   3. No'\n",
            "echo ''\n",
            "echo ' Esc to cancel'\n",
            "sleep 600\n",
        ),
        trigger = trigger.display(),
        rule = rule,
    );
    let path = dir.join("claude");
    std::fs::write(&path, script).expect("write fake claude");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake claude");
    path
}

/// The detector's `state` for `target`'s agent, or `None` when no agent is
/// reported.
fn agent_state(server: &ServerGuard, target: &str) -> Option<String> {
    let out = server.stdout(&["agent", "show", target, "--json"]);
    let json: serde_json::Value = serde_json::from_str(&out).ok()?;
    json["agents"][0]["state"].as_str().map(str::to_owned)
}

/// phux-1x9s.6: a pane rebuilt by an upgrade is wired exactly as a fresh
/// spawn is. Its agent detector re-arms, so a screen that changes only after
/// the upgrade still moves the agent's state, and its event sink still feeds
/// the journal (`watch --until bell`).
#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn agent_detector_and_pane_events_rearm_after_graceful_upgrade() {
    let dir = tempfile::tempdir().expect("temp dir");
    let trigger = dir.path().join("paint");
    let claude = write_gated_claude(dir.path(), &trigger);
    let server = ServerGuard(
        common::ServerGuard::builder("upg")
            .seed_command(format!("exec '{}'", claude.display()))
            .env("PHUX_AGENT_STARTUP_GRACE_MS", "200")
            .start(),
    );

    // Armed before the upgrade: the idle screen is identified and published.
    assert!(
        poll(Duration::from_secs(20), || agent_state(&server, SESSION)
            .is_some_and(|state| state == "idle")),
        "the detector should publish `idle` before the upgrade; last: {:?}",
        agent_state(&server, SESSION)
    );

    assert_eq!(server.status(&["upgrade"]), 0, "`phux upgrade` should ack");
    assert!(
        poll(Duration::from_secs(15), || server.status(&["ls"]) == 0),
        "the resumed server should accept connections again"
    );

    // A cursor taken after the resume, so only a post-upgrade event matches.
    let first = server
        .cmd(&["watch", SESSION, "--json", "--timeout", "0"])
        .stdout(Stdio::null())
        .output()
        .expect("watch for a cursor");
    let stderr = String::from_utf8_lossy(&first.stderr);
    let tail = stderr.lines().last().unwrap_or_default();
    let cursor = serde_json::from_str::<serde_json::Value>(tail)
        .unwrap_or_else(|err| panic!("watch stderr tail {tail:?}: {err}"))["cursor"]
        .as_str()
        .expect("a journaling server issues a cursor")
        .to_owned();

    std::fs::write(&trigger, b"").expect("let the fake agent paint its dialog");

    assert!(
        poll(Duration::from_secs(20), || agent_state(&server, SESSION)
            .is_some_and(|state| state == "blocked")),
        "the resumed pane's detector should derive `blocked` from the screen \
         painted after the upgrade; last: {:?}",
        agent_state(&server, SESSION)
    );
    assert_eq!(
        server.status(&[
            "watch",
            SESSION,
            "--json",
            "--after",
            &cursor,
            "--until",
            "bell",
            "--timeout",
            "20",
        ]),
        0,
        "the resumed pane's bell should reach the event journal"
    );
}

/// OSC 7501 is terminal state, not a grid property or a transient badge.
#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn program_status_and_inheritance_survive_graceful_upgrade() {
    let marker = format!("STATUS_READY_{}", std::process::id());
    let server = ServerGuard::start_with_seed(&format!(
        "printf '\\033]7501;state=idle:app=build\\007'; \
         printf '\\033]7501;id=build/test:state=blocked:kind=question:progress=42:msg=UmVhZHk/\\033\\\\'; \
         printf '{marker}\\n'; \
         read answer; \
         printf '\\033]7501;state=idle\\007'; \
         exec sleep 600"
    ));
    assert_eq!(
        server.status(&["wait", SESSION, "--until", &marker, "--timeout", "10"]),
        0,
    );
    assert!(poll(Duration::from_secs(10), || {
        json_of(&server, &["resource", "show", SESSION, "--json"])["program_status"]["active"]["state"]
            == "blocked"
    }));
    let before = json_of(&server, &["resource", "show", SESSION, "--json"]);
    assert_eq!(before["program_status"]["active"]["app"], "build");
    assert_eq!(before["program_status"]["active"]["state"], "blocked");
    assert_eq!(before["program_status"]["active"]["progress"], 42);
    assert_eq!(before["program_status"]["active"]["msg"], "Ready?");

    assert_eq!(server.status(&["upgrade"]), 0);
    assert!(
        poll(Duration::from_secs(15), || server.status(&["ls"]) == 0),
        "the resumed server should accept connections again"
    );
    assert!(poll(Duration::from_secs(10), || {
        let shown = json_of(&server, &["resource", "show", SESSION, "--json"]);
        shown["program_status"] == before["program_status"] && shown["agent"]["state"] == "blocked"
    }));
    let after = json_of(&server, &["resource", "show", SESSION, "--json"]);
    assert_eq!(after["program_status"], before["program_status"]);
    assert_eq!(after["agent"]["state"], "blocked");

    assert_eq!(server.status(&["send-keys", SESSION, "go", "Enter"]), 0);
    assert!(
        poll(Duration::from_secs(10), || {
            let shown = json_of(&server, &["resource", "show", SESSION, "--json"]);
            shown["program_status"]["active"]["state"] == "blocked"
                && shown["program_status"]["active"]["app"].is_null()
        }),
        "replacement of the root must remove inherited app after upgrade"
    );
}
