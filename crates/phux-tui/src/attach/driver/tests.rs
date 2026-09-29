//! Cross-cutting driver tests: attach negotiation, detach classification,
//! onboarding notices, the foreign-topology sweeps, and the chrome-under-
//! overlay probes.
#![allow(clippy::expect_used, reason = "tests")]

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, Layer, ServerCapabilities, TerminalColor, TerminalDefaultColors,
    select_bootstrap_profile,
};
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{AttachTarget, DetachReason, FrameKind, Scope, ViewportInfo};
use tokio::net::UnixStream;

use crate::attach::chrome_ctx::ChromeCtx;
use crate::attach::connection::{Connection, Dial};
use crate::attach::onboarding;
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::paint::{
    SidebarEdge, SidebarReservation, StatusBarPaint, content_rect, paint_bar_after_pane,
    paint_full_frame, sidebar_reservation,
};
use crate::attach::pane_state::{AttachKernel, PaneSlot, published_test_state};
use crate::attach::render::{ReplicaWalk, SelectionRect};
use crate::attach::server_frame::FrameOutcome;
use crate::layout::Workspace;
use crate::predict::PredictiveConfig;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::{OverlayState, RenderOverlay, SelectItem, SelectList};
use crate::settings::{build_resolver_from, config_error_line, keybind_error_line};
use phux_client::agent_meta::{
    AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY,
};
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use phux_client::testkit::{ScriptSpec, ScriptedServer};
use phux_config::KeybindingsCfg;
use phux_config::keybind::ResolvedAction;
use phux_config::widget::WindowInfo;

use super::config_ui::*;
use super::entry::*;
use super::headless::*;
use super::overlay_paint::*;
use super::session_io::*;
use super::subscriptions::*;
use super::viewport::*;

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

#[test]
fn detach_classification_requires_local_intent_and_plain_detach() {
    let detached = |reason| AttachEnd::Detached { reason };
    assert!(is_local_detach(detached(None), true));
    assert!(!is_local_detach(detached(None), false));
    // The reason never overrides the local-intent test.
    assert!(is_local_detach(
        detached(Some(DetachReason::Requested)),
        true
    ));
    assert!(!is_local_detach(
        detached(Some(DetachReason::ServerShutdown)),
        false
    ));
    assert!(!is_local_detach(
        AttachEnd::LastPaneClosed {
            exit_status: Some(0)
        },
        true
    ));
}

/// A session created from inside the TUI seeds its pane in the client's cwd,
/// not the daemon's.
#[test]
fn create_session_target_carries_client_cwd() {
    let expected = std::env::current_dir()
        .expect("cwd")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        create_session_target("picker".to_owned()),
        AttachTarget::CreateIfMissing {
            name: "picker".to_owned(),
            command: None,
            cwd: Some(expected),
        }
    );
}

fn session_name_painter() -> StatusBarPainter {
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        ..StatusCfg::default()
    };
    let bar =
        phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar");
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
    let theme = crate::render::theme::Theme::default();
    let mut chrome = ChromeCtx {
        viewport: (cols, 24),
        sidebar: None,
        status_bar: Some(painter),
        sidebar_painter: None,
        session_name: "demo",
        theme: &theme,
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
    let workspace = Workspace::single(id.clone());
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

/// `toggle-sidebar` is client-local and must survive a `switch-session` in
/// both directions; config seeds only the first attach.
#[test]
fn sidebar_enabled_carries_across_a_session_switch() {
    assert!(!seed_sidebar_enabled(None, false));
    assert!(seed_sidebar_enabled(None, true));
    assert!(seed_sidebar_enabled(Some(true), false));
    assert!(!seed_sidebar_enabled(Some(false), true));
}

/// One malformed chord disables only itself; detach survives.
#[test]
fn attach_resolver_survives_one_bad_chord_and_keeps_detach() {
    let cfg = phux_config::parse_str(
        "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\nd = \"detach\"\n",
        Path::new("test.toml"),
    )
    .expect("parses");
    let (mut resolver, diags) = build_resolver_from(&cfg.keybindings);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].binding, "q-");
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
    match resolver.feed(phux_config::keybind::parse_chord("d").expect("chord")) {
        phux_config::keybind::Feed::Resolved(ra) => assert_eq!(ra.action, "detach"),
        other => panic!("detach must survive one bad chord, got {other:?}"),
    }
}

/// The error lines name the first bad chord, count the rest, and point at
/// `phux config check` (the diagnosing verb, not `config show`).
#[test]
fn config_error_lines_name_the_problem_and_point_at_config_check() {
    let diags = |toml: &str| {
        let cfg = phux_config::parse_str(toml, Path::new("test.toml")).expect("parses");
        build_resolver_from(&cfg.keybindings).1
    };
    let one = keybind_error_line(&diags(
        "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n",
    ));
    assert!(
        one.contains("\"q-\"") && one.contains("run: phux config check"),
        "{one}"
    );
    assert!(!one.contains("more;"), "{one}");
    let three = keybind_error_line(&diags(
        "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n\"w-\" = \"kill-pane\"\n\"e-\" = \"kill-pane\"\n",
    ));
    assert!(
        three.contains("\"e-\"") && three.contains("+2 more; run: phux config check"),
        "{three}"
    );
    assert_eq!(keybind_error_line(&[]), "");

    let line = config_error_line(&"boom");
    assert!(
        line.contains("config error: boom") && line.contains("phux config check"),
        "{line}"
    );
    assert!(!line.contains("config show"), "{line}");
}

/// A resolver from the shipped defaults, walked to the pending-prefix state.
fn pending_resolver() -> phux_config::keybind::Resolver {
    let cfg = default_cfg();
    let mut r = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver");
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    assert_eq!(r.feed(prefix), phux_config::keybind::Feed::Partial);
    r
}

fn default_cfg() -> phux_config::Config {
    phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
        .expect("default config parses")
}

/// The which-key deadline arms once at `now + delay` and keeps that anchor;
/// an early chord, a disabled config, or an overlay disarms it.
#[test]
fn which_key_deadline_arms_once_and_disarms_on_resolve_disable_or_overlay() {
    let now = tokio::time::Instant::now();
    let delay = Duration::from_millis(600);
    let mut deadline = None;
    update_which_key_deadline(&mut deadline, true, true, false, now, delay);
    assert_eq!(deadline, Some(now + delay));
    update_which_key_deadline(&mut deadline, true, true, false, now + delay / 2, delay);
    assert_eq!(deadline, Some(now + delay), "anchor survives re-passes");
    update_which_key_deadline(&mut deadline, false, true, false, now, delay);
    assert_eq!(deadline, None, "an early chord suppresses the popup");

    update_which_key_deadline(&mut deadline, true, false, false, now, delay);
    assert_eq!(deadline, None, "disabled in config");
    update_which_key_deadline(&mut deadline, true, true, true, now, delay);
    assert_eq!(deadline, None, "a modal owns input");
    update_which_key_deadline(&mut deadline, true, true, false, now, delay);
    update_which_key_deadline(&mut deadline, true, true, true, now, delay);
    assert_eq!(deadline, None, "an overlay appearing disarms");
}

/// The timeout pushes a passthrough popup and leaves the prefix pending; it
/// declines with no pending prefix or over a modal.
#[test]
fn which_key_timeout_pushes_a_passthrough_popup_only_when_pending() {
    let cfg = default_cfg();
    let theme = crate::render::Theme::default();
    let resolver = pending_resolver();
    let mut overlays = OverlayState::new();
    assert!(push_which_key_overlay(
        &mut overlays,
        Some(&resolver),
        Some(&cfg.keybindings),
        &theme
    ));
    assert!(
        overlays.top_is_passthrough(),
        "the popup can never eat a chord"
    );
    assert!(resolver.pending_at_prefix(), "the pending prefix survives");

    let idle = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver");
    let mut overlays = OverlayState::new();
    assert!(!push_which_key_overlay(
        &mut overlays,
        Some(&idle),
        Some(&cfg.keybindings),
        &theme
    ));
    assert!(!overlays.is_active());

    let mut overlays = OverlayState::new();
    overlays.push(palette_overlay());
    assert!(!push_which_key_overlay(
        &mut overlays,
        Some(&resolver),
        Some(&cfg.keybindings),
        &theme
    ));
    assert_eq!(overlays.depth(), 1, "nothing stacked on the modal");
}

/// Foreign layout and agent GET replies cache on a decodable value and clear
/// on garbage or a tombstone; an identical agent record reports no change.
#[test]
fn foreign_replies_cache_clear_and_survive_garbage() {
    let sid = SessionId::new(7);
    let mut peers = PeerWatch::default();
    let mut ws = Workspace::single(ResourceId::local(1));
    ws.add_window("logs".to_owned(), ResourceId::local(2));
    let bytes = ws.encode_cbor().expect("encode");
    peers.apply_layout_reply(sid, Some(&bytes));
    assert_eq!(
        peers.foreign_layouts.get(&sid).map(|w| w.windows.len()),
        Some(2)
    );
    peers.apply_layout_reply(sid, Some(b"not cbor"));
    assert!(!peers.foreign_layouts.contains_key(&sid));
    peers.apply_layout_reply(sid, Some(&bytes));
    peers.apply_layout_reply(sid, None);
    assert!(!peers.foreign_layouts.contains_key(&sid));

    let id = ResourceId::local(3);
    let record = AgentRecord {
        name: "packer".to_owned(),
        kind: Some("codex".to_owned()),
        state: AgentMetaState::Working,
        ..AgentRecord::default()
    };
    assert!(peers.apply_agent_reply(id.clone(), Some(&record.encode())));
    assert_eq!(peers.foreign_agents[&id].name, "packer");
    assert!(!peers.apply_agent_reply(id.clone(), Some(&record.encode())));
    peers.apply_agent_reply(id.clone(), Some(b"not json"));
    assert!(!peers.foreign_agents.contains_key(&id));
    peers.apply_agent_reply(id.clone(), Some(&record.encode()));
    peers.apply_agent_reply(id.clone(), None);
    assert!(!peers.foreign_agents.contains_key(&id));
}

/// Peer layout keys are SUBSCRIBED once (there is no unsubscribe), still
/// GET each sweep, and our own session is never subscribed here.
#[tokio::test]
async fn peer_layout_keys_are_subscribed_not_just_read() {
    let (mut client, server) = pair();
    let mut peers = PeerWatch {
        sessions: vec![
            phux_protocol::wire::info::SessionInfo::new(SessionId::new(1), "work")
                .with_window_count(1),
            phux_protocol::wire::info::SessionInfo::new(SessionId::new(2), "scratch")
                .with_window_count(1),
        ],
        focused_session: Some(SessionId::new(1)),
        ..PeerWatch::default()
    };
    let mut next = 1;
    for _ in 0..2 {
        peers
            .sweep_layouts(&mut client, &mut next)
            .await
            .expect("sweep sends");
    }
    let frames = drain(client, server).await;
    let (peer_key, own_key) = (layout_key(SessionId::new(2)), layout_key(SessionId::new(1)));
    let subscribe_count = |key: &str| {
        frames
            .iter()
            .filter(|f| {
                matches!(f, FrameKind::SubscribeMetadata { scope, key: k }
                if *scope == Scope::Group(DEFAULT_GROUP_ID) && k == key)
            })
            .count()
    };
    assert_eq!(subscribe_count(&peer_key), 1, "{frames:?}");
    assert_eq!(subscribe_count(&own_key), 0, "{frames:?}");
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, FrameKind::GetMetadata { key, .. } if *key == peer_key))
    );
}

/// Foreign agent watches: each terminal (deduped) is fetched once and
/// subscribed, local or satellite; only satellites also read the asked
/// flag (a local ask arrives as an event).
#[tokio::test]
async fn foreign_agent_ids_are_fetched_and_subscribed_once() {
    let (mut client, server) = pair();
    let local = ResourceId::local(10);
    let satellite = ResourceId::satellite("prod-3", 2);
    let (mut peers, mut next) = (PeerWatch::default(), 1);
    peers
        .watch_agents(
            &mut client,
            vec![local.clone(), satellite.clone(), local.clone()],
            &mut next,
        )
        .await
        .expect("sweep sends");
    let frames = drain(client, server).await;
    let count = |id: &ResourceId, get: bool, key: &str| {
        frames
            .iter()
            .filter(|f| match f {
                FrameKind::GetMetadata { scope, key: k, .. } => {
                    get && *scope == Scope::Resource(id.clone()) && k == key
                }
                FrameKind::SubscribeMetadata { scope, key: k } => {
                    !get && *scope == Scope::Resource(id.clone()) && k == key
                }
                _ => false,
            })
            .count()
    };
    for id in [&local, &satellite] {
        assert_eq!(count(id, true, RESOURCE_AGENT_KEY), 1, "{id}: {frames:?}");
        assert_eq!(count(id, false, RESOURCE_AGENT_KEY), 1, "{id}: {frames:?}");
        assert!(peers.foreign_agent_subscribed.contains(id));
    }
    assert_eq!(count(&satellite, true, RESOURCE_ASKED_KEY), 1, "{frames:?}");
    assert_eq!(count(&local, true, RESOURCE_ASKED_KEY), 0, "{frames:?}");
}

/// Pruning keeps only live foreign panes' records and drops the send-once
/// subscription marker with them, so a re-spawned id re-subscribes.
#[test]
fn prune_foreign_agents_retains_only_live_foreign_panes() {
    let (live, stale) = (ResourceId::local(1), ResourceId::local(2));
    let mut peers = PeerWatch {
        foreign_agents: [
            (live.clone(), AgentRecord::default()),
            (stale.clone(), AgentRecord::default()),
        ]
        .into(),
        foreign_agent_subscribed: [live.clone(), stale.clone()].into(),
        ..PeerWatch::default()
    };
    peers.prune_agents(&HashSet::from([live.clone()]));
    let (cache, subscribed) = (&peers.foreign_agents, &peers.foreign_agent_subscribed);
    assert!(cache.contains_key(&live) && !cache.contains_key(&stale));
    assert!(subscribed.contains(&live) && !subscribed.contains(&stale));
    peers.prune_agents(&HashSet::new());
    let (cache, subscribed) = (&peers.foreign_agents, &peers.foreign_agent_subscribed);
    assert!(cache.is_empty() && subscribed.is_empty());
}

#[test]
fn frame_ack_is_emitted_only_for_state_sync_consumers() {
    let ack = Some((
        ResourceId::local(7),
        phux_protocol::StreamId::new(1).expect("stream"),
        phux_protocol::BootstrapId::new(1).expect("bootstrap"),
        42u64,
    ));
    assert_eq!(should_emit_frame_ack(false, ack.clone()), None);
    assert_eq!(should_emit_frame_ack(true, ack.clone()), ack);
    assert_eq!(should_emit_frame_ack(true, None), None);
}

/// Terminal replies need the negotiated feature (else one notice), and an
/// outcome that ends the loop sends none and adds no notice: the session has
/// no PTY left to answer.
#[test]
fn terminal_replies_require_the_feature_and_a_live_session() {
    let reply = (ResourceId::local(7), b"\x1b[0n".to_vec());
    let outcome = || FrameOutcome {
        pty_writes: vec![reply.clone()],
        ..FrameOutcome::default()
    };
    let mut supported = outcome();
    assert_eq!(
        take_terminal_replies(&mut supported, true),
        vec![reply.clone()]
    );
    assert!(supported.notices.is_empty());

    let mut old_server = outcome();
    assert!(take_terminal_replies(&mut old_server, false).is_empty());
    assert_eq!(old_server.notices.len(), 1);
    assert!(old_server.notices[0].text.contains("terminal-reply"));

    let end = Some(AttachEnd::LastPaneClosed {
        exit_status: Some(7),
    });
    let mut exiting = FrameOutcome {
        exit: true,
        exit_reason: end,
        ..outcome()
    };
    assert!(take_terminal_replies(&mut exiting, true).is_empty());
    assert!(exiting.pty_writes.is_empty() && exiting.notices.is_empty());
    assert_eq!(exiting.exit_reason, end);
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
    let mut completion = HeadlessCompletion::new(Some(1));
    completion.note_history_request(&id, stream_id, bootstrap_id);
    completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
    assert!(
        !completion.is_complete(false),
        "READY may precede history and metadata"
    );
    completion.observe_frame(
        &FrameKind::MetadataValue {
            request_id: 1,
            value: None,
        },
        7,
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
        let mut completion = HeadlessCompletion::new(None);
        completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
        completion.note_history_request(&id, stream_id, bootstrap_id);
        completion.observe_frame(&frame, 7);
        assert!(completion.is_complete(true));
    }
}

/// ADR-0060: without a recorder, `run_buffered` fails at connect exactly as
/// before the tee existed.
#[tokio::test(flavor = "current_thread")]
async fn run_buffered_without_a_recorder_passes_the_bare_sink() {
    let socket =
        std::env::temp_dir().join(format!("phux-rec-guard-{}-absent.sock", std::process::id()));
    let err = run_buffered(
        &Dial::uds(&socket),
        AttachTarget::Last,
        PredictiveConfig::disabled(),
        None,
        None,
        None,
    )
    .await
    .expect_err("there is no server at that socket");
    assert!(
        matches!(
            err,
            AttachError::Io(_) | AttachError::Connect(_) | AttachError::Unreachable(_)
        ),
        "{err:?}"
    );
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

#[tokio::test(flavor = "current_thread")]
async fn attach_negotiation_preserves_custom_caps_then_sends_attach() {
    let colors = TerminalDefaultColors {
        foreground: TerminalColor { r: 1, g: 2, b: 3 },
        background: TerminalColor { r: 4, g: 5, b: 6 },
    };
    let (mut client, mut server) = pair();
    let client_side = async {
        client
            .negotiate(
                attach_client_name(),
                attach_client_caps(Some(colors), &test_dial()),
            )
            .await
            .expect("HELLO_OK");
        client
            .send(&FrameKind::Attach {
                attach_id: 1,
                target: AttachTarget::Last,
                viewport: ViewportInfo::new(120, 40),
                request_scrollback: true,
                scrollback_limit_lines: 10_000,
                role_policy: None,
            })
            .await
            .expect("ATTACH");
    };
    let server_side = async {
        let hello = server.recv().await.expect("HELLO");
        let FrameKind::Hello { client_caps, .. } = &hello else {
            panic!("expected HELLO");
        };
        let (selected_profile, bootstrap_limits) =
            select_bootstrap_profile(client_caps, &BootstrapCapabilities::new())
                .expect("intersect");
        server
            .send(&FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile,
                bootstrap_limits,
            })
            .await
            .expect("HELLO_OK");
        (hello, server.recv().await.expect("ATTACH"))
    };
    let ((), (hello, attach)) = tokio::join!(client_side, server_side);
    let FrameKind::Hello { client_caps, .. } = hello else {
        panic!("first frame must be HELLO");
    };
    assert_eq!(client_caps.default_colors, Some(colors));
    assert!(client_caps.layers.contains(Layer::L3));
    assert!(matches!(attach, FrameKind::Attach { .. }));
}

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

// ---- composited frames through the PTY-probe oracle ------------------------

const PROBE_SIDEBAR_W: u16 = 20;
/// Distinctive sidebar window label and host; neither appears in pane text.
const PROBE_WINDOW: &str = "w1-agent";
const PROBE_HOST: &str = "probe-host";
const PROBE_PANE_TEXT: &str = "PANE-BASE";

/// Replay `bytes` into a fresh terminal and project it to trimmed rows.
fn probe_rows(bytes: &[u8], (cols, rows): (u16, u16)) -> Vec<String> {
    let mut probe = PaneSlot::new_with_size(cols, rows).expect("probe slot");
    probe.terminal.vt_write(bytes);
    let mut frame = phux_core::screen::RenderedFrame::blank(cols, rows);
    probe
        .renderer
        .render_at_cells(
            ReplicaWalk::for_test(&probe.terminal),
            &mut frame,
            (0, 0),
            (cols, rows),
        )
        .expect("project probe cells");
    frame
        .cells
        .chunks(usize::from(cols))
        .map(|row| {
            row.iter()
                .map(|c| c.grapheme.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn probe_window(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        zoomed: false,
        attention: false,
        branch: None,
        exited: None,
        badge: None,
    }
}

fn probe_sidebar(windows: &[WindowInfo]) -> SidebarPainter {
    let mut painter = SidebarPainter::new(crate::render::theme::Theme::default());
    painter.set_roster(vec![crate::render::chrome::sidebar::SessionRosterEntry {
        name: "probe".to_owned(),
        host: PROBE_HOST.to_owned(),
        active: true,
        selectable: true,
        ..Default::default()
    }]);
    painter.set_windows(windows.to_vec());
    painter
}

/// A full frame with the SHIPPED status bar (from `default.toml`), so the
/// real lineup, slot policy, and shrink ladders are exercised together.
fn shipped_frame_rows(
    view: (u16, u16),
    windows: &[WindowInfo],
    sidebar: Option<SidebarReservation>,
) -> Vec<String> {
    let (cols, rows) = view;
    let id = ResourceId::local(1);
    let workspace = Workspace::single(id.clone());
    let cfg =
        phux_config::parse_with_defaults("", Path::new("/nonexistent/c.toml")).expect("defaults");
    let mut status_bar = crate::settings::compose_status_bar(&cfg, &[])
        .expect("the shipped status lineup must build")
        .expect("the shipped lineup is non-empty");
    status_bar.set_windows(windows.to_vec());
    let pane_rows = rows.saturating_sub(1);
    let mut panes = HashMap::from([(
        id.clone(),
        PaneSlot::new_with_size(cols, pane_rows).expect("slot"),
    )]);
    let kernel = published_test_kernel(&id, cols, pane_rows, PROBE_PANE_TEXT.as_bytes());
    let theme = crate::render::theme::Theme::default();
    let mut sidebar_painter = probe_sidebar(windows);
    let mut out: Vec<u8> = Vec::new();
    let mut chrome = ChromeCtx {
        viewport: view,
        sidebar,
        status_bar: Some(&mut status_bar),
        sidebar_painter: Some(&mut sidebar_painter),
        session_name: "phux",
        theme: &theme,
    };
    paint_full_frame(
        &mut out,
        &workspace.render_window(None).expect("layout"),
        &mut panes,
        &kernel,
        Some(&id),
        &mut chrome,
    );
    probe_rows(&out, view)
}

/// The shipped bar across widths: roomy shows tabs and session with no
/// teaching strip; phone-sized swaps the clock for a `switch` chip; narrower
/// still collapses far tabs behind `›` while keeping the active tab whole.
#[test]
fn shipped_frame_bar_degrades_gracefully_with_width() {
    let windows = [
        probe_window("zsh", false),
        probe_window("nvim", true),
        probe_window("server", false),
        probe_window("logs", false),
    ];
    let roomy = shipped_frame_rows((100, 12), &windows[..3], None);
    let bar = &roomy[0];
    assert!(bar.contains(" 1 nvim ") && bar.contains("phux"), "{bar:?}");
    for absent in ["s Sessions", "S Settings", "switch"] {
        assert!(!bar.contains(absent), "{bar:?}");
    }
    assert!(roomy.join("\n").contains(PROBE_PANE_TEXT));

    for (view, collapsed) in [((46, 12), false), ((36, 10), true)] {
        let rows = shipped_frame_rows(view, &windows, None);
        let bar = &rows[0];
        assert!(bar.contains(" 1 nvim "), "active tab whole: {bar:?}");
        assert!(bar.contains("switch"), "{bar:?}");
        assert!(!bar.contains("Space palette"), "{bar:?}");
        assert!(bar.chars().count() <= usize::from(view.0), "{bar:?}");
        if collapsed {
            assert!(
                bar.contains('\u{203a}') && !bar.contains("3 logs"),
                "{bar:?}"
            );
        }
    }
}

/// On a narrow terminal the sidebar yields: panes own the whole width, no
/// strip paints, and reflow, mouse routing, and the divider all use the same
/// yielded reservation (a stale strip would put the divider near col 35).
#[test]
fn shipped_frame_yields_the_sidebar_on_a_narrow_terminal() {
    use crate::layout::{LayoutNode, LayoutState, SplitDir, split_at};
    use crate::multi_pane::{RouteDecision, route_mouse_event};
    use crate::render::ChromeBreakpoints;
    use crate::render::chrome::status_bar::Position;
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};

    let view = (50u16, 12u16);
    let min_pane = ChromeBreakpoints::DEFAULT.min_pane_cols;
    let sidebar = sidebar_reservation(view.0, true, 20, SidebarEdge::Left, min_pane);
    assert!(sidebar.is_none());
    assert!(sidebar_reservation(view.0, true, 0, SidebarEdge::Left, min_pane).is_none());

    let rows = shipped_frame_rows(view, &[probe_window("nvim", true)], sidebar);
    let frame = rows.join("\n");
    assert!(
        rows.iter().any(|r| r.starts_with(PROBE_PANE_TEXT)),
        "{frame}"
    );
    assert!(!frame.contains(PROBE_HOST), "{frame}");
    let rail = rows.iter().find(|r| r.contains('─')).expect("pane rail");
    assert!(
        rail.starts_with('─') && rail.chars().count() == usize::from(view.0),
        "{rail:?}"
    );

    let content = content_rect(view, Some(Position::Top), sidebar);
    assert_eq!((content.x, content.w), (0, view.0));
    let id = ResourceId::local(1);
    let workspace = Workspace::single(id.clone());
    let pane_rect = view_rects(&workspace, None, content, view)[&id];
    assert_eq!((pane_rect.x, pane_rect.w), (0, view.0));
    let press_at = |x: u16| MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(content.y),
    };
    let ls = workspace.render_window(None).expect("layout");
    assert!(matches!(
        route_mouse_event(ls.as_ref(), content, view, &press_at(0)),
        RouteDecision::Pane { target, .. } if target == id
    ));
    let right = ResourceId::local(2);
    let split = LayoutState {
        tree: Some(
            split_at(
                &LayoutNode::Leaf(id.clone()),
                &id,
                &right,
                SplitDir::Horizontal,
                0.5,
            )
            .expect("split"),
        ),
        focus: Some(id),
    };
    let divider_x = (0..view.0)
        .find(|&x| {
            matches!(
                route_mouse_event(&split, content, view, &press_at(x)),
                RouteDecision::Divider { .. }
            )
        })
        .expect("a divider column");
    assert!((20..30).contains(&divider_x), "{divider_x}");
}

/// One `paint_active_overlay` frame over a left sidebar, with the sidebar
/// painter threaded when `with_painter`, projected to strip columns and rows.
fn overlay_frame(overlay: Box<dyn RenderOverlay>, with_painter: bool) -> (Vec<String>, String) {
    const VIEW: (u16, u16) = (80, 24);
    let theme = crate::render::Theme::default();
    let id = ResourceId::local(1);
    let workspace = Workspace::single(id.clone());
    let sidebar = Some(SidebarReservation {
        edge: SidebarEdge::Left,
        width: PROBE_SIDEBAR_W,
    });
    let pane_cols = VIEW.0 - PROBE_SIDEBAR_W;
    let mut panes = HashMap::from([(
        id.clone(),
        PaneSlot::new_with_size(pane_cols, VIEW.1).expect("slot"),
    )]);
    let kernel = published_test_kernel(&id, pane_cols, VIEW.1, PROBE_PANE_TEXT.as_bytes());
    let mut sidebar_painter = probe_sidebar(&[probe_window(PROBE_WINDOW, true)]);
    let mut overlays = OverlayState::new();
    overlays.push(overlay);
    let mut out: Vec<u8> = Vec::new();
    let mut chrome = ChromeCtx {
        viewport: VIEW,
        sidebar,
        status_bar: None,
        sidebar_painter: with_painter.then_some(&mut sidebar_painter),
        session_name: "probe",
        theme: &theme,
    };
    paint_active_overlay(
        &mut out,
        &overlays,
        workspace.render_window(None).as_deref(),
        &mut panes,
        &kernel,
        Some(&id),
        &mut chrome,
    );
    let rows = probe_rows(&out, VIEW);
    let strip = rows
        .iter()
        .map(|r| {
            r.chars()
                .take(usize::from(PROBE_SIDEBAR_W))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (rows, strip)
}

/// The command palette, as the dispatcher builds it.
fn palette_overlay() -> Box<dyn RenderOverlay> {
    let item = |action: &str| {
        SelectItem::new(
            action,
            ResolvedAction {
                action: action.to_owned(),
                args: std::collections::BTreeMap::new(),
            },
        )
    };
    let theme = crate::render::Theme::default();
    Box::new(SelectList::new(
        "command palette",
        vec![item("detach"), item("new-window")],
        &theme,
    ))
}

/// Opening the palette must NOT blank the sidebar: the floating-modal base
/// frame repaints the strip and panes, then centers the modal in the pane
/// area. Without the painter the strip is blank (the bug's shape, so the
/// probe cannot false-pass).
#[test]
fn command_palette_keeps_sidebar_visible() {
    let (_, blank) = overlay_frame(palette_overlay(), false);
    assert!(
        !blank.contains(PROBE_WINDOW) && !blank.contains(PROBE_HOST),
        "{blank}"
    );

    let (rows, strip) = overlay_frame(palette_overlay(), true);
    let all = rows.join("\n");
    assert!(
        strip.contains(PROBE_WINDOW) && strip.contains(PROBE_HOST),
        "{all}"
    );
    assert!(
        all.contains("command palette") && all.contains(PROBE_PANE_TEXT),
        "{all}"
    );
    assert!(
        !strip.contains('┌') && !strip.contains('└'),
        "modal intruded into the strip:\n{strip}"
    );
    insta::assert_snapshot!("palette_over_sidebar", all);
}

/// Every floating overlay kind (fleet, which-key, prompt, toast) shares the
/// base-frame path and must keep the sidebar too.
#[test]
fn all_floating_overlays_keep_sidebar_visible() {
    let theme = crate::render::Theme::default();
    let wk_cfg = KeybindingsCfg {
        prefix_table: std::iter::once((
            "d".to_owned(),
            phux_config::Action::Bare("detach".to_owned()),
        ))
        .collect(),
        ..KeybindingsCfg::default()
    };
    let fleet_items = crate::attach::fleet::fleet_items(
        &Workspace::single(ResourceId::local(1)),
        &[],
        None,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(!fleet_items.iter().all(SelectItem::is_header));
    let overlays: Vec<(&str, Box<dyn RenderOverlay>)> = vec![
        (
            "agent-fleet",
            Box::new(
                SelectList::new("agent fleet", fleet_items, &theme)
                    .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
            ),
        ),
        (
            "which-key",
            Box::new(crate::render::overlay::WhichKeyOverlay::from_config(
                &wk_cfg, &theme,
            )),
        ),
        (
            "prompt",
            Box::new(crate::render::overlay::PromptOverlay::new(
                "rename window",
                "rename-window",
                "name",
                "1",
                &theme,
            )),
        ),
        (
            "toast",
            Box::new(crate::render::overlay::ToastOverlay::new(
                "notice",
                vec!["a line".to_owned()],
                &theme,
            )),
        ),
    ];
    for (label, overlay) in overlays {
        let (rows, strip) = overlay_frame(overlay, true);
        assert!(
            strip.contains(PROBE_WINDOW) && strip.contains(PROBE_HOST),
            "{label}:\n{}",
            rows.join("\n")
        );
    }
}

/// The copy-mode strip counts a block selection as rows x band columns
/// (12 here, with `start_col > end_col` not underflowing), distinct from the
/// linear count (3).
#[test]
fn copy_mode_status_block_cell_count_differs_from_linear() {
    let theme = crate::render::Theme::default();
    let status_of = |sel: SelectionRect| {
        let mut out: Vec<u8> = Vec::new();
        paint_copy_mode_status(&mut out, sel, (80, 24), &theme).expect("status");
        String::from_utf8_lossy(&out).into_owned()
    };
    let rect = |start_row, start_col, end_row, end_col, rectangle| SelectionRect {
        start_row,
        start_col,
        end_row,
        end_col,
        rectangle,
    };
    assert!(status_of(rect(0, 5, 2, 2, true)).contains("· 12 "));
    assert!(status_of(rect(0, 5, 2, 2, false)).contains("· 3 "));
    assert!(status_of(rect(1, 2, 3, 6, true)).contains("· 15 "));
}

/// An admitted burst settles the pacer's debt itself: its deadline was just
/// pushed out and a saturating socket starves the timer arm.
#[test]
fn an_admitted_burst_settles_the_withheld_debt() {
    use super::loop_state::burst_settles_debt;
    assert!(burst_settles_debt(true, false));
    assert!(burst_settles_debt(false, true));
    assert!(!burst_settles_debt(false, false));
}

/// Pointer motion must not arm the reply grace (a drag would keep pacing off
/// for the whole client); press and release do.
#[test]
fn pointer_motion_does_not_arm_the_reply_grace() {
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
    let expects = |action| {
        super::loop_state::input_expects_a_reply(&InputEvent::Mouse(MouseEvent {
            action,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: 4.0,
            y: 2.0,
        }))
    };
    assert!(!expects(MouseAction::Motion));
    assert!(expects(MouseAction::Press) && expects(MouseAction::Release));
}
