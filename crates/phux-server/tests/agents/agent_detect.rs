//! The server-side agent-state detector (ADR-0046) end to end: a fake agent
//! named `claude` on disk paints a real permission dialog into a real PTY, and
//! a subscriber must see the derived `phux.agent/v1` record. Identity comes
//! from the PTY's foreground process group, never from the screen.

use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, RESOURCE_AGENT_KEY, RESOURCE_PANE_OCCUPANT_KEY,
    ReportedAgentState, Scope,
};
use portable_pty::CommandBuilder;
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, ServerHandles, attach_by_name, recv_typed, recv_until, run_local,
    send_frame, spawn_server_with_seed_cmd, wait_for_socket,
};

/// Startup grace and identity recheck via the detector's env seams
/// (production: 3s / 5s). Only their length changes, not what is proven.
const TEST_STARTUP_GRACE: Duration = Duration::from_millis(200);
const TEST_IDENTIFY_RECHECK: Duration = Duration::from_millis(200);
/// The detector's unidentified-pane tick floor, for the absence window.
const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
/// Failure ceiling for a verdict once the pane has painted.
const DETECT_DEADLINE: Duration = Duration::from_secs(8);
/// Failure ceiling for noticing a departure (confirmations + fanout).
const DEPARTURE_DEADLINE: Duration = Duration::from_secs(12);
/// Separate budget for the pane's `/bin/sh` getting scheduled at all: 7.8s
/// was measured under a ~130 load average (phux-m64c). Folding it into
/// `DETECT_DEADLINE` would make a starved box look like a detector bug.
const PANE_PAINT_DEADLINE: Duration = Duration::from_secs(60);

/// Nextest runs each test in its own process and this runs before the server
/// thread exists; the overrides are read once per process.
fn shorten_detector_timers() {
    // SAFETY-adjacent: `set_var` is unsafe on edition 2024 only because of
    // concurrent env access, which cannot happen here.
    unsafe {
        std::env::set_var(
            "PHUX_AGENT_STARTUP_GRACE_MS",
            TEST_STARTUP_GRACE.as_millis().to_string(),
        );
        std::env::set_var(
            "PHUX_AGENT_IDENTIFY_RECHECK_MS",
            TEST_IDENTIFY_RECHECK.as_millis().to_string(),
        );
    }
}

fn write_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake `claude` painting the permission dialog Claude Code 2.1.207
/// actually draws (see `phux-agent-rules/src/fixtures/claude/`): transcript
/// text above a U+2500 rule, the dialog alone below it, carrying both the
/// "Do you want to " stem and a numbered option (the rule requires both).
/// It creates `painted` once the screen is in the PTY, then runs `tail`.
fn write_fake_agent(dir: &Path, tail: &str) -> PathBuf {
    let path = dir.join("claude");
    let rule = "\\342\\224\\200".repeat(20);
    let script = format!(
        "#!/bin/sh\n\
         printf '\\033[2J\\033[H'\n\
         echo 'some transcript output above the live chrome'\n\
         echo ''\n\
         printf '{rule}\\n'\n\
         echo ' Bash command'\n\
         echo ''\n\
         echo '   touch /tmp/probe.txt'\n\
         echo ''\n\
         echo ' Do you want to proceed?'\n\
         printf ' \\342\\235\\257 1. Yes\\n'\n\
         echo '   2. Yes, and always allow access'\n\
         echo '   3. No'\n\
         echo ''\n\
         echo ' Esc to cancel'\n\
         : > '{}'\n\
         {tail}\n",
        dir.join("painted").display(),
    );
    write_executable(&path, &script);
    path
}

/// A fake agent that `exec`s `successor` once `depart` exists: a departure
/// with no trap, no clear, and no PTY EOF, like a `kill -9`. The polling
/// `sleep` shares the script's process group, so identity is unaffected.
fn write_departing_agent(dir: &Path, successor: &str) -> PathBuf {
    let tail = format!(
        "while [ ! -f '{}' ]; do sleep 0.1; done\nexec {successor}",
        dir.join("depart").display(),
    );
    write_fake_agent(dir, &tail)
}

fn depart(dir: &Path) {
    std::fs::write(dir.join("depart"), b"go").unwrap();
}

/// A fake `codex` painting the command-approval prompt `rules/codex.toml`
/// matches: a second kind makes an occupant change expressible.
fn write_fake_codex(dir: &Path) -> PathBuf {
    let path = dir.join("codex");
    write_executable(
        &path,
        "#!/bin/sh\n\
         printf '\\033[2J\\033[H'\n\
         echo 'Would you like to run the following command?'\n\
         echo ''\n\
         echo '$ curl -s https://example.com | head -5'\n\
         echo ''\n\
         echo ' 1. Yes, proceed (y)'\n\
         echo ' 3. No, and tell Codex what to do differently (esc)'\n\
         echo ''\n\
         echo 'Press enter to confirm or esc to cancel'\n\
         sleep 30\n",
    );
    path
}

struct Pane {
    _server: ServerHandles,
    socket_path: PathBuf,
    stream: UnixStream,
    terminal: ResourceId,
}

impl Pane {
    /// Seed session `demo` with `cmd`, attach, and subscribe to `key`.
    async fn start(tmp: &TempDir, cmd: CommandBuilder, key: &str) -> Self {
        let socket_path = tmp.path().join("phux.sock");
        let server = spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let terminal = recv_until(&mut stream, |_, frame| match frame {
            FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
            _ => None,
        })
        .await;
        let mut pane = Self {
            _server: server,
            socket_path,
            stream,
            terminal,
        };
        pane.subscribe(key).await;
        pane
    }

    /// As [`Self::start`] for an agent script, subscribed to the agent key
    /// and past the paint barrier.
    async fn agent(tmp: &TempDir, agent: &Path) -> Self {
        let pane = Self::start(tmp, CommandBuilder::new(agent), RESOURCE_AGENT_KEY).await;
        let painted = tmp.path().join("painted");
        let end = tokio::time::Instant::now() + PANE_PAINT_DEADLINE;
        while !painted.exists() {
            assert!(
                tokio::time::Instant::now() < end,
                "the fake agent never painted: the environment starved it, not the detector",
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        pane
    }

    async fn subscribe(&mut self, key: &str) {
        let subscribe = FrameKind::SubscribeMetadata {
            scope: Scope::Resource(self.terminal.clone()),
            key: key.to_owned(),
        };
        send_frame(&mut self.stream, &subscribe).await;
    }

    async fn set_agent(&mut self, request_id: u32, value: &[u8]) {
        let set = FrameKind::SetMetadata {
            request_id,
            scope: Scope::Resource(self.terminal.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: value.to_vec(),
        };
        send_frame(&mut self.stream, &set).await;
    }

    /// The next `METADATA_CHANGED` for `key` on this pane: `Some(None)` is a
    /// delete tombstone, `None` the deadline.
    async fn next_change(&mut self, key: &str, end: tokio::time::Instant) -> Option<Option<Value>> {
        next_change(&mut self.stream, &self.terminal, key, end).await
    }

    async fn next_record(&mut self, end: tokio::time::Instant) -> Option<Value> {
        loop {
            if let Some(record) = self.next_change(RESOURCE_AGENT_KEY, end).await? {
                return Some(record);
            }
        }
    }

    /// Wait for the converged `state` (the detector may publish `idle` a tick
    /// before it derives `blocked`, phux-manu).
    async fn await_state(&mut self, want: &str, within: Duration) -> Option<Value> {
        let end = tokio::time::Instant::now() + within;
        loop {
            let record = self.next_record(end).await?;
            if record["state"] == want {
                return Some(record);
            }
        }
    }

    /// Every record until `done` holds or `within` elapses, as
    /// `(kind, state)` pairs in subscriber order.
    async fn records_until(
        &mut self,
        within: Duration,
        done: impl Fn(&(String, String)) -> bool,
    ) -> Vec<(String, String)> {
        let end = tokio::time::Instant::now() + within;
        let mut seen = Vec::new();
        while let Some(record) = self.next_record(end).await {
            let pair = (field(&record, "kind"), field(&record, "state"));
            let finished = done(&pair);
            seen.push(pair);
            if finished {
                break;
            }
        }
        seen
    }
}

async fn next_change(
    stream: &mut UnixStream,
    terminal: &ResourceId,
    want: &str,
    end: tokio::time::Instant,
) -> Option<Option<Value>> {
    loop {
        let remaining = end.checked_duration_since(tokio::time::Instant::now())?;
        let (_, frame) = timeout(remaining, recv_typed(stream)).await.ok()?;
        if let FrameKind::MetadataChanged {
            scope, key, value, ..
        } = frame
            && key == want
            && scope == Scope::Resource(terminal.clone())
        {
            return Some(value.map(|bytes| serde_json::from_slice(&bytes).unwrap()));
        }
    }
}

fn field(record: &Value, name: &str) -> String {
    record[name].as_str().unwrap_or("").to_owned()
}

fn deadline(within: Duration) -> tokio::time::Instant {
    tokio::time::Instant::now() + within
}

/// A live dialog is `blocked`, identified as `claude` from the process group,
/// with the manifest's name and no detector-written `attention` (L3 §3.7
/// derives it). A watcher that never attaches (`phux watch`) receives the
/// same record: fanout once resolved mailboxes through attached clients only.
/// Hook evidence (`REPORT_AGENT_STATE`) enters the same pipeline.
#[test]
fn detector_publishes_blocked_from_a_live_prompt_box() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let agent = write_fake_agent(tmp.path(), "sleep 30");
        let mut pane = Pane::agent(&tmp, &agent).await;
        let mut watcher = wait_for_socket(&pane.socket_path, SOCKET_CONNECT_DEADLINE).await;
        let subscribe = FrameKind::SubscribeMetadata {
            scope: Scope::Resource(pane.terminal.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        };
        send_frame(&mut watcher, &subscribe).await;

        let record = pane.await_state("blocked", DETECT_DEADLINE).await;
        let record = record.expect("a live permission dialog must publish `blocked`");
        assert_eq!(
            (field(&record, "kind"), field(&record, "name")),
            ("claude".into(), "claude".into())
        );
        assert!(record.get("attention").is_none(), "{record}");

        let end = deadline(DETECT_DEADLINE);
        loop {
            let change = next_change(&mut watcher, &pane.terminal, RESOURCE_AGENT_KEY, end).await;
            let change = change.expect("an unattached subscriber must receive the record");
            if change.is_some_and(|record| record["state"] == "blocked") {
                break;
            }
        }

        let report = FrameKind::Command {
            request_id: 71,
            command: Command::ReportAgentState {
                terminal_id: pane.terminal.clone(),
                state: ReportedAgentState::Done,
            },
        };
        send_frame(&mut pane.stream, &report).await;
        let end = deadline(DETECT_DEADLINE);
        let (mut acked, mut saw_done) = (false, false);
        while !(acked && saw_done) {
            let remaining = end.checked_duration_since(tokio::time::Instant::now());
            let remaining = remaining.expect("REPORT_AGENT_STATE did not converge");
            match timeout(remaining, recv_typed(&mut pane.stream))
                .await
                .unwrap()
                .1
            {
                FrameKind::CommandResult {
                    request_id: 71,
                    result: CommandResult::Ok,
                } => acked = true,
                FrameKind::MetadataChanged { key, value, .. } if key == RESOURCE_AGENT_KEY => {
                    let record: Option<Value> = value.map(|b| serde_json::from_slice(&b).unwrap());
                    saw_done = record.is_some_and(|r| r["state"] == "done");
                }
                _ => {}
            }
        }
    });
}

/// The fail-safe: a plain shell painting dialog-shaped text gets a
/// pane-occupant record but never an agent record. An unidentified pane is
/// not an idle agent; it is not an agent.
#[test]
fn a_plain_shell_pane_never_gets_an_agent_record() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "echo 'Do you want to proceed?'; echo '1. Yes'; sleep 20",
        ]);
        let mut pane = Pane::start(&tmp, cmd, RESOURCE_PANE_OCCUPANT_KEY).await;
        let occupant = pane
            .next_change(RESOURCE_PANE_OCCUPANT_KEY, deadline(DETECT_DEADLINE))
            .await
            .flatten()
            .expect("a plain shell publishes a pane-occupant record");
        assert_eq!(
            (&occupant["foreground"], &occupant["is_pane_shell"]),
            (&"sh".into(), &true.into())
        );

        pane.subscribe(RESOURCE_AGENT_KEY).await;
        let window = TEST_STARTUP_GRACE + TICK_UNIDENTIFIED * 2;
        let record = pane.next_record(deadline(window)).await;
        assert!(
            record.is_none(),
            "no agent in the process group, no record: {record:?}"
        );
    });
}

/// ADR-0046 §E: `DELETE`ing the record (`phux agent clear`) hands it back to
/// the detector. The edge filter used to still hold the pre-delete tuple, so a
/// `blocked` agent (which never emits again) vanished from the sidebar forever.
#[test]
fn deleting_the_record_hands_it_back_to_the_detector() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let agent = write_fake_agent(tmp.path(), "sleep 30");
        let mut pane = Pane::agent(&tmp, &agent).await;
        assert!(pane.await_state("blocked", DETECT_DEADLINE).await.is_some());

        let delete = FrameKind::DeleteMetadata {
            request_id: 7,
            scope: Scope::Resource(pane.terminal.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        };
        send_frame(&mut pane.stream, &delete).await;
        assert!(
            pane.await_state("blocked", DETECT_DEADLINE).await.is_some(),
            "after a DELETE the detector must resume and republish",
        );
    });
}

/// THE WEDGE (phux-w7z2.13): a human declaration (`phux agent set --state
/// working`) outranks derivation, but once its process dies without a trap
/// or clear the record must withdraw to `unknown` — keeping the human's name
/// and kind, dropping `attention` — instead of reporting `working` forever.
#[test]
fn a_declared_state_does_not_survive_the_death_of_the_process_it_describes() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let agent = write_departing_agent(tmp.path(), "sleep 300");
        let mut pane = Pane::agent(&tmp, &agent).await;
        assert!(pane.await_state("blocked", DETECT_DEADLINE).await.is_some());

        let declared = br#"{"name":"me","kind":"claude","state":"working","attention":"high"}"#;
        pane.set_agent(11, declared).await;
        assert!(pane.await_state("working", DETECT_DEADLINE).await.is_some());
        depart(tmp.path());

        let healed = pane.await_state("unknown", DEPARTURE_DEADLINE).await;
        let healed = healed.expect("a declared record must withdraw once its process is gone");
        assert_eq!(
            (field(&healed, "name"), field(&healed, "kind")),
            ("me".into(), "claude".into())
        );
        assert!(healed.get("attention").is_none(), "{healed}");
    });
}

/// A detector-written record is retracted (deleted) when the agent leaves a
/// pane that keeps running, after `VACANT_CONFIRMATIONS`.
#[test]
fn a_detector_written_record_is_retracted_when_the_agent_leaves_the_pane() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let agent = write_departing_agent(tmp.path(), "sleep 300");
        let mut pane = Pane::agent(&tmp, &agent).await;
        assert!(pane.await_state("blocked", DETECT_DEADLINE).await.is_some());
        depart(tmp.path());

        let end = deadline(DEPARTURE_DEADLINE);
        loop {
            let change = pane.next_change(RESOURCE_AGENT_KEY, end).await;
            if change.expect("the record was never retracted").is_none() {
                break;
            }
        }
    });
}

/// I2 (phux-w7z2.27): when `codex` replaces `claude` in a pane, a subscriber
/// must never see one record pairing claude's kind with codex's state. The
/// correcting write lands on `unknown`, then the pane converges on codex's
/// own derived state.
#[test]
fn a_kind_change_never_leaves_a_stale_kind_beside_a_live_state() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let codex = write_fake_codex(tmp.path());
        let agent = write_departing_agent(tmp.path(), &codex.display().to_string());
        let mut pane = Pane::agent(&tmp, &agent).await;
        assert!(pane.await_state("blocked", DETECT_DEADLINE).await.is_some());
        depart(tmp.path());

        let pairs = pane
            .records_until(DEPARTURE_DEADLINE, |(k, s)| k == "codex" && s == "blocked")
            .await;
        let switch = pairs.iter().position(|(kind, _)| kind == "codex");
        let switch = switch.unwrap_or_else(|| panic!("never learned of codex: {pairs:?}"));
        assert_eq!(pairs[switch].1, "unknown", "{pairs:?}");
        assert!(
            pairs[switch..].iter().all(|(kind, _)| kind == "codex"),
            "{pairs:?}"
        );
        assert!(
            pairs[switch..].iter().any(|(_, state)| state == "blocked"),
            "{pairs:?}"
        );
    });
}

/// phux-w7z2.45: with an explicit (shim-declared) `kind: claude`, which the
/// server must preserve, a codex handover must withdraw the state rather than
/// ever pair `kind: claude` with a state derived from codex's screen.
#[test]
fn a_declared_kind_never_gains_a_state_derived_from_a_different_occupant() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let codex = write_fake_codex(tmp.path());
        let agent = write_departing_agent(tmp.path(), &codex.display().to_string());
        let mut pane = Pane::agent(&tmp, &agent).await;
        assert!(pane.await_state("blocked", DETECT_DEADLINE).await.is_some());
        pane.set_agent(11, br#"{"name":"claude","kind":"claude"}"#)
            .await;
        depart(tmp.path());

        let pairs = pane
            .records_until(DEPARTURE_DEADLINE, |(k, s)| k == "claude" && s == "unknown")
            .await;
        let withdrawn = pairs
            .iter()
            .position(|(k, s)| k == "claude" && s == "unknown");
        let withdrawn = withdrawn.unwrap_or_else(|| panic!("never withdrew: {pairs:?}"));
        assert!(
            pairs[withdrawn..]
                .iter()
                .all(|(k, s)| k != "claude" || s == "unknown"),
            "{pairs:?}",
        );
    });
}

/// ADR-0046 §8: an identity-only `SET_METADATA` is not a declaration, so the
/// detector fills `state` in around the human's fields. It used to write
/// nothing (edge filter), leaving `state` unset forever.
#[test]
fn an_identity_only_set_gets_its_state_filled_in_by_the_detector() {
    shorten_detector_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let agent = write_fake_agent(tmp.path(), "sleep 30");
        let mut pane = Pane::agent(&tmp, &agent).await;
        assert!(pane.next_record(deadline(DETECT_DEADLINE)).await.is_some());
        pane.set_agent(9, br#"{"name":"reviewer","session":"fleet-7"}"#)
            .await;

        // Wait for the conjunction: a detector write in flight before the SET
        // applied can still say `name: claude` (phux-uaon).
        let end = deadline(DETECT_DEADLINE);
        let mut saw_reviewer = false;
        let filled = loop {
            let record = pane.next_record(end).await;
            let record = record.expect("state never filled in around the human name");
            match field(&record, "name").as_str() {
                "reviewer" if record["state"] == "blocked" => break record,
                "reviewer" => saw_reviewer = true,
                "claude" if saw_reviewer => panic!("detector clobbered the name: {record}"),
                _ => {}
            }
        };
        assert_eq!(field(&filled, "session"), "fleet-7", "{filled}");
    });
}
