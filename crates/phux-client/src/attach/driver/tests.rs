//! Cross-cutting driver tests: attach negotiation, detach classification,
//! onboarding notices, coalesced replies, the foreign-topology sweeps,
//! and the chrome-under-overlay probes.
#![allow(clippy::expect_used, reason = "tests")]

use std::collections::HashMap;
use std::io::{self};
use std::path::Path;
use std::time::Duration;

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::Layer;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, Scope, ViewportInfo};

use crate::agent_meta::{AgentMetaState, AgentRecord};
use crate::attach::connection::{Connection, Dial};
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::paint::{
    SidebarEdge, SidebarReservation, StatusBarPaint, content_rect, paint_bar_after_pane,
    paint_full_frame,
};
use crate::attach::pane_state::{AttachKernel, PaneSlot};
use crate::attach::render::ReplicaWalk;
use crate::attach::render::SelectionRect;
use crate::attach::server_frame::FrameOutcome;
use crate::layout::Workspace;
use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use crate::predict::PredictiveConfig;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::OverlayState;

use super::config_ui::*;
use super::entry::*;
use super::headless::*;
use super::overlay_paint::*;
use super::session_io::*;
use super::subscriptions::*;
use super::viewport::*;
use crate::attach::pane_state::published_test_state;
use crate::layout_ops::LAYOUT_KEY;

use crate::testkit::{ScriptSpec, ScriptedServer};
use phux_protocol::PROTOCOL_VERSION;

fn published_test_kernel(
    terminal_id: &TerminalId,
    cols: u16,
    rows: u16,
    bytes: &[u8],
) -> AttachKernel {
    published_test_state(&[(terminal_id, cols, rows, bytes)]).0
}
use phux_protocol::caps::{
    BootstrapCapabilities, ServerCapabilities, TerminalColor, TerminalDefaultColors,
    select_bootstrap_profile,
};
use phux_protocol::wire::frame::DetachReason;
use tokio::net::UnixStream;

#[test]
fn detach_classification_requires_local_intent_and_plain_detach() {
    assert!(is_local_detach(AttachEnd::Detached { reason: None }, true));
    assert!(!is_local_detach(
        AttachEnd::Detached { reason: None },
        false
    ));
    // The reason qualifies the ending, never the local-intent test: a
    // server that names REQUESTED for a detach we asked for is the same
    // local detach as a server that names nothing.
    assert!(is_local_detach(
        AttachEnd::Detached {
            reason: Some(DetachReason::Requested),
        },
        true,
    ));
    assert!(!is_local_detach(
        AttachEnd::Detached {
            reason: Some(DetachReason::ServerShutdown),
        },
        false,
    ));
    assert!(!is_local_detach(
        AttachEnd::LastPaneClosed {
            exit_status: Some(0),
        },
        true,
    ));
}

/// phux-0db: a session created from inside the TUI (picker "new
/// session") seeds its pane in the client's cwd, not `None` (= the
/// daemon's CWD).
#[test]
fn create_session_target_carries_client_cwd() {
    let expected = std::env::current_dir()
        .expect("test cwd")
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

/// phux-i0e8.2.3: the attach-time notice seam `main_loop` calls right
/// after the bootstrap chrome refresh. A configured bar accepts the
/// reconnect notice (and paints it full-row on the next bar paint); no
/// painter, or no notice, is a quiet no-op.
#[test]
fn apply_initial_notice_sets_the_painter_slot_at_attach() {
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};

    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        ..StatusCfg::default()
    };
    let bar =
        phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar");
    let mut painter =
        StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);
    let before = std::time::Instant::now();
    assert!(
        apply_initial_notice(
            Some(&mut painter),
            Some(Notice::info("re-attached after server restart")),
        ),
        "a configured bar must accept the reconnect notice"
    );
    // The slot is genuinely occupied: it survives until NOTICE_TTL and
    // clears on the tick after — the same expiry path the live
    // status_tick drives (full-row rendering itself is pinned by the
    // phux-i0e8.2.1 painter tests).
    assert!(
        !painter.clear_expired_notice(before),
        "the notice must hold the slot for its TTL"
    );
    assert!(
        painter.clear_expired_notice(before + crate::render::chrome::status_bar::NOTICE_TTL * 2),
        "the seeded notice must expire like any other transient notice"
    );

    // No painter: degrades (returns false), never panics.
    assert!(!apply_initial_notice(
        None,
        Some(Notice::info("re-attached after server restart")),
    ));
    // No notice: a first attach is a no-op even with a painter.
    assert!(!apply_initial_notice(Some(&mut painter), None));
}

fn returning_onboarding_claim(path: &std::path::Path) -> super::super::onboarding::AttachClaim {
    let intro = super::super::onboarding::begin_attach(path).expect("intro claim");
    assert!(intro.commit());
    assert_eq!(
        super::super::onboarding::after_detach(path),
        Some(super::super::onboarding::DETACH_NOTICE)
    );
    super::super::onboarding::begin_attach(path).expect("return claim")
}

#[test]
fn accepted_return_notice_is_retryable_when_attach_exits_before_paint() {
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("onboarding.json");
    let claim = returning_onboarding_claim(&path);
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        ..StatusCfg::default()
    };
    let bar =
        phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar");
    let mut painter =
        StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);

    assert!(apply_initial_notice(
        Some(&mut painter),
        Some(Notice::info(super::super::onboarding::RETURN_NOTICE)),
    ));
    drop(claim);

    assert_eq!(
        super::super::onboarding::begin_attach(&path)
            .expect("return remains retryable")
            .moment(),
        super::super::onboarding::AttachMoment::Return
    );
}

#[test]
fn delivered_return_notice_commits_onboarding_claim() {
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("onboarding.json");
    let claim = returning_onboarding_claim(&path);
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        ..StatusCfg::default()
    };
    let bar =
        phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar");
    let mut painter =
        StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);
    assert!(apply_initial_notice(
        Some(&mut painter),
        Some(Notice::info(super::super::onboarding::RETURN_NOTICE)),
    ));

    let mut out = Vec::new();
    let delivered = paint_bar_after_pane(
        Some(&mut painter),
        &mut out,
        (80, 24),
        None,
        "demo",
        None,
        None,
        false,
    );
    assert!(matches!(delivered, StatusBarPaint::Published { .. }));
    let mut escaped = false;
    let plain: String = String::from_utf8_lossy(&out)
        .chars()
        .filter(|ch| {
            if escaped {
                if ch.is_ascii_alphabetic() {
                    escaped = false;
                }
                false
            } else if *ch == '\x1b' {
                escaped = true;
                false
            } else {
                true
            }
        })
        .collect();
    assert!(
        plain.contains(super::super::onboarding::RETURN_NOTICE),
        "painted text: {plain:?}"
    );
    let mut claim = Some(claim);
    finish_return_onboarding_after_paint(&mut claim, Some(&painter), delivered);

    assert!(super::super::onboarding::begin_attach(&path).is_none());
}

#[test]
fn truncated_return_notice_retries_until_the_full_notice_is_published() {
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("onboarding.json");
    let mut claim = Some(returning_onboarding_claim(&path));
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        ..StatusCfg::default()
    };
    let bar =
        phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar");
    let mut painter =
        StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);
    assert!(apply_initial_notice(
        Some(&mut painter),
        Some(Notice::info(super::super::onboarding::RETURN_NOTICE)),
    ));

    let mut out = Vec::new();
    let truncated = paint_bar_after_pane(
        Some(&mut painter),
        &mut out,
        (20, 24),
        None,
        "demo",
        None,
        None,
        false,
    );
    finish_return_onboarding_after_paint(&mut claim, Some(&painter), truncated);
    assert!(claim.is_some(), "a truncated notice must remain retryable");

    let delivered = paint_bar_after_pane(
        Some(&mut painter),
        &mut out,
        (80, 24),
        None,
        "demo",
        None,
        None,
        false,
    );
    finish_return_onboarding_after_paint(&mut claim, Some(&painter), delivered);

    assert!(claim.is_none());
    assert!(super::super::onboarding::begin_attach(&path).is_none());
}

#[test]
fn sidebar_reservation_changes_view_rects_for_pty_reflow() {
    let id = TerminalId::local(1);
    let workspace = Workspace::single(id.clone());
    let viewport = (100, 30);
    let full = view_rects(
        &workspace,
        None,
        content_rect(viewport, None, None),
        viewport,
    );
    let inset = view_rects(
        &workspace,
        None,
        content_rect(
            viewport,
            None,
            Some(SidebarReservation {
                edge: SidebarEdge::Left,
                width: 20,
            }),
        ),
        viewport,
    );

    assert_eq!(full.get(&id).expect("full rect").w, 100);
    assert_eq!(inset.get(&id).expect("inset rect").w, 80);
    assert_eq!(inset.get(&id).expect("inset rect").x, 20);
}

/// `toggle-sidebar` is client-local chrome, not session state, and a
/// `switch-session` re-enters `main_loop` — which re-reads
/// `[sidebar] enabled`. Without the carry, the user's toggle is silently
/// reverted on every space switch: the strip blinks shut exactly when
/// they are moving between the spaces it exists to show them.
///
/// Both directions must carry. The runtime value is authoritative once it
/// exists, so a config default can neither re-open a strip the user shut
/// nor close one they opened.
#[test]
fn sidebar_enabled_carries_across_a_session_switch() {
    // First attach: nothing carried, so `[sidebar] enabled` decides —
    // including the shipped default, which must stay byte-identical.
    assert!(!seed_sidebar_enabled(None, false));
    assert!(seed_sidebar_enabled(None, true));
    // Switched after `toggle-sidebar` opened it: the runtime value wins
    // over a config that defaults the strip off. This is the regression.
    assert!(seed_sidebar_enabled(Some(true), false));
    // ...and symmetrically, closing it by hand survives a config that
    // defaults it on — a switch must not re-open what the user shut.
    assert!(!seed_sidebar_enabled(Some(false), true));
}

#[test]
fn attach_error_io_display_includes_source() {
    let err = AttachError::Io(io::Error::other("boom"));
    let msg = err.to_string();
    assert!(msg.contains("attach loop io error"));
}

// -- lenient resolver at attach (phux-i0e8.3.4) -----------------------

#[test]
fn attach_resolver_survives_one_bad_chord_and_keeps_detach() {
    // Before phux-i0e8.3.4, one malformed chord ("q-") made
    // build_resolver_from return None: EVERY binding died, including
    // detach. Now the attach path always gets a resolver and only the
    // offending binding is disabled.
    let cfg = phux_config::parse_str(
        r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            d = "detach"
            "#,
        Path::new("test.toml"),
    )
    .expect("test config parses");
    let (mut resolver, diags) = build_resolver_from(&cfg.keybindings);
    assert_eq!(diags.len(), 1, "exactly the bad binding is reported");
    assert_eq!(diags[0].binding, "q-");

    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
    match resolver.feed(phux_config::keybind::parse_chord("d").expect("chord")) {
        phux_config::keybind::Feed::Resolved(ra) => assert_eq!(ra.action, "detach"),
        other => panic!("detach must survive one bad chord, got {other:?}"),
    }
}

#[test]
fn keybind_error_line_names_the_chord_and_config_check() {
    let cfg = phux_config::parse_str(
        r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            "#,
        Path::new("test.toml"),
    )
    .expect("test config parses");
    let (_, diags) = build_resolver_from(&cfg.keybindings);
    let line = keybind_error_line(&diags);
    assert!(
        line.contains("\"q-\""),
        "line must name the offending chord: {line}"
    );
    assert!(
        line.contains("run: phux config check"),
        "line must point at the checker: {line}"
    );
    assert!(
        !line.contains("more;"),
        "a single diagnostic carries no +N count: {line}"
    );
}

#[test]
fn keybind_error_line_counts_additional_disabled_bindings() {
    let cfg = phux_config::parse_str(
        r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            "w-" = "kill-pane"
            "e-" = "kill-pane"
            "#,
        Path::new("test.toml"),
    )
    .expect("test config parses");
    let (_, diags) = build_resolver_from(&cfg.keybindings);
    assert_eq!(diags.len(), 3);
    let line = keybind_error_line(&diags);
    // BTreeMap order: "e-" first, the other two summarized.
    assert!(
        line.contains("\"e-\""),
        "line must name the first offending chord: {line}"
    );
    assert!(
        line.contains("+2 more; run: phux config check"),
        "line must count the remaining disabled bindings: {line}"
    );
}

#[test]
fn keybind_error_line_is_empty_without_diagnostics() {
    assert_eq!(keybind_error_line(&[]), "");
}

#[test]
fn config_error_line_recommends_config_check() {
    // phux-i0e8.3.5: the remedy is the verb that diagnoses
    // (`config check`), not the one that merely renders the
    // effective config (`config show`).
    let line = config_error_line(&"boom");
    assert!(
        line.contains("phux config check"),
        "line must point at the checker: {line}"
    );
    assert!(
        !line.contains("config show"),
        "line must not recommend config show: {line}"
    );
    assert!(
        line.contains("config error: boom"),
        "line must carry the error display: {line}"
    );
}

// -- which-key popup arming (phux-foz.2) ------------------------------

/// Build a resolver from the shipped defaults and walk it to the
/// pending-prefix state (`C-a` fed, continuation awaited).
fn pending_resolver() -> phux_config::keybind::Resolver {
    let cfg = phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
        .expect("default config parses");
    let mut r = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    assert_eq!(r.feed(prefix), phux_config::keybind::Feed::Partial);
    assert!(r.pending_at_prefix());
    r
}

#[test]
fn which_key_deadline_arms_once_and_holds_its_anchor() {
    let mut deadline = None;
    let now = tokio::time::Instant::now();
    let delay = Duration::from_millis(600);
    update_which_key_deadline(&mut deadline, true, true, false, now, delay);
    assert_eq!(deadline, Some(now + delay), "arms at now + delay");
    // A later pass (other select! arms fired) keeps the ORIGINAL
    // anchor — the popup is not postponed by unrelated wakeups.
    update_which_key_deadline(
        &mut deadline,
        true,
        true,
        false,
        now + Duration::from_millis(300),
        delay,
    );
    assert_eq!(deadline, Some(now + delay), "anchor survives re-passes");
}

#[test]
fn which_key_deadline_disarms_when_an_early_chord_resolves() {
    // The suppression path: prefix pressed (armed), then a fast
    // continuation resolves the chord BEFORE the timeout — the next
    // loop pass sees pending=false and must disarm, so the popup
    // never appears.
    let mut deadline = None;
    let now = tokio::time::Instant::now();
    let delay = Duration::from_millis(600);
    update_which_key_deadline(&mut deadline, true, true, false, now, delay);
    assert!(deadline.is_some());
    update_which_key_deadline(&mut deadline, false, true, false, now, delay);
    assert_eq!(deadline, None, "early chord suppresses the popup");
}

#[test]
fn which_key_deadline_respects_disable_and_active_overlay() {
    let mut deadline = None;
    let now = tokio::time::Instant::now();
    let delay = Duration::from_millis(600);
    // Disabled in config: never arms.
    update_which_key_deadline(&mut deadline, true, false, false, now, delay);
    assert_eq!(deadline, None);
    // A modal already up: never arms (it owns input; the resolver was
    // reset on entry anyway).
    update_which_key_deadline(&mut deadline, true, true, true, now, delay);
    assert_eq!(deadline, None);
    // Armed, then an overlay appears before the timeout: disarms.
    update_which_key_deadline(&mut deadline, true, true, false, now, delay);
    assert!(deadline.is_some());
    update_which_key_deadline(&mut deadline, true, true, true, now, delay);
    assert_eq!(deadline, None);
}

#[test]
fn which_key_timeout_pushes_the_popup_and_keeps_the_prefix_pending() {
    // The timeout path: a pending-at-prefix resolver + keybindings
    // snapshot ⇒ the popup is pushed; the resolver still holds the
    // pending prefix so the NEXT chord completes normally.
    let cfg = phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
        .expect("default config parses");
    let resolver = pending_resolver();
    let mut overlays = OverlayState::new();
    let theme = crate::render::Theme::default();
    let pushed = push_which_key_overlay(
        &mut overlays,
        Some(&resolver),
        Some(&cfg.keybindings),
        &theme,
    );
    assert!(pushed, "timeout must push the which-key popup");
    assert!(overlays.is_active());
    assert!(
        overlays.top_is_passthrough(),
        "the popup must be input-passthrough so it can never eat a chord"
    );
    assert!(
        resolver.pending_at_prefix(),
        "pushing the popup must not consume the pending prefix"
    );
}

#[test]
fn which_key_push_declines_without_pending_prefix_or_over_a_modal() {
    let cfg = phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
        .expect("default config parses");
    let theme = crate::render::Theme::default();

    // Resolver at the root (no pending prefix): no push.
    let idle = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");
    let mut overlays = OverlayState::new();
    assert!(!push_which_key_overlay(
        &mut overlays,
        Some(&idle),
        Some(&cfg.keybindings),
        &theme,
    ));
    assert!(!overlays.is_active());

    // A modal already up: no push (would stack over user input).
    let pending = pending_resolver();
    let mut overlays = OverlayState::new();
    overlays.push(palette_overlay());
    assert!(!push_which_key_overlay(
        &mut overlays,
        Some(&pending),
        Some(&cfg.keybindings),
        &theme,
    ));
    assert_eq!(overlays.depth(), 1, "nothing stacked on the modal");
}

/// phux-jy4t: the layout metadata key is per-session, so two sessions
/// never share (and clobber) one bucket.
#[test]
fn layout_key_is_per_session() {
    use phux_protocol::ids::SessionId;
    let a = layout_key(SessionId::new(1));
    let b = layout_key(SessionId::new(2));
    assert_eq!(a, "phux.tui.layout/v1/1");
    assert_eq!(b, "phux.tui.layout/v1/2");
    assert_ne!(a, b, "different sessions get different keys");
    assert!(a.starts_with(LAYOUT_KEY), "still under the layout prefix");
}

/// phux-foz.8: a foreign session's layout GET reply round-trips into
/// the picker cache; a tombstone (`None`) or garbage clears/skips the
/// entry so the picker falls back to the plain switch row.
#[test]
fn apply_foreign_layout_reply_caches_clears_and_survives_garbage() {
    use phux_protocol::ids::SessionId;
    let sid = SessionId::new(7);
    let mut cache: HashMap<SessionId, Workspace> = HashMap::new();

    // A decodable envelope lands in the cache with its windows intact.
    let mut ws = Workspace::single(TerminalId::local(1));
    ws.add_window("logs".to_owned(), TerminalId::local(2));
    let bytes = ws.encode_cbor().expect("encode");
    apply_foreign_layout_reply(&mut cache, sid, Some(&bytes));
    assert_eq!(cache.get(&sid).map(|w| w.windows.len()), Some(2));

    // Garbage clears the stale entry rather than keeping it.
    apply_foreign_layout_reply(&mut cache, sid, Some(b"not cbor"));
    assert!(!cache.contains_key(&sid), "undecodable reply clears");

    // Re-cache, then a tombstone (nothing persisted) clears again.
    apply_foreign_layout_reply(&mut cache, sid, Some(&bytes));
    assert!(cache.contains_key(&sid));
    apply_foreign_layout_reply(&mut cache, sid, None);
    assert!(!cache.contains_key(&sid), "tombstone clears");
}

/// phux-jpqd: a foreign pane's agent-record GET reply round-trips into
/// the fleet cache; a tombstone (`None`) or an unparseable record
/// clears the entry so the fleet row falls back to `?`/"no agent".
#[test]
fn apply_foreign_agent_reply_caches_clears_and_survives_garbage() {
    let id = TerminalId::local(3);
    let mut cache: HashMap<TerminalId, AgentRecord> = HashMap::new();

    // A well-formed record lands with its identity intact.
    let record = AgentRecord {
        name: "packer".to_owned(),
        kind: Some("codex".to_owned()),
        state: AgentMetaState::Working,
        ..AgentRecord::default()
    };
    apply_foreign_agent_reply(&mut cache, id.clone(), Some(&record.encode()));
    assert_eq!(cache.get(&id).map(|r| r.name.as_str()), Some("packer"));

    // Garbage (no non-empty `name`) clears the stale entry.
    apply_foreign_agent_reply(&mut cache, id.clone(), Some(b"not json"));
    assert!(!cache.contains_key(&id), "unparseable record clears");

    // Re-cache, then a tombstone (no record) clears again.
    apply_foreign_agent_reply(&mut cache, id.clone(), Some(&record.encode()));
    assert!(cache.contains_key(&id));
    apply_foreign_agent_reply(&mut cache, id.clone(), None);
    assert!(!cache.contains_key(&id), "tombstone clears");
}

/// phux-jpqd: pruning keeps only the agent records whose panes still
/// appear in some cached foreign layout — a peer closing a pane (or a
fn session_info(id: u32, name: &str) -> phux_protocol::wire::info::SessionInfo {
    phux_protocol::wire::info::SessionInfo::new(phux_protocol::ids::SessionId::new(id), name)
        .with_window_count(1)
}

/// phux-k0cw: peer layout keys are SUBSCRIBED, not merely read once.
///
/// The one-shot sweep this replaces was an attach-time photograph that
/// rotted silently — tolerable while peers appeared only inside a modal
/// the user had just opened, wrong once they feed the always-on strip.
/// The subscribe must go out even when the GET will answer `None`: a peer
/// that has not persisted a layout yet is exactly the one whose first
/// write matters.
#[tokio::test]
async fn peer_layout_keys_are_subscribed_not_just_read() {
    let (client_stream, server_stream) = UnixStream::pair().expect("pair");
    let mut client = Connection::from_stream(client_stream);
    let mut server = Connection::from_stream(server_stream);

    let sessions = vec![session_info(1, "work"), session_info(2, "scratch")];
    let mut next_request_id = 1;
    let mut pending = HashMap::new();
    let mut subscribed = std::collections::HashSet::new();

    let sent = async {
        sync_foreign_layout_subscriptions(
            &mut client,
            &sessions,
            Some(phux_protocol::ids::SessionId::new(1)),
            &mut next_request_id,
            &mut pending,
            &mut subscribed,
        )
        .await
        .expect("sweep sends");
        // A second sweep against the same graph must not re-subscribe:
        // there is no UNSUBSCRIBE verb, so a resend is pure wire noise.
        sync_foreign_layout_subscriptions(
            &mut client,
            &sessions,
            Some(phux_protocol::ids::SessionId::new(1)),
            &mut next_request_id,
            &mut pending,
            &mut subscribed,
        )
        .await
        .expect("second sweep sends");
        drop(client);
    };

    let collect = async {
        let mut frames = Vec::new();
        while let Ok(frame) = server.recv().await {
            frames.push(frame);
        }
        frames
    };
    let ((), frames) = tokio::join!(sent, collect);

    let peer_key = crate::layout_ops::layout_key(phux_protocol::ids::SessionId::new(2));
    let peer_subscribes: Vec<_> = frames
        .iter()
        .filter(|f| {
            matches!(f, FrameKind::SubscribeMetadata { scope, key }
                    if *scope == Scope::Group(DEFAULT_GROUP_ID) && *key == peer_key)
        })
        .collect();
    assert_eq!(
        peer_subscribes.len(),
        1,
        "the peer's layout key is subscribed exactly once across two sweeps: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, FrameKind::GetMetadata { key, .. } if *key == peer_key)),
        "the GET is still sent — the subscribe is the edge, the GET is the level: {frames:?}"
    );

    // Our OWN session is never subscribed through this path; the driver
    // already holds its layout subscription, and a second one would
    // double every local broadcast.
    let own_key = crate::layout_ops::layout_key(phux_protocol::ids::SessionId::new(1));
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, FrameKind::SubscribeMetadata { key, .. } if *key == own_key)),
        "the focused session is excluded: {frames:?}"
    );
}

/// phux-k0cw: a satellite pane's metadata scope is normatively refused
/// (`docs/spec/L3.md`), so subscribing to one earns an
/// `UNSUPPORTED_SATELLITE_ROUTE` per sweep — errors the correlated-refusal
/// intercept swallows silently, which is the worst kind of wire noise.
#[tokio::test]
async fn satellite_panes_are_never_subscribed() {
    let (client_stream, server_stream) = UnixStream::pair().expect("pair");
    let mut client = Connection::from_stream(client_stream);
    let mut server = Connection::from_stream(server_stream);

    let local = TerminalId::local(1);
    let satellite = TerminalId::satellite("prod-3", 2);
    let mut ws = Workspace::single(local.clone());
    ws.add_window("remote".to_owned(), satellite.clone());

    let mut next_request_id = 1;
    let mut pending = HashMap::new();
    let mut subscribed = std::collections::HashSet::new();

    let sent = async {
        sync_foreign_agent_subscriptions(
            &mut client,
            &ws,
            &mut next_request_id,
            &mut pending,
            &mut subscribed,
        )
        .await
        .expect("sweep sends");
        drop(client);
    };
    let collect = async {
        let mut frames = Vec::new();
        while let Ok(frame) = server.recv().await {
            frames.push(frame);
        }
        frames
    };
    let ((), frames) = tokio::join!(sent, collect);

    assert!(
        frames.iter().any(|f| matches!(
            f,
            FrameKind::SubscribeMetadata { scope, .. } if *scope == Scope::Terminal(local.clone())
        )),
        "the local pane is subscribed: {frames:?}"
    );
    assert!(
        !frames.iter().any(|f| matches!(
            f,
            FrameKind::SubscribeMetadata { scope, .. }
                | FrameKind::GetMetadata { scope, .. }
                if *scope == Scope::Terminal(satellite.clone())
        )),
        "a satellite pane is never asked for or subscribed: {frames:?}"
    );
    assert!(
        !subscribed.contains(&satellite),
        "and it never enters the send-once bookkeeping"
    );
}

/// session leaving the graph) evicts its record so the cache stays
/// bounded to the live foreign pane set.
#[test]
fn prune_foreign_agents_retains_only_live_foreign_panes() {
    use phux_protocol::ids::SessionId;
    let live = TerminalId::local(1);
    let stale = TerminalId::local(2);
    let mut cache: HashMap<TerminalId, AgentRecord> = HashMap::new();
    cache.insert(live.clone(), AgentRecord::default());
    cache.insert(stale.clone(), AgentRecord::default());
    let mut subscribed: std::collections::HashSet<TerminalId> =
        [live.clone(), stale.clone()].into_iter().collect();

    // One foreign layout holds only `live`.
    let mut foreign_layouts: HashMap<SessionId, Workspace> = HashMap::new();
    foreign_layouts.insert(SessionId::new(9), Workspace::single(live.clone()));

    prune_foreign_agents(&mut cache, &mut subscribed, &foreign_layouts);
    assert!(
        cache.contains_key(&live),
        "a pane still in a layout survives"
    );
    assert!(
        !cache.contains_key(&stale),
        "a pane in no layout is evicted"
    );
    // phux-k0cw: the send-once subscription bookkeeping is pruned with
    // the record. Left behind, it would suppress the re-subscribe if that
    // pane id ever came back, and the row would go permanently silent.
    assert!(subscribed.contains(&live));
    assert!(
        !subscribed.contains(&stale),
        "a dead pane's subscription marker is dropped so a re-spawn re-subscribes"
    );

    // No cached layouts at all evicts everything.
    prune_foreign_agents(&mut cache, &mut subscribed, &HashMap::new());
    assert!(cache.is_empty(), "no foreign layouts => no foreign agents");
    assert!(subscribed.is_empty());
}

#[test]
fn raw_consumer_does_not_emit_frame_ack() {
    let ack = Some((
        TerminalId::local(7),
        phux_protocol::StreamId::new(1).expect("stream"),
        phux_protocol::BootstrapId::new(1).expect("bootstrap"),
        42u64,
    ));
    assert_eq!(should_emit_frame_ack(false, ack), None);
}

#[test]
fn state_sync_consumer_emits_frame_ack() {
    let ack = Some((
        TerminalId::local(7),
        phux_protocol::StreamId::new(1).expect("stream"),
        phux_protocol::BootstrapId::new(1).expect("bootstrap"),
        42u64,
    ));
    assert_eq!(should_emit_frame_ack(true, ack.clone()), ack);
    assert_eq!(should_emit_frame_ack(true, None), None);
}

#[test]
fn terminal_replies_require_negotiated_server_feature() {
    let reply = (TerminalId::local(7), b"\x1b[0n".to_vec());
    let mut supported = FrameOutcome {
        pty_writes: vec![reply.clone()],
        ..FrameOutcome::default()
    };
    assert_eq!(
        take_terminal_replies(&mut supported, true),
        vec![reply.clone()]
    );
    assert!(supported.notices.is_empty());

    let mut old_server = FrameOutcome {
        pty_writes: vec![reply],
        ..FrameOutcome::default()
    };
    assert!(take_terminal_replies(&mut old_server, false).is_empty());
    assert!(old_server.pty_writes.is_empty());
    assert_eq!(old_server.notices.len(), 1);
    assert!(old_server.notices[0].text.contains("terminal-reply"));
}

/// phux-501l hardening: an outcome that ends the loop writes nothing.
///
/// Both call sites send terminal replies before they read `outcome.exit`,
/// so an outcome carrying both would write into a session it is already
/// abandoning. Suppression belongs here, at the seam the two share, rather
/// than at either one.
///
/// Note the `terminal_reply_supported = true` argument: this must hold on
/// the path where replies are otherwise perfectly sendable. It is the exit,
/// not the feature negotiation, that makes them pointless.
#[test]
fn an_exiting_outcome_sends_no_terminal_reply() {
    let mut exiting = FrameOutcome {
        pty_writes: vec![(TerminalId::local(7), b"\x1b[0n".to_vec())],
        exit: true,
        exit_reason: Some(AttachEnd::LastPaneClosed {
            exit_status: Some(7),
        }),
        ..FrameOutcome::default()
    };
    assert!(
        take_terminal_replies(&mut exiting, true).is_empty(),
        "an ended session has no PTY to answer; writing here races the server's own exit",
    );
    assert!(exiting.pty_writes.is_empty());
    // No notice: this is the normal end of a session, not a degradation
    // the user needs told about. The `LastPaneClosed` explanation is what
    // the CLI prints, and it must survive intact.
    assert!(exiting.notices.is_empty());
    assert_eq!(
        exiting.exit_reason,
        Some(AttachEnd::LastPaneClosed {
            exit_status: Some(7)
        })
    );
}

/// phux-501l, the actual defect: a write that fails because the peer is
/// already gone must not become the reason the attach loop ended.
///
/// The last pane's shell exits, so the server emits `TERMINAL_OUTPUT` then
/// `TERMINAL_CLOSED` back to back and exits, closing the socket. One client
/// read pulls both frames. Acking the output writes into the dead socket
/// and, before this, killed the loop with `Io(BrokenPipe)` — so the
/// `TERMINAL_CLOSED` sitting in the *same batch* was never processed and
/// "the last pane exited 7" was replaced by "attach loop io error".
///
/// The classifier is what lets the loop keep going and end for the reason
/// the frames give. A genuine local IO fault must still be fatal, so the
/// discrimination is on `ErrorKind`, not on "any Io".
#[test]
fn a_write_to_a_departed_peer_is_not_a_loop_ending_error() {
    for kind in [
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::ConnectionAborted,
    ] {
        assert!(
            peer_gone(&AttachError::Io(io::Error::from(kind))),
            "{kind:?} means the peer hung up, not that this process faulted",
        );
    }

    // A real local failure is still fatal: if the server were still there,
    // the write would not have failed, so these cannot be a departed peer.
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::OutOfMemory,
        io::ErrorKind::InvalidData,
    ] {
        assert!(
            !peer_gone(&AttachError::Io(io::Error::from(kind))),
            "{kind:?} is a local fault and must still fail the loop",
        );
    }

    // Non-Io endings are classified by their own variants and must never
    // be swallowed as a departed peer.
    assert!(!peer_gone(&AttachError::Disconnected));
    assert!(!peer_gone(&AttachError::Protocol("bad frame".to_owned())));
}

#[test]
fn headless_completion_drains_history_and_metadata_after_attach_ready() {
    let terminal_id = TerminalId::local(7);
    let stream_id = phux_protocol::StreamId::new(1).expect("stream");
    let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
    let mut completion = HeadlessCompletion::new(Some(1));
    completion.note_history_request(&terminal_id, stream_id, bootstrap_id);

    completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
    assert!(
        !completion.is_complete(false),
        "ATTACH_READY may be queued before history and metadata replies"
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
        "layout and agent replies do not complete a pending history chain"
    );
    completion.observe_frame(
        &FrameKind::HistoryPage {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            rows: 0,
            page_seq: 1,
            cursor: bytes::Bytes::from_static(b"newest"),
            next_cursor: Some(bytes::Bytes::from_static(b"older")),
            payload: bytes::Bytes::new(),
        },
        7,
    );
    completion.note_history_request(&terminal_id, stream_id, bootstrap_id);
    assert!(
        !completion.is_complete(true),
        "an intermediate history page keeps its generation pending"
    );
    completion.observe_frame(
        &FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            rows: 0,
            bootstrap_id,
            page_seq: 1,
            cursor: bytes::Bytes::from_static(b"older"),
            next_cursor: None,
            payload: bytes::Bytes::new(),
        },
        7,
    );
    assert!(completion.is_complete(true));
}

#[test]
fn headless_history_control_responses_clear_outstanding_request() {
    let terminal_id = TerminalId::local(8);
    let stream_id = phux_protocol::StreamId::new(1).expect("stream");
    let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
    let terminal_frames = [
        FrameKind::HistoryTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: bytes::Bytes::from_static(b"cursor"),
            reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
        },
        FrameKind::HistoryRejected {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: bytes::Bytes::from_static(b"cursor"),
            reason: phux_protocol::wire::frame::HistoryRejectionReason::TooSmall,
            required_bytes: 128,
            required_rows: 1,
        },
    ];
    for frame in terminal_frames {
        let mut completion = HeadlessCompletion::new(None);
        completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
        completion.note_history_request(&terminal_id, stream_id, bootstrap_id);
        completion.observe_frame(&frame, 7);
        assert!(completion.is_complete(true));
    }
}

/// ADR-0060 guard: the `rec: None` arm of `run_buffered` must behave
/// exactly as the function did before the tee existed — the bare
/// `StdoutSink`, and the same pre-handshake failure delivered on the
/// cooked outer terminal (phux-roz) with no wrapper in the way.
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
    )
    .await
    .expect_err("there is no server at that socket");
    assert!(
        matches!(
            err,
            AttachError::Io(_) | AttachError::Connect(_) | AttachError::Unreachable(_)
        ),
        "the unrecorded path must still fail at connect, unchanged: {err:?}"
    );
}

#[test]
fn attach_error_disconnected_is_distinct_from_io() {
    let a = AttachError::Disconnected;
    let b = AttachError::Io(io::Error::other("foo"));
    assert_ne!(std::mem::discriminant(&a), std::mem::discriminant(&b),);
}
#[tokio::test(flavor = "current_thread")]
async fn attach_negotiation_waits_for_hello_ok_and_sends_one_hello() {
    let (client_stream, server_stream) = UnixStream::pair().expect("pair");
    let mut client = Connection::from_stream(client_stream);
    let server = tokio::spawn(ScriptedServer::on_stream(server_stream, ScriptSpec::new()).run());

    let res = client
        .negotiate(attach_client_name(), attach_client_caps(None))
        .await;
    assert!(
        res.is_ok(),
        "handshake should succeed when HELLO_OK arrives"
    );
    let selected = client
        .negotiated_bootstrap()
        .expect("successful negotiation installs immutable profile state");
    assert_eq!(selected.limits, phux_protocol::BootstrapLimits::default());
    let duplicate = client
        .negotiate(attach_client_name(), attach_client_caps(None))
        .await;
    assert!(
        matches!(duplicate, Err(AttachError::Protocol(_))),
        "a second local negotiation must be rejected before it reaches the wire"
    );
    drop(client);
    let seen = server.await.expect("scripted server task");
    assert!(
        matches!(seen.as_slice(), [FrameKind::Hello { .. }]),
        "attach construction must send exactly one HELLO, got {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn attach_negotiation_preserves_custom_caps_then_sends_attach() {
    let colors = TerminalDefaultColors {
        foreground: TerminalColor { r: 1, g: 2, b: 3 },
        background: TerminalColor { r: 4, g: 5, b: 6 },
    };
    let (client_stream, server_stream) = UnixStream::pair().expect("pair");
    let mut client = Connection::from_stream(client_stream);
    let mut server = Connection::from_stream(server_stream);

    let client_side = async {
        client
            .negotiate(attach_client_name(), attach_client_caps(Some(colors)))
            .await
            .expect("HELLO_OK");
        client
            .send(&FrameKind::Attach {
                attach_id: 1,
                target: AttachTarget::Last,
                viewport: ViewportInfo::new(120, 40),
                request_scrollback: true,
                scrollback_limit_lines: 10_000,
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
                .expect("fixture profiles intersect");
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
        let attach = server.recv().await.expect("ATTACH");
        (hello, attach)
    };

    let ((), (hello, attach)) = tokio::join!(client_side, server_side);
    let FrameKind::Hello { client_caps, .. } = hello else {
        panic!("first frame must be HELLO");
    };
    assert_eq!(client_caps.default_colors, Some(colors));
    assert!(client_caps.layers.contains(Layer::L3));
    assert!(
        matches!(attach, FrameKind::Attach { .. }),
        "ATTACH must immediately follow the single HELLO exchange"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn attach_negotiation_rejects_non_hello_ok_reply() {
    let (client_stream, server_stream) = UnixStream::pair().expect("pair");
    let mut client = Connection::from_stream(client_stream);
    let mut server = Connection::from_stream(server_stream);

    let server_side = async move {
        let frame = server.recv().await.expect("server recv hello");
        assert!(
            matches!(frame, FrameKind::Hello { .. }),
            "first client frame must be HELLO"
        );
        server
            .send(&FrameKind::Detached {
                reason: Some(DetachReason::ProtocolError),
                message: String::new(),
            })
            .await
            .expect("server send detached");
    };

    let negotiation = client.negotiate(attach_client_name(), attach_client_caps(None));
    let (res, ()) = tokio::join!(negotiation, server_side);
    match res {
        Err(AttachError::Protocol(msg)) => {
            // phux-i0e8.7.3: a frame with no arm is explained as version
            // skew with a remedy, never dumped as a Debug rendering.
            assert!(msg.contains("unexpected HELLO reply"), "{msg}");
            assert!(msg.contains("run `phux doctor`"), "{msg}");
        }
        other => panic!("expected protocol error, got {other:?}"),
    }
}

// -----------------------------------------------------------------
// phux-foz.10: chrome persists while overlays are open.
// -----------------------------------------------------------------

use crate::render::overlay::{RenderOverlay, SelectItem, SelectList};
use phux_config::KeybindingsCfg;
use phux_config::keybind::ResolvedAction;
use phux_config::widget::WindowInfo;

/// The probe viewport for the overlay-chrome tests.
const PROBE_VIEW: (u16, u16) = (80, 24);
/// Sidebar strip width for the overlay-chrome tests.
const PROBE_SIDEBAR_W: u16 = 20;
/// Window label shown on the sidebar's name row. Distinctive: appears
/// nowhere in any pane content or overlay body, so finding it in the
/// replayed frame proves the strip painted.
const PROBE_WINDOW: &str = "w1-agent";
/// Branch shown on the sidebar's branch row (herdr-style, phux-p4vp).
const PROBE_BRANCH: &str = "foz10-br";
/// Content written into the pane mirror, to prove the base frame
/// repainted around the floating modal.
const PROBE_PANE_TEXT: &str = "PANE-BASE";

/// A composited frame — panes, dividers and the **shipped** status bar
/// — at an arbitrary viewport, replayed through the PTY-probe oracle.
///
/// This is the closest the repo gets to "what is on the user's glass":
/// it runs the real `paint_full_frame` path with the bar built from
/// `default.toml`, so the shipped `[status]` lineup, the responsive
/// slot policy, and the widget-level shrink ladders are all exercised
/// together rather than one layer at a time.
fn shipped_frame_rows(view: (u16, u16), windows: &[WindowInfo]) -> Vec<String> {
    let (cols, rows) = view;
    let id = TerminalId::local(1);
    let workspace = Workspace::single(id.clone());

    let cfg = phux_config::parse_with_defaults("", std::path::Path::new("/nonexistent/c.toml"))
        .expect("shipped defaults parse");
    let mut status_bar = crate::attach::reload::compose_status_bar(&cfg, &[])
        .expect("the shipped status lineup must build")
        .expect("the shipped lineup is non-empty");
    status_bar.set_windows(windows.to_vec());

    // One row of the viewport belongs to the bar.
    let pane_rows = rows.saturating_sub(1);
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    panes.insert(
        id.clone(),
        PaneSlot::new_with_size(cols, pane_rows).expect("pane slot"),
    );
    let engine_kernel = published_test_kernel(&id, cols, pane_rows, PROBE_PANE_TEXT.as_bytes());

    let mut out: Vec<u8> = Vec::new();
    paint_full_frame(
        &mut out,
        &workspace.render_window(None).expect("layout"),
        &mut panes,
        &engine_kernel,
        Some(&id),
        view,
        Some(&mut status_bar),
        None,
        None,
        "phux",
    );

    let mut probe = PaneSlot::new_with_size(cols, rows).expect("probe slot");
    probe.terminal.vt_write(&out);
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
    (0..rows)
        .map(|r| {
            let base = usize::from(r) * usize::from(cols);
            frame.cells[base..base + usize::from(cols)]
                .iter()
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
    }
}

/// The whole composited frame at a roomy viewport: padded tab strip,
/// hints, session name and clock, all on one bar row.
#[test]
fn shipped_frame_at_a_roomy_viewport() {
    let windows = [
        probe_window("zsh", false),
        probe_window("nvim", true),
        probe_window("server", false),
    ];
    let rows = shipped_frame_rows((100, 12), &windows);
    let bar = rows.last().expect("a bar row");
    assert!(bar.contains(" 1:nvim "), "{bar:?}");
    assert!(bar.contains("Space palette"), "{bar:?}");
    assert!(bar.contains("phux"), "{bar:?}");
    assert!(!bar.contains("switch"), "{bar:?}");
    assert!(rows.join("\n").contains(PROBE_PANE_TEXT), "{rows:?}");
}

/// The same frame on a phone-sized grid. This is the shape the
/// responsive work exists for: the hints and the clock are gone and a
/// `switch` chip has taken their place, while every tab that is shown
/// is shown whole.
#[test]
fn shipped_frame_at_a_phone_sized_viewport() {
    let windows = [
        probe_window("zsh", false),
        probe_window("nvim", true),
        probe_window("server", false),
        probe_window("logs", false),
    ];
    let rows = shipped_frame_rows((46, 12), &windows);
    let bar = rows.last().expect("a bar row");
    assert!(bar.contains(" 1:nvim "), "active tab whole: {bar:?}");
    assert!(bar.contains("switch"), "{bar:?}");
    assert!(!bar.contains("Space palette"), "hints yield: {bar:?}");
    assert!(bar.chars().count() <= 46, "row overran: {bar:?}");
    assert!(rows.join("\n").contains(PROBE_PANE_TEXT), "{rows:?}");
    insta::assert_snapshot!("shipped_frame_phone_sized", rows.join("\n"));
}

/// Narrower still, where the tab strip itself has to give: it keeps
/// the active tab and its neighbours whole and stands in for the rest
/// with a `›`, rather than clipping a label into a window name that
/// does not exist.
#[test]
fn shipped_frame_when_the_tab_strip_must_collapse() {
    let windows = [
        probe_window("zsh", false),
        probe_window("nvim", true),
        probe_window("server", false),
        probe_window("logs", false),
    ];
    let rows = shipped_frame_rows((36, 10), &windows);
    let bar = rows.last().expect("a bar row");
    assert!(bar.contains(" 1:nvim "), "active tab whole: {bar:?}");
    assert!(bar.contains('\u{203a}'), "hidden tabs are marked: {bar:?}");
    assert!(!bar.contains("3:logs"), "the far tab is dropped: {bar:?}");
    assert!(bar.contains("switch"), "affordance survives: {bar:?}");
    assert!(bar.chars().count() <= 36, "row overran: {bar:?}");
    insta::assert_snapshot!("shipped_frame_collapsed_tabs", rows.join("\n"));
}

/// Replay `bytes` (a full frame of VT output) into a fresh libghostty
/// terminal — the house PTY-probe oracle — and project the resulting
/// grid to row-major plain text via the same `render_at_cells` surface
/// the production compositor uses.
fn replay_rows(bytes: &[u8]) -> Vec<String> {
    let (cols, rows) = PROBE_VIEW;
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
    (0..rows)
        .map(|r| {
            let base = usize::from(r) * usize::from(cols);
            frame.cells[base..base + usize::from(cols)]
                .iter()
                .map(|c| c.grapheme.as_str())
                .collect::<String>()
        })
        .collect()
}

/// The sidebar strip columns (left dock) of every replayed row, joined
/// as one string per row.
fn strip_columns(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|r| r.chars().take(usize::from(PROBE_SIDEBAR_W)).collect())
        .collect()
}

/// One `paint_active_overlay` frame for `overlay`, with the sidebar
/// enabled (left, width 20) and its painter threaded when
/// `with_painter`. Returns the emitted VT bytes.
fn paint_overlay_frame(overlay: Box<dyn RenderOverlay>, with_painter: bool) -> Vec<u8> {
    let theme = crate::render::Theme::default();
    let id = TerminalId::local(1);
    let workspace = Workspace::single(id.clone());
    let sidebar = Some(SidebarReservation {
        edge: SidebarEdge::Left,
        width: PROBE_SIDEBAR_W,
    });
    // Pane renderer metadata is separate from the published engine replica.
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    panes.insert(
        id.clone(),
        PaneSlot::new_with_size(PROBE_VIEW.0 - PROBE_SIDEBAR_W, PROBE_VIEW.1).expect("pane slot"),
    );
    let engine_kernel = published_test_kernel(
        &id,
        PROBE_VIEW.0 - PROBE_SIDEBAR_W,
        PROBE_VIEW.1,
        PROBE_PANE_TEXT.as_bytes(),
    );

    let mut sidebar_painter = SidebarPainter::new(theme);
    sidebar_painter.set_windows(vec![WindowInfo {
        name: PROBE_WINDOW.to_owned(),
        active: true,
        zoomed: false,
        attention: false,
        branch: Some(PROBE_BRANCH.to_owned()),
    }]);

    let mut overlays = OverlayState::new();
    overlays.push(overlay);

    let mut out: Vec<u8> = Vec::new();
    paint_active_overlay(
        &mut out,
        &overlays,
        &workspace,
        &mut panes,
        &engine_kernel,
        Some(&id),
        None,
        PROBE_VIEW,
        None,
        sidebar,
        with_painter.then_some(&mut sidebar_painter),
        "probe",
        &theme,
    );
    out
}

/// The command palette, as the dispatcher builds it (`SelectList`).
fn palette_overlay() -> Box<dyn RenderOverlay> {
    let theme = crate::render::Theme::default();
    let items = vec![
        SelectItem::new(
            "detach",
            ResolvedAction {
                action: "detach".to_owned(),
                args: std::collections::BTreeMap::new(),
            },
        ),
        SelectItem::new(
            "new-window",
            ResolvedAction {
                action: "new-window".to_owned(),
                args: std::collections::BTreeMap::new(),
            },
        ),
    ];
    Box::new(SelectList::new("command palette", items, &theme))
}

/// The agent-fleet dashboard, as the dispatcher builds it (phux-foz.7):
/// a `SelectList` carrying the fleet live key, with rows from
/// [`crate::attach::fleet::fleet_items`]. It rides the same bounded
/// floating-modal path as the palette, and the driver's fleet-dirty
/// live-refresh repaints it through `paint_active_overlay` — so it must
/// keep the sidebar visible on every refresh frame too.
fn fleet_overlay() -> Box<dyn RenderOverlay> {
    let theme = crate::render::Theme::default();
    let workspace = Workspace::single(TerminalId::local(1));
    let items = crate::attach::fleet::fleet_items(
        &workspace,
        &[],
        None,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(
        !items.iter().all(SelectItem::is_header),
        "probe fleet dashboard must have selectable rows"
    );
    Box::new(
        SelectList::new("agent fleet", items, &theme)
            .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
    )
}

/// phux-foz.10 mechanism guard: this pins the DEFECT shape so the
/// regression tests below cannot false-pass. A floating-modal repaint
/// whose base frame omits the sidebar painter leaves the reserved strip
/// columns blank — the "sidebar vanishes while the palette is open" bug.
#[test]
fn overlay_base_frame_without_painter_blanks_the_sidebar() {
    let rows = replay_rows(&paint_overlay_frame(palette_overlay(), false));
    let strip = strip_columns(&rows).join("\n");
    assert!(
        !strip.contains(PROBE_WINDOW) && !strip.contains(PROBE_BRANCH),
        "probe must detect the blank strip when the painter is absent;\n{strip}"
    );
}

/// phux-foz.10: opening the command palette must NOT blank the sidebar.
/// The floating-modal base frame repaints the strip (window label +
/// branch line) and the panes, then paints the modal on top.
#[test]
fn command_palette_keeps_sidebar_visible() {
    let rows = replay_rows(&paint_overlay_frame(palette_overlay(), true));
    let all = rows.join("\n");
    let strip = strip_columns(&rows).join("\n");
    assert!(
        strip.contains(PROBE_WINDOW),
        "sidebar window label must survive the palette;\n{all}"
    );
    assert!(
        strip.contains(PROBE_BRANCH),
        "sidebar branch line must survive the palette;\n{all}"
    );
    assert!(
        all.contains("command palette"),
        "the palette itself must be painted on top;\n{all}"
    );
    assert!(
        all.contains(PROBE_PANE_TEXT),
        "pane content must stay visible around the floating modal;\n{all}"
    );
    // phux-foz.14: the modal centers inside the pane content rect, so its
    // box corners land right of the sidebar divider — never inside the
    // reserved strip columns (the sidebar draws no corner glyphs itself).
    assert!(
        !strip.contains('┌') && !strip.contains('└'),
        "modal box corners must not intrude into the sidebar columns;\n{strip}"
    );
    // Pin the exact composition: sidebar strip + pane + centered modal.
    insta::assert_snapshot!(
        "palette_over_sidebar",
        rows.iter()
            .map(|r| r.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// phux-foz.10: every bounded (floating) overlay kind shares the same
/// base-frame path, so which-key, prompts, pickers, and toasts
/// must all keep the sidebar visible too.
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
    let overlays: Vec<(&str, Box<dyn RenderOverlay>)> = vec![
        ("palette", palette_overlay()),
        // phux-foz.7 fleet dashboard: same floating-modal path, and the
        // driver's fleet-dirty live refresh repaints it while it is
        // open — the sidebar must survive every refresh frame.
        ("agent-fleet", fleet_overlay()),
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
        let rows = replay_rows(&paint_overlay_frame(overlay, true));
        let strip = strip_columns(&rows).join("\n");
        assert!(
            strip.contains(PROBE_WINDOW),
            "{label}: sidebar window label must survive the overlay;\n{}",
            rows.join("\n")
        );
        assert!(
            strip.contains(PROBE_BRANCH),
            "{label}: sidebar branch line must survive the overlay;\n{}",
            rows.join("\n")
        );
    }
}

/// The copy-mode status strip counts a block selection as
/// `span_rows * band_cols`, distinct from the linear bounding-box count,
/// and never underflows when the tuple-normalized corners leave
/// `start_col > end_col` (a multi-row up-left drag).
#[test]
fn copy_mode_status_block_cell_count_differs_from_linear() {
    let theme = crate::render::Theme::default();
    let status_of = |sel: SelectionRect| -> String {
        let mut out: Vec<u8> = Vec::new();
        paint_copy_mode_status(&mut out, sel, (80, 24), &theme).expect("status");
        String::from_utf8_lossy(&out).into_owned()
    };

    // Corners tuple-normalize to start=(0,5), end=(2,2): 3 spanned rows,
    // column band {2,3,4,5} = 4 wide. Note start_col (5) > end_col (2).
    let corners = |rectangle| SelectionRect {
        start_row: 0,
        start_col: 5,
        end_row: 2,
        end_col: 2,
        rectangle,
    };

    // Block: 3 rows * 4 band cols = 12 (and no underflow despite 5 > 2).
    assert!(
        status_of(corners(true)).contains("12 cell(s)"),
        "block count must be span_rows * band_cols = 12"
    );
    // Linear: the bounding-box arithmetic saturates the reversed columns to
    // a width of 1, giving 3 rows * 1 = 3 — a different number, proving the
    // branch is taken and that the shared corners no longer panic.
    assert!(
        status_of(corners(false)).contains("3 cell(s)"),
        "linear count must differ from the block count"
    );

    // A plainly-ordered block (start_col <= end_col) counts the full band.
    let ordered_block = SelectionRect {
        start_row: 1,
        start_col: 2,
        end_row: 3,
        end_col: 6,
        rectangle: true,
    };
    // 3 rows * band {2..=6} (5 wide) = 15.
    assert!(
        status_of(ordered_block).contains("15 cell(s)"),
        "ordered block: 3 rows * 5 band cols = 15"
    );
}
