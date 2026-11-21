//! The connection states, without a server.

use std::time::{Duration, Instant};

use phux_client_runtime::Client;
use phux_client_runtime::control::Status;

use super::standing::plainly;
use super::{Banner, Desk, RETRY_CEILING, Standing, backoff, forgets, incompatible, sessions};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use phux_client_runtime::Runtime;
use phux_client_runtime::control::{ControlOptions, PaneDescriptor, Topology};
use phux_protocol::ids::ResourceId;

use crate::desk::{NoticeKind, Pending, Shown};
use crate::launch::{self, Options};

mod phone;

/// A refused token as the QUIC lane renders it (`dial::dial_message`).
const QUIC_REFUSED: &str =
    "mini refused the pairing token (unauthorized); re-pair it with `phux host add mini`";

fn desk() -> Desk {
    let options = Options {
        socket: Some(PathBuf::from("/nowhere/phux.sock")),
        ..Options::default()
    };
    Desk::with_preferences(
        &launch::resolve(&options),
        None,
        rune::prefs::Prefs::memory(),
    )
}

fn pane(id: u32, session: u32, cwd: Option<&str>) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(id),
        session_id: session,
        session_name: format!("s{session}"),
        cols: 80,
        rows: 24,
        title: None,
        cwd: cwd.map(str::to_owned),
        is_focused: false,
        key: None,
        revision: None,
        name: None,
        activation: None,
        exec: None,
        foreground: None,
        icon: None,
    }
}

fn topology(panes: Vec<PaneDescriptor>) -> Topology {
    crate::desk::topology(Vec::new(), panes)
}

fn embedded() -> Arc<Client> {
    Arc::new(Runtime::embedded(ControlOptions::default()))
}

#[test]
fn a_failed_open_keeps_its_clock_so_a_gone_terminal_is_given_up() {
    let mut desk = desk();
    let client = embedded();
    let asked = Instant::now() - Duration::from_secs(60);
    desk.connection.topology = Some(topology(vec![pane(1, 1, None)]));
    // Listed but not openable yet: still wanted, and no younger.
    desk.connection.wanted = Some((ResourceId::local(1), asked));
    desk.open_wanted(&client);
    assert_eq!(desk.connection.wanted.as_ref().map(|w| w.1), Some(asked));
    // Gone past the grace: let go.
    desk.connection.topology = Some(topology(Vec::new()));
    desk.open_wanted(&client);
    assert_eq!(desk.connection.wanted, None);
}

#[test]
fn the_fallback_pane_is_the_selected_sessions() {
    let graph = topology(vec![pane(1, 1, None), pane(2, 2, None)]);
    assert_eq!(
        sessions::default_pane(&graph, 2),
        Some(ResourceId::local(2))
    );
    assert_eq!(sessions::default_pane(&graph, 3), None);
}

#[test]
fn new_work_starts_in_the_focused_panes_directory() {
    let mut desk = desk();
    assert_eq!(desk.focused_cwd(), None);
    desk.connection.topology = Some(topology(vec![pane(1, 1, Some("/there")), pane(2, 1, None)]));
    let shown = |id| super::super::Shown {
        resource: ResourceId::local(id),
        view: phux_client_runtime::ViewId::from_raw(1),
    };
    desk.connection.shown = Some(shown(1));
    assert_eq!(desk.focused_cwd().as_deref(), Some("/there"));
    // A pane that hasn't reported yet goes by the one beside it.
    desk.connection.shown = Some(shown(2));
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Split {
        dir: phux_client_core::layout::SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(phux_client_core::layout::LayoutNode::Leaf(
            ResourceId::local(1),
        )),
        right: Box::new(phux_client_core::layout::LayoutNode::Leaf(
            ResourceId::local(2),
        )),
    });
    assert_eq!(desk.focused_cwd().as_deref(), Some("/there"));
}

#[test]
fn a_window_says_where_it_stands() {
    let mut desk = desk();
    assert!(matches!(desk.standing(), Standing::Unreachable(_)));
    desk.link.finding = true;
    assert_eq!(desk.standing(), Standing::Starting);
    desk.link.finding = false;
    desk.connection.client = Some(embedded());
    desk.connection.status = Status::Connecting;
    assert!(matches!(desk.standing(), Standing::Connecting(_)));
    desk.connection.status = Status::Attached;
    desk.connection.topology = Some(topology(Vec::new()));
    assert_eq!(desk.standing(), Standing::NoSessions);
    desk.connection.status = Status::Closed;
    assert!(matches!(desk.standing(), Standing::Closed(_)));
    desk.link.incompatible = Some("server speaks 12".to_owned());
    assert_eq!(
        desk.standing(),
        Standing::Incompatible("server speaks 12".to_owned())
    );
}

#[test]
fn nothing_retries_what_retrying_cant_change() {
    let mut desk = desk();
    desk.connection.status = Status::Failed;
    assert!(!desk.stuck());
    desk.connection.error =
        Some("mini: WebSocket handshake: HTTP error: 401 Unauthorized".to_owned());
    assert!(desk.stuck());
    // The QUIC lane's refusal is as final as the WebSocket's.
    desk.connection.error = Some(QUIC_REFUSED.to_owned());
    assert!(desk.stuck());
    desk.connection.error = Some("mini did not answer (timed out)".to_owned());
    assert!(!desk.stuck());
    desk.connection.error = None;
    desk.failed_with("server error VersionIncompatible: server speaks 12");
    assert!(desk.stuck());
}

#[test]
fn the_runtime_renders_a_protocol_refusal_as_this_reads_it() {
    use phux_protocol::wire::frame::{ErrorCode, FrameKind};
    let client = embedded();
    client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    let refused = client
        .feed(FrameKind::Error {
            request_id: None,
            code: ErrorCode::VersionIncompatible,
            message: "server speaks 12".to_owned(),
        })
        .expect_err("a fatal refusal");
    assert_eq!(
        incompatible(&refused.to_string()).as_deref(),
        Some("server speaks 12")
    );
}

/// A desk with a terminal on screen whose connection dropped a while ago.
fn stale(client: Arc<Client>) -> Desk {
    let mut desk = desk();
    desk.connection.shown = Some(Shown {
        resource: ResourceId::local(1),
        view: phux_client_runtime::ViewId::from_raw(1),
    });
    desk.connection.client = Some(client);
    desk.connection.status = Status::Connecting;
    desk.link.down_since = Some(Instant::now() - Duration::from_secs(5));
    desk
}

#[test]
fn a_stale_window_says_when_it_has_stopped_trying() {
    let mut desk = stale(embedded());
    assert!(matches!(desk.banner(), Some(Banner::Reconnecting(_))));
    desk.connection.status = Status::Failed;
    desk.failed_with("server error VersionIncompatible: server speaks 12");
    assert_eq!(
        desk.banner(),
        Some(Banner::Stuck(Standing::Incompatible(
            "server speaks 12".to_owned()
        )))
    );
    desk.link.incompatible = None;
    desk.connection.status = Status::Closed;
    desk.link.ended = Some("replaced".to_owned());
    assert_eq!(
        desk.banner(),
        Some(Banner::Stuck(Standing::Closed("replaced".to_owned())))
    );
    desk.link.ended = None;
    desk.connection.status = Status::Failed;
    for refusal in [
        "mini: WebSocket handshake: HTTP error: 401 Unauthorized",
        QUIC_REFUSED,
    ] {
        desk.connection.error = Some(refusal.to_owned());
        let Some(Banner::Stuck(Standing::Unreachable(why))) = desk.banner() else {
            panic!("{refusal} keeps retrying");
        };
        assert!(why.contains("Set it up again"), "{why}");
    }
}

#[test]
fn a_killed_session_leaves_the_link_up_for_the_next_one() {
    use phux_client_runtime::control::Event;
    use phux_protocol::wire::frame::DetachReason;
    let client = embedded();
    let mut desk = desk();
    desk.connection.client = Some(client.clone());
    desk.connection.status = Status::Attached;
    let ended = |reason| Event::Detached {
        reason: Some(reason),
        message: "the session this attach was rooted in was killed".to_owned(),
    };
    desk.apply_connection(&client, ended(DetachReason::SessionKilled));
    assert!(
        desk.link_is_up(),
        "the socket stays open and nothing retries"
    );
    desk.apply_connection(&client, ended(DetachReason::AuthorizationRevoked));
    assert!(!desk.link_is_up(), "this one really did end the connection");
}

/// Counts the wakes a client delivers.
#[derive(Default)]
struct Wakes(AtomicU32);

impl phux_client_runtime::Listener for Wakes {
    fn on_activity(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_terminal_that_cant_take_focus_doesnt_wake_the_window() {
    let client = embedded();
    let wakes = Arc::new(Wakes::default());
    client.set_listener(wakes.clone());
    let _ = client.take_activity();
    let before = wakes.0.load(Ordering::SeqCst);
    let mut desk = stale(client.clone());
    // Down: the connection coming back is the wake.
    assert!(!desk.sync_focus(&client));
    // Up but not ready: a timer looks again, not a loop.
    desk.connection.status = Status::Attached;
    assert!(desk.sync_focus(&client));
    assert_eq!(wakes.0.load(Ordering::SeqCst), before);
    desk.focused = false;
    assert!(!desk.sync_focus(&client));
}

#[test]
fn only_the_first_connection_is_a_replay() {
    use phux_client_runtime::control::ControlPlane;
    let client = embedded();
    let mut desk = stale(client.clone());
    assert!(!desk.link.heard);
    client.with_control(ControlPlane::connection_opened);
    let mut observation = client.take_activity();
    desk.follow_connection(&client, &mut observation);
    assert!(desk.link.heard);
    // A replaced client is the same server, whatever its epoch.
    desk.renew();
    assert_eq!(desk.connection.epoch, 0);
    assert!(desk.link.heard);
}

#[test]
fn a_replaced_client_keeps_whats_waiting_on_you() {
    let mut desk = stale(embedded());
    let held = Pending::Paste(ResourceId::local(1), "a\nb".to_owned());
    desk.pending = Some(held.clone());
    desk.say(NoticeKind::Error, "spawn failed");
    desk.renew();
    assert_eq!(desk.pending, Some(held));
    let notice = desk.notice.as_ref().map(|notice| notice.text.as_str());
    assert_eq!(notice, Some("spawn failed"));
}

#[test]
fn a_replaced_client_keeps_the_notice_clock() {
    let mut desk = stale(embedded());
    desk.clock = 100.0;
    desk.renew();
    desk.say(NoticeKind::Error, "after the retry");
    let expires = desk.toasts().next_due().unwrap_or(0.0);
    assert!(expires > 100.0);
}

/// A stale window holding everything a server can leave behind.
fn holding(client: Option<Arc<Client>>) -> Desk {
    let mut desk = stale(embedded());
    desk.connection.client = client;
    desk.link.heard = true;
    desk.link.home = Some(("work".to_owned(), None));
    desk.link.org_unavailable = Some(1);
    desk.link.keep = Some("old machine session".into());
    desk.link.incompatible = Some("server speaks 12".to_owned());
    desk.pending = Some(Pending::Close(ResourceId::local(1)));
    desk.connection.wanted = Some((ResourceId::local(3), Instant::now()));
    desk
}

fn assert_nothing_held(desk: &Desk) {
    assert_eq!(desk.pending, None);
    assert_eq!(desk.connection.wanted, None);
    assert_eq!(desk.link.home, None);
    assert_eq!(desk.link.org_unavailable, None);
    assert_eq!(desk.link.keep, None);
    assert_eq!(desk.link.incompatible, None);
    assert!(!desk.link.heard);
    assert_eq!(desk.link.down_since, None);
}

#[test]
fn another_server_drops_what_was_held_for_this_one() {
    let mut desk = holding(Some(embedded()));
    let server = desk.link.server;
    desk.reach(PathBuf::from("/nowhere/other.sock"), true);
    assert_nothing_held(&desk);
    assert!(desk.link.server > server);
}

#[test]
fn a_lost_machine_leaves_nothing_for_the_stand_in() {
    let mut desk = holding(None);
    desk.machines.current = Some("mini".to_owned());
    desk.lost_remembered("mini", "not a registered host".to_owned());
    assert_nothing_held(&desk);
    assert_eq!(desk.machines.current, None);
    assert!(desk.link.moved.is_some());
}

#[test]
fn only_a_real_launch_forgets_only_its_own_machine() {
    let gone = "mini is not a registered host";
    assert!(forgets(false, Some("mini"), "mini", gone));
    assert!(!forgets(true, Some("mini"), "mini", gone));
    assert!(!forgets(false, Some("other"), "mini", gone));
    assert!(!forgets(false, None, "mini", gone));
    assert!(!forgets(false, Some("mini"), "mini", "config unreadable"));
}

const REFUSED: &str = "server error VersionIncompatible: server speaks 12";

#[test]
fn a_finished_handoff_outlasts_the_old_servers_refusal() {
    let mut desk = stale(embedded());
    desk.connection.status = Status::Failed;
    desk.failed_with(REFUSED);
    assert!(desk.stuck());
    desk.server_handed_off();
    assert!(!desk.stuck());
    // The re-exec is still going: that refusal is the old server's.
    desk.connection.status = Status::Failed;
    desk.failed_with(REFUSED);
    assert!(!desk.stuck());
    assert!(matches!(desk.standing(), Standing::Connecting(_)));
    // Past that it's the answer, and it says so again.
    desk.link.handoff_until = Some(Instant::now());
    desk.failed_with(REFUSED);
    assert!(desk.stuck());
    assert!(matches!(desk.standing(), Standing::Incompatible(_)));
}

#[test]
fn a_handoff_here_changes_nothing_on_a_remote() {
    let mut desk = stale(embedded());
    desk.machines.current = Some("mini".to_owned());
    desk.failed_with(REFUSED);
    desk.server_handed_off();
    assert!(desk.stuck());
}

#[test]
fn a_failed_search_keeps_the_last_known_server() {
    let mut desk = desk();
    let aimed = desk.launch.socket.clone();
    let mut failed = desk.launch.clone();
    failed.socket = Err("timed out".to_owned());
    desk.adopt(failed);
    assert_eq!(desk.launch.socket, aimed);
    assert_eq!(desk.connection.error.as_deref(), Some("timed out"));
}

#[test]
fn a_failed_search_keeps_what_the_updater_manages() {
    let mut desk = desk();
    let target = (PathBuf::from("/phux"), PathBuf::from("/sock"));
    desk.updater.install = Ok(PathBuf::from("/Applications/Phux Rune.app"));
    desk.updater.target = Some(target.clone());
    let mut failed = desk.launch.clone();
    failed.socket = Err("timed out".to_owned());
    failed.aimed = false;
    desk.adopt(failed.clone());
    assert_eq!(desk.updater.target, Some(target));
    // A search that found one replaces it, and an aimed run still has none.
    let found = PathBuf::from("/sock2");
    failed.socket = Ok(found.clone());
    failed.phux = Some(PathBuf::from("/phux"));
    desk.adopt(failed.clone());
    assert_eq!(desk.updater.target.as_ref().map(|t| &t.1), Some(&found));
    failed.socket = Err("timed out".to_owned());
    failed.aimed = true;
    desk.adopt(failed);
    assert_eq!(desk.updater.target, None);
}

#[test]
fn retries_widen_to_a_ceiling() {
    assert_eq!(backoff(0), Duration::from_secs(2));
    assert_eq!(backoff(1), Duration::from_secs(4));
    assert_eq!(backoff(3), Duration::from_secs(16));
    assert_eq!(backoff(9), RETRY_CEILING);
    assert_eq!(backoff(u32::MAX), RETRY_CEILING);
}

#[test]
fn transport_errors_read_as_words() {
    assert_eq!(
        plainly("connect: Connection refused (os error 61)"),
        "The server isn't answering."
    );
    assert_eq!(
        plainly("connect: no answer within the initial budget"),
        "No answer from the machine."
    );
    assert!(plainly("websocket: 401 pairing token refused").contains("Set it up again"));
    assert!(plainly(QUIC_REFUSED).contains("Set it up again"));
    assert_eq!(plainly("something new"), "something new");
}

#[test]
fn a_window_without_phux_installs_it_and_says_why_an_install_failed() {
    let mut desk = Desk::with_preferences(
        &launch::Resolved::offline("no phux"),
        None,
        rune::prefs::Prefs::memory(),
    );
    assert_eq!(desk.standing(), Standing::NoCli(None));
    // What `install_phux` sets before its worker runs the real installer.
    desk.link.installing = true;
    desk.link.finding = true;
    assert_eq!(desk.standing(), Standing::Installing);
    // A retry while installing waits for the install's own search.
    let attempt = desk.link.attempt;
    desk.reconnect();
    assert_eq!(desk.link.attempt, attempt);
    desk.link
        .found
        .0
        .send((attempt, Err("no release".to_owned())))
        .unwrap();
    assert!(desk.pump_link());
    assert_eq!(
        desk.standing(),
        Standing::NoCli(Some("no release".to_owned()))
    );
}

#[test]
fn an_install_shows_a_moving_bar_until_it_ends() {
    let mut desk = Desk::with_preferences(
        &launch::Resolved::offline("no phux"),
        None,
        rune::prefs::Prefs::memory(),
    );
    desk.link.installing = true;
    desk.link.finding = true;
    let mut rt = crate::test_support::runtime(
        desk,
        crate::ui::view,
        (900.0, 600.0),
        rune::Env::new(false, 1.0),
    );
    rt.frame(0.0);
    assert!(rt.find("placeholder-progress").is_some());
    rt.state.link.installing = false;
    rt.state.link.finding = false;
    rt.frame(1.0);
    assert!(rt.find("placeholder-progress").is_none());
}
