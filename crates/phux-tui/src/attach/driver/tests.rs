//! Driver test fixtures and attach/onboarding/reflow scenarios. Config,
//! peer-watch and composited-chrome families live in private sibling modules.
#![allow(clippy::expect_used, reason = "tests")]

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::time::Duration;

use phux_protocol::caps::{Layer, ServerCapabilities, TerminalColor, TerminalDefaultColors};
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{AttachTarget, DetachReason, FrameKind, Scope, ViewportInfo};
use tokio::net::UnixStream;

use crate::attach::chrome_ctx::ChromeCtx;
use crate::attach::connection::{Connection, Dial};
use crate::attach::onboarding;
use crate::attach::outcome::AttachError;
use crate::attach::paint::{
    SidebarEdge, SidebarReservation, StatusBarPaint, content_rect, paint_bar_after_pane,
    paint_full_frame, sidebar_reservation,
};
use crate::attach::pane_state::{AttachKernel, PaneSlot, published_test_state};
use crate::attach::render::SelectionRect;
use crate::layout::ArrangementDoc;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter, widgets::WindowInfo};
use crate::render::overlay::{OverlayState, RenderOverlay, SelectItem, SelectList};
use crate::settings::{build_resolver_from, config_error_line, keybind_error_line};
use phux_client::agent_meta::{
    AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY,
};
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use phux_client::testkit::{ScriptSpec, ScriptedServer};
use phux_config::KeybindingsCfg;

use super::config_ui::*;
use super::entry::*;
use super::headless::*;
use super::overlay_paint::*;
use super::session_io::*;
use super::subscriptions::*;
use super::viewport::*;

mod chrome_frames;
mod configuration;
mod peer_watches;

use chrome_frames::{PROBE_PANE_TEXT, PROBE_SESSION, probe_window, shipped_frame_rows};

/// The local dial: these tests drive a `UnixStream::pair`.
fn test_dial() -> Dial {
    Dial::uds(Path::new("/tmp/phux-driver-test.sock"))
}

fn published_test_kernel(id: &ResourceId, cols: u16, rows: u16, bytes: &[u8]) -> AttachKernel {
    published_test_state(&[(id, cols, rows, bytes)]).0
}

/// A connected pair; drop the client and [`drain`] the server to read what
/// was sent.
fn pair() -> (Connection, Connection) {
    let (a, b) = UnixStream::pair().expect("pair");
    (Connection::from_stream(a), Connection::from_stream(b))
}

async fn drain(client: Connection, mut server: Connection) -> Vec<FrameKind> {
    drop(client);
    let mut frames = Vec::new();
    while let Ok(frame) = server.recv().await {
        frames.push(frame);
    }
    frames
}

fn session_name_painter() -> StatusBarPainter {
    use crate::render::chrome::status_bar::widgets::{StatusBar, WidgetRegistry};
    use phux_config::{StatusCfg, Widget};
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        ..StatusCfg::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar");
    StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom)
}

/// The attach-time notice seam: a configured bar takes the reconnect notice
/// for its TTL; no painter or no notice is a quiet no-op.
#[test]
fn apply_initial_notice_sets_the_painter_slot_at_attach() {
    let mut painter = session_name_painter();
    let before = std::time::Instant::now();
    let notice = || Some(Notice::info("re-attached after server restart"));
    assert!(apply_initial_notice(Some(&mut painter), notice()));
    assert!(!painter.clear_expired_notice(before), "held for its TTL");
    assert!(
        painter.clear_expired_notice(before + crate::render::chrome::status_bar::NOTICE_TTL * 2)
    );
    assert!(!apply_initial_notice(None, notice()));
    assert!(!apply_initial_notice(Some(&mut painter), None));
}

/// A painter carrying the return-onboarding notice, and the claim for it.
fn returning(path: &Path) -> (onboarding::AttachClaim, StatusBarPainter) {
    let intro = onboarding::begin_attach(path).expect("intro claim");
    assert!(intro.commit());
    assert_eq!(
        onboarding::after_detach(path),
        Some(onboarding::DETACH_NOTICE)
    );
    let claim = onboarding::begin_attach(path).expect("return claim");
    let mut painter = session_name_painter();
    assert!(apply_initial_notice(
        Some(&mut painter),
        Some(Notice::info(onboarding::RETURN_NOTICE))
    ));
    (claim, painter)
}

fn paint_bar(painter: &mut StatusBarPainter, cols: u16, out: &mut Vec<u8>) -> StatusBarPaint {
    let mut chrome = ChromeCtx {
        status_bar: Some(painter),
        ..ChromeCtx::bare((cols, 24), "demo")
    };
    paint_bar_after_pane(out, &mut chrome, None, None, false)
}

/// The return notice commits the onboarding claim only once it is painted
/// whole: an attach that exits first, or a truncated paint, stays retryable.
#[test]
fn return_notice_commits_onboarding_only_when_fully_published() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("onboarding.json");
    let (claim, _painter) = returning(&path);
    drop(claim);
    assert_eq!(
        onboarding::begin_attach(&path)
            .expect("still retryable")
            .moment(),
        onboarding::AttachMoment::Return
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("onboarding.json");
    let (claim, mut painter) = returning(&path);
    let mut claim = Some(claim);
    let mut out = Vec::new();
    let truncated = paint_bar(&mut painter, 20, &mut out);
    finish_return_onboarding_after_paint(&mut claim, Some(&painter), truncated);
    assert!(claim.is_some(), "a truncated notice must remain retryable");

    out.clear();
    let delivered = paint_bar(&mut painter, 80, &mut out);
    assert!(matches!(delivered, StatusBarPaint::Published { .. }));
    let mut escaped = false;
    let plain: String = String::from_utf8_lossy(&out)
        .chars()
        .filter(|ch| {
            let keep = !escaped && *ch != '\x1b';
            escaped = if escaped {
                !ch.is_ascii_alphabetic()
            } else {
                *ch == '\x1b'
            };
            keep
        })
        .collect();
    assert!(plain.contains(onboarding::RETURN_NOTICE), "{plain:?}");
    finish_return_onboarding_after_paint(&mut claim, Some(&painter), delivered);
    assert!(claim.is_none());
    assert!(onboarding::begin_attach(&path).is_none());
}

#[test]
fn sidebar_reservation_changes_view_rects_for_pty_reflow() {
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let viewport = (100, 30);
    let rect = |sidebar| {
        view_rects(
            &workspace,
            None,
            content_rect(viewport, None, sidebar),
            viewport,
        )[&id]
    };
    assert_eq!(rect(None).w, 100);
    let inset = rect(Some(SidebarReservation {
        edge: SidebarEdge::Left,
        width: 20,
    }));
    assert_eq!((inset.x, inset.w), (20, 80));
}

fn default_cfg() -> phux_config::Config {
    phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
        .expect("default config parses")
}

/// A write to a departed peer must not become the reason the loop ended (it
/// hid "the last pane exited 7" behind "attach loop io error"); local IO
/// faults and non-IO endings stay fatal.
#[test]
fn a_write_to_a_departed_peer_is_not_a_loop_ending_error() {
    use io::ErrorKind::*;
    for (kind, gone) in [
        (BrokenPipe, true),
        (ConnectionReset, true),
        (ConnectionAborted, true),
        (PermissionDenied, false),
        (OutOfMemory, false),
        (InvalidData, false),
    ] {
        assert_eq!(
            peer_gone(&AttachError::Io(io::Error::from(kind))),
            gone,
            "{kind:?}"
        );
    }
    assert!(!peer_gone(&AttachError::Disconnected));
    assert!(!peer_gone(&AttachError::Protocol("bad frame".to_owned())));
}

#[test]
fn headless_completion_drains_history_and_metadata_after_attach_ready() {
    use phux_protocol::wire::frame::{HistoryRejectionReason, HistoryTombstoneReason};
    let id = ResourceId::local(7);
    let stream_id = phux_protocol::StreamId::new(1).expect("stream");
    let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
    let page = |cursor: &'static [u8], next: Option<&'static [u8]>| FrameKind::HistoryPage {
        terminal_id: id.clone(),
        stream_id,
        bootstrap_id,
        rows: 0,
        page_seq: 1,
        cursor: bytes::Bytes::from_static(cursor),
        next_cursor: next.map(bytes::Bytes::from_static),
        payload: bytes::Bytes::new(),
    };
    let mut completion = HeadlessCompletion::default();
    completion.note_history_request(&id, stream_id, bootstrap_id);
    completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
    assert!(
        !completion.is_complete(false),
        "READY may precede history and metadata"
    );
    assert!(
        !completion.is_complete(true),
        "metadata does not complete history"
    );
    completion.observe_frame(&page(b"newest", Some(b"older")), 7);
    completion.note_history_request(&id, stream_id, bootstrap_id);
    assert!(
        !completion.is_complete(true),
        "an intermediate page keeps it pending"
    );
    completion.observe_frame(&page(b"older", None), 7);
    assert!(completion.is_complete(true));

    // A page the engine does not follow up (its prefetch window is full)
    // settles the chain even though older history remains on the server.
    let mut completion = HeadlessCompletion::default();
    completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
    completion.note_history_request(&id, stream_id, bootstrap_id);
    completion.observe_frame(&page(b"newest", Some(b"older")), 7);
    assert!(
        completion.is_complete(true),
        "an unrequested next cursor must not hold the snapshot open"
    );

    // Tombstone and rejection are terminal answers too.
    for frame in [
        FrameKind::HistoryTombstone {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            cursor: bytes::Bytes::from_static(b"cursor"),
            reason: HistoryTombstoneReason::Pruned,
        },
        FrameKind::HistoryRejected {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            cursor: bytes::Bytes::from_static(b"cursor"),
            reason: HistoryRejectionReason::TooSmall,
            required_bytes: 128,
            required_rows: 1,
        },
    ] {
        let mut completion = HeadlessCompletion::default();
        completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
        completion.note_history_request(&id, stream_id, bootstrap_id);
        completion.observe_frame(&frame, 7);
        assert!(completion.is_complete(true));
    }
}

/// Negotiation sends exactly one HELLO, captures `server_id` verbatim
/// (ADR-0053), and refuses a second local negotiation.
#[tokio::test(flavor = "current_thread")]
async fn attach_negotiation_waits_for_hello_ok_and_sends_one_hello() {
    let (client_stream, server_stream) = UnixStream::pair().expect("pair");
    let mut client = Connection::from_stream(client_stream);
    let server = tokio::spawn(ScriptedServer::on_stream(server_stream, ScriptSpec::new()).run());
    assert!(client.server_id().is_none());
    client
        .negotiate(attach_client_name(), attach_client_caps(None, &test_dial()))
        .await
        .expect("handshake succeeds on HELLO_OK");
    let selected = client.negotiated_bootstrap().expect("profile state");
    assert_eq!(selected.limits, phux_protocol::BootstrapLimits::default());
    assert_eq!(client.server_id(), Some(&[][..]));
    let duplicate = client
        .negotiate(attach_client_name(), attach_client_caps(None, &test_dial()))
        .await;
    assert!(matches!(duplicate, Err(AttachError::Protocol(_))));
    drop(client);
    let seen = server.await.expect("scripted server task");
    assert!(
        matches!(seen.as_slice(), [FrameKind::Hello { .. }]),
        "{seen:?}"
    );
}

mod negotiation;

/// A non-HELLO_OK reply is explained as version skew with a remedy.
#[tokio::test(flavor = "current_thread")]
async fn attach_negotiation_rejects_non_hello_ok_reply() {
    let (mut client, mut server) = pair();
    let server_side = async move {
        assert!(matches!(
            server.recv().await.expect("hello"),
            FrameKind::Hello { .. }
        ));
        server
            .send(&FrameKind::Detached {
                reason: Some(DetachReason::ProtocolError),
                message: String::new(),
            })
            .await
            .expect("send detached");
    };
    let negotiation =
        client.negotiate(attach_client_name(), attach_client_caps(None, &test_dial()));
    let (res, ()) = tokio::join!(negotiation, server_side);
    match res {
        Err(AttachError::Protocol(msg)) => {
            assert!(
                msg.contains("unexpected HELLO reply") && msg.contains("run `phux doctor`"),
                "{msg}"
            );
        }
        other => panic!("expected protocol error, got {other:?}"),
    }
}

mod narrow_sidebar;

mod copy_mode_status;
mod pacing;
