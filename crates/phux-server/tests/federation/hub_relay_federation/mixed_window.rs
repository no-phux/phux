//! A hub window that mixes a local pane with panes on two satellites
//! (phux-lxov.1): each is spawned with `owner_terminal: None` and its id is
//! placed in one session arrangement. Input, resize, snapshot, and kill work
//! on all three; one satellite's link dropping leaves the arrangement whole
//! and the other two panes live, and its pane replays a snapshot once the
//! link returns.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use phux_protocol::ids::{ArrangementKey, OperationId};
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementValue, EntryRef, EntryTarget,
    OrgFilter, OrganizationPage, SplitDirection,
};

use super::*;

/// A loopback TCP relay in front of a satellite's WebSocket listener. Cutting
/// it drops the hub's link (and refuses redials) while the satellite and its
/// panes keep running; restoring it lets the hub's redial through.
struct LinkRelay {
    port: u16,
    cut: Rc<Cell<bool>>,
    conns: Rc<RefCell<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl LinkRelay {
    async fn start(target_port: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cut = Rc::new(Cell::new(false));
        let conns = Rc::new(RefCell::new(Vec::new()));
        let accept = tokio::task::spawn_local({
            let cut = Rc::clone(&cut);
            let conns = Rc::clone(&conns);
            async move {
                while let Ok((inbound, _)) = listener.accept().await {
                    if cut.get() {
                        drop(inbound);
                        continue;
                    }
                    conns
                        .borrow_mut()
                        .push(tokio::task::spawn_local(forward(inbound, target_port)));
                }
            }
        });
        Self {
            port,
            cut,
            conns,
            accept,
        }
    }

    const fn port(&self) -> u16 {
        self.port
    }

    /// Drop every relayed connection and refuse new ones.
    fn cut(&self) {
        self.cut.set(true);
        for conn in self.conns.borrow_mut().drain(..) {
            conn.abort();
        }
    }

    fn restore(&self) {
        self.cut.set(false);
    }
}

impl Drop for LinkRelay {
    fn drop(&mut self) {
        self.cut();
        self.accept.abort();
    }
}

async fn forward(mut inbound: TcpStream, target_port: u16) {
    if let Ok(mut outbound) = TcpStream::connect(("127.0.0.1", target_port)).await {
        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
    }
}

/// Spawn `/bin/cat` on the hub (`host: None`) or on satellite `host`, with
/// no owner, retrying while the hub's link to `host` is still dialing.
async fn spawn_cat(hub: &mut UnixStream, request_id: u32, host: Option<&str>) -> ResourceId {
    let spawn = || {
        let spawn = Spawn {
            owner_terminal: None,
            initial_size: Some((80, 24)),
            ..Spawn::command(&["/bin/cat"])
        };
        host.map_or_else(|| spawn.clone(), |host| on_satellite(host, spawn.clone()))
    };
    let deadline = Instant::now() + STEP_DEADLINE;
    loop {
        match spawn_resource(hub, request_id, spawn()).await {
            SpawnResult::Ok(id) => {
                assert_eq!(id.host().map(SatelliteHost::as_str), host, "{id:?}");
                return id;
            }
            SpawnResult::Err(SpawnError::SatelliteUnreachable(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => panic!("spawn on {host:?} failed: {other:?}"),
        }
    }
}

async fn organization(hub: &mut UnixStream, request_id: u32) -> OrganizationPage {
    let command = Command::GetOrganization {
        filter: OrgFilter::All,
        page_size: 0,
        cursor: None,
    };
    match command_via_hub(hub, request_id, command).await.0 {
        CommandResult::OkWith(CommandValue::OrganizationPage(page)) => *page,
        other => panic!("GET_ORGANIZATION failed: {other:?}"),
    }
}

async fn arrangement(
    hub: &mut UnixStream,
    request_id: u32,
    key: ArrangementKey,
) -> ArrangementValue {
    let command = Command::ArrangementGet { arrangement: key };
    match command_via_hub(hub, request_id, command).await.0 {
        CommandResult::OkWith(CommandValue::Arrangement(value)) => *value,
        other => panic!("ARRANGEMENT_GET failed: {other:?}"),
    }
}

const fn leaf(n: u8, target: EntryTarget) -> ArrangementNode {
    ArrangementNode::Leaf(EntryRef {
        entry: [n; 16],
        target,
    })
}

fn satellite_target(id: &ResourceId) -> EntryTarget {
    let ResourceId::Satellite { host, id } = id else {
        panic!("not a satellite id: {id:?}");
    };
    EntryTarget::Satellite {
        host: host.clone(),
        id: *id,
    }
}

/// One window: the local pane on the left, the two satellite panes stacked
/// on the right.
fn mixed_window(local: EntryTarget, a: &ResourceId, b: &ResourceId) -> ArrangementDoc {
    let right = ArrangementNode::Split {
        dir: SplitDirection::Vertical,
        ratio: 0.5,
        first: Box::new(leaf(2, satellite_target(a))),
        second: Box::new(leaf(3, satellite_target(b))),
    };
    let tree = ArrangementNode::Split {
        dir: SplitDirection::Horizontal,
        ratio: 0.5,
        first: Box::new(leaf(1, local)),
        second: Box::new(right),
    };
    ArrangementDoc {
        groups: vec![ArrangementGroup {
            id: [9; 16],
            name: Some("mixed".to_owned()),
            tree,
        }],
    }
}

/// Place `local`, `a`, and `b` in one window of `local`'s session (which must
/// be `hub-session`); returns the arrangement key and the placed document.
async fn arrange_mixed_window(
    hub: &mut UnixStream,
    local: &ResourceId,
    a: &ResourceId,
    b: &ResourceId,
) -> (ArrangementKey, ArrangementDoc) {
    let page = organization(hub, 100).await;
    let record = page
        .resources
        .iter()
        .find(|r| r.live.as_ref() == Some(local))
        .expect("the local pane has a resource record");
    let session = page
        .sessions
        .iter()
        .find(|s| s.key == record.session)
        .expect("its session is listed");
    assert_eq!(
        session.name, "hub-session",
        "an owner-less spawn joins the hub session"
    );
    let key = session.arrangement.expect("the session has an arrangement");
    let current = arrangement(hub, 101, key).await;
    let document = mixed_window(EntryTarget::Member(record.key), a, b);
    let replace = Command::ArrangementReplace {
        operation: OperationId::new([7; 16]).unwrap(),
        arrangement: key,
        expected: current.revision,
        document: document.clone(),
    };
    let (result, _) = command_via_hub(hub, 102, replace).await;
    assert!(
        matches!(result, CommandResult::OkWith(CommandValue::Committed(_))),
        "a session arrangement takes satellite entries beside members: {result:?}"
    );
    assert_layout_intact(hub, 103, key, &document).await;
    (key, document)
}

async fn assert_layout_intact(
    hub: &mut UnixStream,
    request_id: u32,
    key: ArrangementKey,
    document: &ArrangementDoc,
) {
    let value = arrangement(hub, request_id, key).await;
    assert_eq!(&value.document, document, "all three leaves stay placed");
}

/// Drain `RESOURCE_OUTPUT` until `needle` appears on `id`, skipping the
/// other panes' trailing output.
async fn await_echo(hub: &mut UnixStream, id: &ResourceId, needle: u8) {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    let found = recv_until_deadline(hub, deadline, |_, frame| match frame {
        FrameKind::ResourceOutput {
            terminal_id, bytes, ..
        } if &terminal_id == id => {
            acc.extend_from_slice(&bytes);
            acc.contains(&needle).then_some(())
        }
        _ => None,
    })
    .await;
    assert!(
        found.is_some(),
        "echo {:?} on {id:?} never arrived; got {:?}",
        needle as char,
        String::from_utf8_lossy(&acc)
    );
}

async fn type_and_echo(hub: &mut UnixStream, id: &ResourceId, c: char, key: PhysicalKey) {
    send_key_and_enter(hub, id, c, key).await;
    await_echo(hub, id, c as u8).await;
}

async fn screen(hub: &mut UnixStream, request_id: u32, id: &ResourceId) -> serde_json::Value {
    match get_screen_via_hub(hub, request_id, id.clone()).await {
        CommandResult::OkWith(CommandValue::Json(json)) => serde_json::from_str(&json).unwrap(),
        other => panic!("GET_SCREEN {id:?} failed: {other:?}"),
    }
}

/// Resize `id` and poll its snapshot until the grid and the echoed text
/// both show.
async fn resize_and_snapshot(
    hub: &mut UnixStream,
    request_id: u32,
    id: &ResourceId,
    (cols, rows): (u16, u16),
    text: char,
) {
    let resize = FrameKind::ResizeTerminal {
        terminal_id: id.clone(),
        cols,
        rows,
        cell_px: None,
    };
    send_frame(hub, &resize).await;
    let deadline = Instant::now() + STEP_DEADLINE;
    loop {
        let screen = screen(hub, request_id, id).await;
        let sized = (screen["cols"].as_u64(), screen["rows"].as_u64())
            == (Some(u64::from(cols)), Some(u64::from(rows)));
        let shows_text = screen["lines"].as_array().is_some_and(|lines| {
            lines
                .iter()
                .any(|l| l.as_str().is_some_and(|l| l.contains(text)))
        });
        if sized && shows_text {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{id:?} never showed {cols}x{rows} with {text:?}: {screen}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn attach_local(hub: &mut UnixStream, id: &ResourceId) {
    let (result, _) = command_via_hub(hub, 300, Command::attach_terminal(id.clone(), None)).await;
    assert_eq!(
        result,
        CommandResult::Ok,
        "ATTACH_RESOURCE on the local pane"
    );
}

/// Cut `b`'s link and wait for the hub's typed teardown notice.
async fn drop_link(hub: &mut UnixStream, relay: &LinkRelay) {
    relay.cut();
    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    let message = recv_until_deadline(hub, deadline, |_, frame| match frame {
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message,
        } if message.contains("sat-b") => Some(message),
        _ => None,
    })
    .await;
    assert!(
        message.is_some(),
        "no SatelliteUnreachable notice for sat-b"
    );
}

/// While `sat-b` is down: its pane is `SatelliteUnreachable`, the aggregate
/// degrades to the other two, and the window keeps all three leaves.
async fn assert_degraded(
    hub: &mut UnixStream,
    panes: [&ResourceId; 3],
    layout: (ArrangementKey, &ArrangementDoc),
) {
    let [local, a, b] = panes;
    assert_error(
        &get_screen_via_hub(hub, 400, b.clone()).await,
        ErrorCode::SatelliteUnreachable,
    );
    let (result, errors) = get_state_via_hub(hub, 401).await;
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("aggregated GET_STATE must never fail, got {result:?}");
    };
    let listed = |id: &ResourceId| snapshot.resources.iter().any(|p| &p.id == id);
    assert!(listed(local) && listed(a), "the live panes stay listed");
    assert!(!listed(b), "the unreachable pane is not listed as live");
    assert!(
        errors
            .iter()
            .any(|(code, message)| *code == ErrorCode::SatelliteUnreachable
                && message.contains("sat-b")),
        "{errors:?}"
    );
    assert_layout_intact(hub, 402, layout.0, layout.1).await;
}

async fn kill(hub: &mut UnixStream, request_id: u32, id: &ResourceId) {
    let command = Command::KillResource {
        terminal_id: id.clone(),
        operation_id: None,
    };
    let (result, _) = command_via_hub(hub, request_id, command).await;
    assert_eq!(result, CommandResult::Ok, "KILL_RESOURCE {id:?}");
}

/// Poll the aggregate until none of `panes` is listed.
async fn await_all_gone(hub: &mut UnixStream, panes: &[&ResourceId]) {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut request_id = 900;
    loop {
        let (result, _) = get_state_via_hub(hub, request_id).await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
            panic!("GET_STATE failed: {result:?}");
        };
        let left: Vec<_> = panes
            .iter()
            .filter(|id| snapshot.resources.iter().any(|p| &&p.id == *id))
            .collect();
        if left.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "still listed after kill: {left:?}"
        );
        request_id += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[test]
fn a_mixed_window_spans_two_satellites_and_survives_one_link_dropping() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (port_a, a_shutdown, a_task) = spawn_satellite(tmp.path().join("sat-a.sock")).await;
        let (port_b, b_shutdown, b_task) = spawn_satellite(tmp.path().join("sat-b.sock")).await;
        let relay = LinkRelay::start(port_b).await;
        let (hub_shutdown, hub_task) = spawn_hub_with_session(
            tmp.path().join("hub.sock"),
            vec![
                satellite_entry("sat-a", port_a),
                satellite_entry("sat-b", relay.port()),
            ],
            Some("hub-session"),
        );
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        let local = spawn_cat(&mut hub, 10, None).await;
        let a = spawn_cat(&mut hub, 11, Some("sat-a")).await;
        let b = spawn_cat(&mut hub, 12, Some("sat-b")).await;
        let (key, document) = arrange_mixed_window(&mut hub, &local, &a, &b).await;

        attach_local(&mut hub, &local).await;
        attach_terminal_until_ok(&mut hub, &a).await;
        attach_terminal_until_ok(&mut hub, &b).await;
        type_and_echo(&mut hub, &local, 'l', PhysicalKey::L).await;
        type_and_echo(&mut hub, &a, 'x', PhysicalKey::X).await;
        type_and_echo(&mut hub, &b, 'y', PhysicalKey::Y).await;
        resize_and_snapshot(&mut hub, 200, &local, (100, 30), 'l').await;
        resize_and_snapshot(&mut hub, 201, &a, (90, 28), 'x').await;
        resize_and_snapshot(&mut hub, 202, &b, (70, 20), 'y').await;

        drop_link(&mut hub, &relay).await;
        assert_degraded(&mut hub, [&local, &a, &b], (key, &document)).await;
        type_and_echo(&mut hub, &local, 'm', PhysicalKey::M).await;
        type_and_echo(&mut hub, &a, 'z', PhysicalKey::Z).await;

        relay.restore();
        let frames = attach_terminal_until_ok(&mut hub, &b).await;
        assert!(
            frames.iter().any(|f| matches!(
                f,
                FrameKind::BootstrapBegin { terminal_id, cols: 70, rows: 20, .. } if terminal_id == &b
            )),
            "the returning pane replays its snapshot at its size: {frames:?}"
        );
        resize_and_snapshot(&mut hub, 500, &b, (70, 20), 'y').await;
        type_and_echo(&mut hub, &b, 'w', PhysicalKey::W).await;
        assert_layout_intact(&mut hub, 501, key, &document).await;

        kill(&mut hub, 600, &local).await;
        kill(&mut hub, 601, &a).await;
        kill(&mut hub, 602, &b).await;
        await_all_gone(&mut hub, &[&local, &a, &b]).await;

        drop(hub);
        drop(relay);
        stop(hub_shutdown, hub_task).await;
        stop(a_shutdown, a_task).await;
        stop(b_shutdown, b_task).await;
    });
}
