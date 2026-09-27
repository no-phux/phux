//! Real-server coverage for the fleet sidebar's roster: a real TUI client
//! attached through a PTY must show a peer session's state histogram. The
//! peer sweep is deferred to the first repaint, and client unit tests call the
//! sweep directly, so this is the test that fails if it is never sent. The
//! histogram (not the row, which exists without any sweep) is the assertion.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::layout::Workspace;
use phux_client::layout_ops::layout_key;
use phux_protocol::ids::{GroupId, ResourceId, SessionId};
use phux_protocol::wire::frame::{FrameKind, Scope};

/// The session the client attaches to, listed in the Sessions panel like
/// every other session since ADR-0112.
const SESSION: &str = "work";
/// The session it does NOT attach to, whose row must carry the swept
/// histogram.
const PEER: &str = "scratch";
/// How many session ids to probe for the peer's persisted layout.
const SESSION_ID_SCAN: u32 = 8;
/// How long to wait for the briefly attached peer client to write its layout.
const LAYOUT_DEADLINE: Duration = Duration::from_secs(20);
/// A liveness bound: the roster is allowed to arrive late.
const ROSTER_DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);

struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    fn start() -> Self {
        Self(
            common::ServerGuard::builder("fleet-sidebar")
                // Panes run the server's `$SHELL`; never inherit the runner's.
                .env("SHELL", "/bin/sh")
                .start(),
        )
    }

    /// The peer's seed pane, read back rather than assumed.
    fn peer_pane(&self) -> ResourceId {
        let stdout = self.success(&["snapshot", "--json", PEER]);
        let snapshot: serde_json::Value =
            serde_json::from_str(&stdout).expect("snapshot JSON for the peer session");
        let pane = u32::try_from(snapshot["pane"].as_u64().expect("snapshot pane id"))
            .expect("pane id fits u32");
        ResourceId::local(pane)
    }

    /// Wait until some session's persisted layout names `pane`; without one
    /// the peer's histogram is legitimately empty and the test could not tell
    /// a working sweep from a missing one. Session ids are scanned because no
    /// JSON surface exposes them.
    fn wait_for_persisted_layout(&self, pane: &ResourceId) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let deadline = Instant::now() + LAYOUT_DEADLINE;
        loop {
            let found = runtime.block_on(async {
                let mut conn = Connection::connect(&self.socket)
                    .await
                    .expect("connect metadata client");
                for candidate in 1..=SESSION_ID_SCAN {
                    let request_id = candidate;
                    conn.send(&FrameKind::GetMetadata {
                        request_id,
                        scope: Scope::Group(GroupId::new(1)),
                        key: layout_key(SessionId::new(candidate)),
                    })
                    .await
                    .expect("request candidate layout");
                    if let FrameKind::MetadataValue {
                        value: Some(bytes), ..
                    } = conn.recv().await.expect("candidate layout reply")
                        && let Ok(workspace) = Workspace::decode_cbor(&bytes)
                        && workspace
                            .windows
                            .iter()
                            .filter_map(|w| w.state.tree.as_ref())
                            .flat_map(phux_client::layout::leaves)
                            .any(|leaf| leaf == *pane)
                    {
                        return true;
                    }
                }
                false
            });
            if found {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no session persisted a layout naming {pane:?}"
            );
            std::thread::sleep(POLL);
        }
    }
}

/// An attached TUI client on a fresh `default` profile with onboarding
/// already complete, so neither a developer's config nor the onboarding
/// overlay decides whether the roster paints. 120x40 keeps the Sessions panel
/// from being yielded away.
struct AttachedClient(common::PtyAttach);

impl AttachedClient {
    fn start(server: &ServerGuard) -> Self {
        Self(common::PtyAttach::start_with(
            &server.socket,
            &[SESSION],
            (120, 40),
            |config, command| {
                std::fs::create_dir_all(config.join("phux")).expect("state dir");
                std::fs::write(
                    config.join("phux/onboarding.json"),
                    r#"{"version":1,"stage":"complete"}"#,
                )
                .expect("preseed completed onboarding state");
                command.env("PHUX_PROFILE", "default");
                command.env("XDG_STATE_HOME", config);
            },
        ))
    }

    fn painted(&self) -> String {
        self.0.painted()
    }

    /// Wait until every phrase has been painted at some point in the
    /// transcript, then return it. Panics with the painted text so a failure
    /// shows what the strip actually rendered instead of just "not found".
    fn wait_for_all(&self, phrases: &[&str]) -> String {
        let deadline = Instant::now() + ROSTER_DEADLINE;
        loop {
            let painted = self.painted();
            if phrases.iter().all(|phrase| painted.contains(phrase)) {
                return painted;
            }
            if Instant::now() >= deadline {
                let missing: Vec<&str> = phrases
                    .iter()
                    .copied()
                    .filter(|phrase| !painted.contains(phrase))
                    .collect();
                panic!("sidebar never painted {missing:?}; painted text was:\n{painted}");
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The most recently painted Sessions roster cell for `name`: cells split on
/// the sidebar separator, rejecting agent rows (prose suffixes) in favor of
/// the optional state histogram.
fn latest_roster_cell<'a>(painted: &'a str, name: &str) -> Option<&'a str> {
    painted.rsplit('│').map(str::trim).find(|cell| {
        ["● ", "○ ", "◆ ", "◐ "].iter().any(|badge| {
            let Some(suffix) = cell
                .strip_prefix(badge)
                .and_then(|rest| rest.strip_prefix(name))
            else {
                return false;
            };
            (suffix.is_empty() || suffix.starts_with(char::is_whitespace))
                && suffix.chars().all(|c| {
                    c.is_whitespace() || c.is_ascii_digit() || matches!(c, '●' | '◆' | '◐' | '?')
                })
        })
    })
}

#[test]
fn roster_cell_parser_ignores_agent_rows_and_status_shortcuts() {
    let painted = "Agents│● scratch blocked - claude│Sessions│○ work│● scratch ●1│C-a s Sessions";
    assert_eq!(latest_roster_cell(painted, "scratch"), Some("● scratch ●1"));
    assert_eq!(latest_roster_cell(painted, "work"), Some("○ work"));
}

/// Everything the peer needs exists before the client attaches, so only the
/// deferred sweep stands between the peer row and its histogram (verified to
/// fail with the deferred send stubbed out).
#[test]
#[ignore = "spawns a real server and attached PTY client; run in the e2e lane"]
fn deferred_peer_sweep_still_describes_the_spaces_roster() {
    let server = ServerGuard::start();
    server.success(&["new", "--json", "-s", PEER]);

    let peer_pane = server.peer_pane();
    let peer_selector = format!("@{}", peer_pane.local_id().expect("local peer pane"));
    // A placed spawn persists a layout under the peer's key.
    server.success(&["spawn", "--target", &peer_selector]);
    server.wait_for_persisted_layout(&peer_pane);

    // `blocked` renders as `●1` (idle/unknown render nothing).
    server.success(&[
        "agent",
        "set",
        &peer_selector,
        "--name",
        "claude",
        "--kind",
        "claude",
        "--state",
        "blocked",
    ]);

    let client = AttachedClient::start(&server);

    let painted = client.wait_for_all(&["Sessions", PEER, "●1"]);

    // The histogram must land on the peer's own row, not the attached one.
    let peer_row = latest_roster_cell(&painted, PEER)
        .unwrap_or_else(|| panic!("no Sessions row for {PEER}:\n{painted}"));
    assert!(
        peer_row.contains("●1"),
        "the peer's row carries its swept histogram:\n{painted}"
    );
    let session_row = latest_roster_cell(&painted, SESSION)
        .unwrap_or_else(|| panic!("no Sessions row for {SESSION}:\n{painted}"));
    assert!(
        !session_row.contains("●1"),
        "the peer's histogram must not leak into the attached session's row:\n{painted}"
    );
}
