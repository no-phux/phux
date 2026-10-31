//! The connection states, without a server.

use std::time::{Duration, Instant};

use phux_client_runtime::Client;
use phux_client_runtime::control::Status;

use super::{Banner, Desk, RETRY_CEILING, Standing, backoff, incompatible, plainly, sessions};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use phux_client_core::layout::wire::HandleMap;
use phux_client_runtime::Runtime;
use phux_client_runtime::control::{ControlOptions, PaneDescriptor, Topology};
use phux_protocol::ids::ResourceId;

use crate::desk::{NoticeKind, Pending, Shown};
use crate::launch::{self, Options};

fn desk() -> Desk {
    let options = Options {
        socket: Some(PathBuf::from("/nowhere/phux.sock")),
        ..Options::default()
    };
    Desk::open(&launch::resolve(&options), None)
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
    }
}

fn topology(panes: Vec<PaneDescriptor>) -> Topology {
    Topology {
        sessions: Vec::new(),
        panes,
        agent_sessions: Vec::new(),
        focused_session: 0,
        focused_pane: ResourceId::local(0),
        authority: None,
        inbox: None,
        handles: HandleMap::new(),
    }
}

fn embedded() -> Arc<Client> {
    Arc::new(Runtime::embedded(ControlOptions::default()))
}

#[test]
fn a_failed_open_keeps_its_clock_so_a_gone_terminal_is_given_up() {
    let mut desk = desk();
    let client = embedded();
    let asked = Instant::now() - Duration::from_secs(60);
    desk.topology = Some(topology(vec![pane(1, 1, None)]));
    // Listed but not openable yet: still wanted, and no younger.
    desk.wanted = Some((ResourceId::local(1), asked));
    desk.open_wanted(&client);
    assert_eq!(desk.wanted.as_ref().map(|w| w.1), Some(asked));
    // Gone past the grace: let go.
    desk.topology = Some(topology(Vec::new()));
    desk.open_wanted(&client);
    assert_eq!(desk.wanted, None);
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
    desk.topology = Some(topology(vec![pane(1, 1, Some("/there")), pane(2, 1, None)]));
    let shown = |id| super::super::Shown {
        terminal: ResourceId::local(id),
        view: phux_client_runtime::ViewId::from_raw(1).expect("nonzero"),
    };
    desk.shown = Some(shown(1));
    assert_eq!(desk.focused_cwd().as_deref(), Some("/there"));
    // A pane that hasn't reported yet goes by the one beside it.
    desk.shown = Some(shown(2));
    desk.layout = Some(phux_client_core::layout::LayoutNode::Split {
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
    desk.client = Some(embedded());
    desk.status = Status::Connecting;
    assert!(matches!(desk.standing(), Standing::Connecting(_)));
    desk.status = Status::Attached;
    desk.topology = Some(topology(Vec::new()));
    assert_eq!(desk.standing(), Standing::NoSessions);
    desk.status = Status::Closed;
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
    desk.status = Status::Failed;
    assert!(!desk.stuck());
    desk.error = Some("websocket: 401".to_owned());
    assert!(desk.stuck());
    desk.error = None;
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
    desk.shown = Some(Shown {
        terminal: ResourceId::local(1),
        view: phux_client_runtime::ViewId::from_raw(1).expect("nonzero"),
    });
    desk.client = Some(client);
    desk.status = Status::Connecting;
    desk.link.down_since = Some(Instant::now() - Duration::from_secs(5));
    desk
}

#[test]
fn a_stale_window_says_when_it_has_stopped_trying() {
    let mut desk = stale(embedded());
    assert!(matches!(desk.banner(), Some(Banner::Reconnecting(_))));
    desk.status = Status::Failed;
    desk.failed_with("server error VersionIncompatible: server speaks 12");
    assert_eq!(
        desk.banner(),
        Some(Banner::Stuck(Standing::Incompatible(
            "server speaks 12".to_owned()
        )))
    );
    desk.link.incompatible = None;
    desk.status = Status::Closed;
    desk.link.ended = Some("replaced".to_owned());
    assert_eq!(
        desk.banner(),
        Some(Banner::Stuck(Standing::Closed("replaced".to_owned())))
    );
    desk.link.ended = None;
    desk.status = Status::Failed;
    desk.error = Some("websocket: 401".to_owned());
    assert!(matches!(
        desk.banner(),
        Some(Banner::Stuck(Standing::Unreachable(_)))
    ));
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
    desk.status = Status::Attached;
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
    assert_eq!(desk.epoch, 0);
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
    let expires = desk.notice.as_ref().map_or(0.0, |notice| notice.expires);
    assert!(expires > 100.0);
}

#[test]
fn another_server_drops_what_was_held_for_this_one() {
    let mut desk = stale(embedded());
    desk.link.heard = true;
    desk.pending = Some(Pending::Close(ResourceId::local(1)));
    desk.reach(PathBuf::from("/nowhere/other.sock"), true);
    assert_eq!(desk.pending, None);
    assert!(!desk.link.heard);
}

#[test]
fn a_failed_search_keeps_the_last_known_server() {
    let mut desk = desk();
    let aimed = desk.launch.socket.clone();
    let mut failed = desk.launch.clone();
    failed.socket = Err("timed out".to_owned());
    desk.adopt(failed);
    assert_eq!(desk.launch.socket, aimed);
    assert_eq!(desk.error.as_deref(), Some("timed out"));
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
    assert_eq!(plainly("something new"), "something new");
}

#[test]
fn a_protocol_refusal_keeps_the_servers_words() {
    assert_eq!(
        incompatible("server error VersionIncompatible: server speaks 12").as_deref(),
        Some("server speaks 12")
    );
    assert_eq!(incompatible("server error Unknown: no"), None);
}
