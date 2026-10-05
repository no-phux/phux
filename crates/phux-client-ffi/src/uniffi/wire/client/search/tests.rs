#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, Layer, LayerSet, ServerCapabilities,
    ServerFeatureSet,
};
use phux_protocol::ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use super::*;

const COLS: u16 = 24;
const ROWS: u16 = 4;

fn history(lines: &[&str]) -> Vec<u8> {
    lines.join("\r\n").into_bytes()
}

/// A `RemoteClient` over a sans-IO runtime client attached to `panes`, each
/// bootstrapped with its bytes. No socket: frames are fed directly.
fn remote_with(panes: &[(u32, Vec<u8>)]) -> Arc<RemoteClient> {
    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("main".into())),
        viewport: (COLS, ROWS),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features(ServerFeatureSet::new()),
            server_id: vec![0xAB; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("HELLO_OK");
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).expect("decodes").0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH queued");
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let resources = panes
        .iter()
        .map(|(id, _)| ResourceInfo::new(ResourceId::local(*id), window, COLS, ROWS))
        .collect();
    let snapshot = SessionSnapshot::new(session, window, ResourceId::local(panes[0].0))
        .with_sessions(vec![SessionInfo::new(session, "main")])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(resources);
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .expect("ATTACHED");
    for (id, bytes) in panes {
        bootstrap(&client, ResourceId::local(*id), bytes);
    }
    client
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ATTACH_READY");
    let _ = client.take_outbound();
    let remote = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
    *remote.client.lock().unwrap() = Some(client);
    remote
}

fn bootstrap(client: &Client, terminal_id: ResourceId, bytes: &[u8]) {
    let stream_id = StreamId::new(1).expect("nonzero");
    let bootstrap_id = BootstrapId::new(1).expect("nonzero");
    client
        .feed(FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: COLS,
            rows: ROWS,
            base_seq: 0,
        })
        .expect("BOOTSTRAP_BEGIN");
    client
        .feed(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes.to_vec().into(),
        })
        .expect("BOOTSTRAP_CHUNK");
    client
        .feed(FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
        .expect("BOOTSTRAP_READY");
}

fn top_row(remote: &RemoteClient, terminal: u32) -> String {
    remote
        .runtime_client()
        .expect("connected")
        .acquire(&ResourceId::local(terminal))
        .expect("published")
        .row_text(0)
}

fn pane(terminal: u32) -> String {
    id::encode(&ResourceId::local(terminal))
}

/// Four rows at the tail show `f g h LIVE TAIL`.
fn scrollback() -> Vec<u8> {
    history(&[
        "needle one",
        "a",
        "b",
        "c",
        "needle two",
        "d",
        "e",
        "f",
        "g",
        "h",
        "LIVE TAIL",
    ])
}

fn search(remote: &RemoteClient, terminal: u32, query: &str) -> ProjectionSearch {
    remote
        .search_projection(pane(terminal), query.into(), true, search_match_limit())
        .expect("search")
}

#[test]
fn reveal_walks_matches_in_document_order_and_clear_keeps_the_viewport() {
    let remote = remote_with(&[(1, scrollback())]);
    assert_eq!(top_row(&remote, 1), "f");
    let found = search(&remote, 1, "needle");
    assert_eq!(found.matches.len(), 2);
    assert!(!found.truncated);
    assert_eq!(top_row(&remote, 1), "f", "searching must not scroll");

    let [first, last] = [found.matches[0], found.matches[1]];
    remote
        .reveal_search_match(pane(1), last.start)
        .expect("reveal last");
    assert_eq!(top_row(&remote, 1), "needle two");
    remote
        .reveal_search_match(pane(1), first.start)
        .expect("reveal first");
    assert_eq!(top_row(&remote, 1), "needle one");

    remote.clear_projection_search(pane(1)).expect("clear");
    assert_eq!(
        top_row(&remote, 1),
        "needle one",
        "clear keeps the viewport"
    );
    assert!(matches!(
        remote.reveal_search_match(pane(1), first.start),
        Err(SearchError::StaleMatch { .. })
    ));
    remote.scroll_projection_to_bottom(pane(1));
    assert_eq!(top_row(&remote, 1), "f");
}

#[test]
fn a_repeated_search_reclaims_the_previous_handles() {
    let remote = remote_with(&[(1, scrollback())]);
    let old = search(&remote, 1, "needle");
    let new = search(&remote, 1, "needle");
    assert_eq!(new.matches.len(), 2);
    for stale in &old.matches {
        assert!(!new.matches.contains(stale), "handles are never reused");
        assert!(matches!(
            remote.reveal_search_match(pane(1), stale.start),
            Err(SearchError::StaleMatch { .. })
        ));
    }
    remote
        .reveal_search_match(pane(1), new.matches[1].start)
        .expect("fresh handle reveals");
}

#[test]
fn unicode_and_wide_glyph_queries_match_whole_graphemes() {
    let bytes = history(&[
        "日本語 wide",
        "café au lait",
        "x",
        "y",
        "z",
        "w",
        "v",
        "LIVE",
    ]);
    let remote = remote_with(&[(1, bytes)]);
    let wide = search(&remote, 1, "日本語");
    assert_eq!(wide.matches.len(), 1);
    remote
        .reveal_search_match(pane(1), wide.matches[0].start)
        .expect("reveal wide");
    assert!(top_row(&remote, 1).starts_with("日本語"));
    assert_eq!(search(&remote, 1, "café").matches.len(), 1);
    assert!(search(&remote, 1, "Café").matches.is_empty());
}

#[test]
fn no_match_is_an_empty_untruncated_result() {
    let remote = remote_with(&[(1, scrollback())]);
    let found = search(&remote, 1, "absent");
    assert!(found.matches.is_empty());
    assert!(!found.truncated);
}

#[test]
fn saturated_search_keeps_matches_revealable_after_repeated_searches() {
    let lines: Vec<_> = (0..600).map(|index| format!("needle {index:03}")).collect();
    let remote = remote_with(&[(1, lines.join("\r\n").into_bytes())]);
    for _ in 0..3 {
        remote.scroll_projection_to_bottom(pane(1));
        let found = search(&remote, 1, "needle");
        assert!(found.truncated);
        assert!(!found.matches.is_empty());
        // The first reveal needs a viewport pin; subsequent reveals must
        // allocate its replacement before releasing that pin.
        for index in [0, 1, 2, 0] {
            remote
                .reveal_search_match(pane(1), found.matches[index].start)
                .expect("bounded search leaves room for viewport pins");
            assert_eq!(top_row(&remote, 1), format!("needle {index:03}"));
        }
        let replaced = search(&remote, 1, "needle");
        remote
            .reveal_search_match(pane(1), replaced.matches[1].start)
            .expect("searching while pinned leaves replacement capacity");
        assert_eq!(top_row(&remote, 1), "needle 001");
        remote.clear_projection_search(pane(1)).expect("clear");
        assert_eq!(top_row(&remote, 1), "needle 001");
    }
}

#[test]
fn sparse_cells_separate_matches_without_splitting_wide_or_wrapped_text() {
    let remote = remote_with(&[(
        1,
        "a\x1b[2Cb\r\n日本語\r\ncafe\u{301}\r\n123456789012345678901234界x"
            .as_bytes()
            .to_vec(),
    )]);
    assert!(search(&remote, 1, "ab").matches.is_empty());
    assert_eq!(search(&remote, 1, "a  b").matches.len(), 1);
    assert_eq!(search(&remote, 1, "b\n日本語").matches.len(), 1);
    assert_eq!(search(&remote, 1, "日本語").matches.len(), 1);
    assert_eq!(search(&remote, 1, "cafe\u{301}").matches.len(), 1);
    assert_eq!(search(&remote, 1, "34界x").matches.len(), 1);

    // With one column left, Ghostty inserts a spacer head before wrapping
    // the wide glyph. Neither that head nor its tail is a searchable space.
    let remote = remote_with(&[(1, "12345678901234567890123界x".as_bytes().to_vec())]);
    assert_eq!(search(&remote, 1, "23界x").matches.len(), 1);
    assert!(search(&remote, 1, "23 界x").matches.is_empty());
}

#[test]
fn bounds_are_explicit() {
    let remote = remote_with(&[(1, scrollback())]);
    assert!(matches!(
        remote.search_projection(pane(1), String::new(), true, 8),
        Err(SearchError::EmptyQuery)
    ));
    let long = "x".repeat(SEARCH_QUERY_BYTE_LIMIT + 1);
    assert!(matches!(
        remote.search_projection(pane(1), long, true, 8),
        Err(SearchError::QueryTooLong { limit: 4096 })
    ));
    let one = remote
        .search_projection(pane(1), "needle".into(), true, 1)
        .expect("bounded");
    assert_eq!(one.matches.len(), 1);
    assert!(one.truncated, "a second match exists past the bound");
    let none = remote
        .search_projection(pane(1), "needle".into(), true, 0)
        .expect("zero bound");
    assert!(none.matches.is_empty());
    assert!(none.truncated);
    let exact = remote
        .search_projection(pane(1), "needle".into(), true, 2)
        .expect("exact bound");
    assert_eq!(exact.matches.len(), 2);
    assert!(!exact.truncated);
}

#[test]
fn panes_search_independently() {
    let remote = remote_with(&[(1, scrollback()), (2, scrollback())]);
    let a = search(&remote, 1, "needle");
    let b = search(&remote, 2, "needle");
    search(&remote, 1, "needle");
    remote.clear_projection_search(pane(1)).expect("clear a");
    remote
        .reveal_search_match(pane(2), b.matches[1].start)
        .expect("pane 2 handles survive pane 1's search and clear");
    assert_eq!(top_row(&remote, 2), "needle two");
    assert_eq!(top_row(&remote, 1), "f");
    assert!(
        remote
            .reveal_search_match(pane(2), a.matches[0].start)
            .is_err(),
        "another pane's handle never resolves here"
    );
}

#[test]
fn a_rebuilt_presentation_invalidates_its_matches() {
    let remote = remote_with(&[(1, scrollback())]);
    let found = search(&remote, 1, "needle");
    remote
        .runtime_client()
        .expect("connected")
        .engine()
        .expect("engine")
        .clear_presentation(&ResourceId::local(1), 1, 1)
        .expect("clear presentation");
    assert!(matches!(
        remote.reveal_search_match(pane(1), found.matches[0].start),
        Err(SearchError::StaleMatch { .. })
    ));
}

#[test]
fn without_a_connection_or_terminal_search_is_unavailable() {
    let offline = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
    assert!(matches!(
        offline.search_projection(pane(1), "needle".into(), true, 8),
        Err(SearchError::Unavailable)
    ));
    assert!(matches!(
        offline.clear_projection_search(pane(1)),
        Err(SearchError::Unavailable)
    ));
    assert!(matches!(
        offline.reveal_search_match(pane(1), u64::MAX),
        Err(SearchError::Unavailable)
    ));
    let remote = remote_with(&[(1, scrollback())]);
    for bad in ["", "not-an-id"] {
        assert!(matches!(
            remote.search_projection(bad.into(), "needle".into(), true, 8),
            Err(SearchError::Unavailable)
        ));
        assert!(matches!(
            remote.clear_projection_search(bad.into()),
            Err(SearchError::Unavailable)
        ));
        assert!(matches!(
            remote.reveal_search_match(bad.into(), u64::MAX),
            Err(SearchError::Unavailable)
        ));
    }
}

#[test]
fn missing_projection_search_is_unavailable() {
    let remote = remote_with(&[(1, scrollback())]);
    let result = remote.search_projection(pane(999), "needle".into(), true, 8);
    assert!(
        matches!(result, Err(SearchError::Unavailable)),
        "{result:?}"
    );
}

#[test]
fn missing_projection_zero_bound_search_is_unavailable() {
    let remote = remote_with(&[(1, scrollback())]);
    let result = remote.search_projection(pane(999), "needle".into(), true, 0);
    assert!(
        matches!(result, Err(SearchError::Unavailable)),
        "{result:?}"
    );
}

#[test]
fn missing_projection_clear_is_unavailable() {
    let remote = remote_with(&[(1, scrollback())]);
    let result = remote.clear_projection_search(pane(999));
    assert!(
        matches!(result, Err(SearchError::Unavailable)),
        "{result:?}"
    );
}

#[test]
fn missing_projection_reveal_is_unavailable_before_arbitrary_anchor_lookup() {
    let remote = remote_with(&[(1, scrollback())]);
    let result = remote.reveal_search_match(pane(999), u64::MAX);
    assert!(
        matches!(result, Err(SearchError::Unavailable)),
        "{result:?}"
    );
}

#[test]
fn missing_projection_reveal_is_unavailable_before_sibling_anchor_lookup() {
    let remote = remote_with(&[(1, scrollback())]);
    let found = search(&remote, 1, "needle");
    let result = remote.reveal_search_match(pane(999), found.matches[0].start);
    assert!(
        matches!(result, Err(SearchError::Unavailable)),
        "{result:?}"
    );
}

fn assert_search_unavailable(remote: &RemoteClient, terminal: u32, anchor: u64) {
    for bound in [8, 0] {
        let result = remote.search_projection(pane(terminal), "needle".into(), true, bound);
        assert!(
            matches!(result, Err(SearchError::Unavailable)),
            "{result:?}"
        );
    }
    let result = remote.clear_projection_search(pane(terminal));
    assert!(
        matches!(result, Err(SearchError::Unavailable)),
        "{result:?}"
    );
    for handle in [u64::MAX, anchor] {
        let result = remote.reveal_search_match(pane(terminal), handle);
        assert!(
            matches!(result, Err(SearchError::Unavailable)),
            "{result:?}"
        );
    }
}

#[test]
fn absent_projection_operations_preserve_sibling_handles_and_viewport() {
    let remote = remote_with(&[(1, scrollback())]);
    let found = search(&remote, 1, "needle");
    remote
        .reveal_search_match(pane(1), found.matches[0].start)
        .expect("pin sibling");
    let client = remote.runtime_client().expect("connected");
    let frame = client.acquire(&ResourceId::local(1)).expect("published");
    assert_search_unavailable(&remote, 999, found.matches[1].start);
    assert!(Arc::ptr_eq(
        &frame,
        &client.acquire(&ResourceId::local(1)).expect("unchanged")
    ));
    remote
        .reveal_search_match(pane(1), found.matches[1].start)
        .expect("sibling handle survives");
    assert_eq!(top_row(&remote, 1), "needle two");
}

#[test]
fn detached_projection_search_operations_are_unavailable() {
    let remote = remote_with(&[(1, scrollback()), (2, scrollback())]);
    let found = search(&remote, 1, "needle");
    let sibling = search(&remote, 2, "needle");
    let client = remote.runtime_client().expect("connected");
    assert!(
        client
            .engine()
            .expect("engine")
            .detach(ResourceId::local(1))
    );
    assert_search_unavailable(&remote, 1, found.matches[0].start);
    assert_search_unavailable(&remote, 1, sibling.matches[0].start);
    remote
        .reveal_search_match(pane(2), sibling.matches[0].start)
        .expect("sibling survives detach");
}

#[test]
fn non_retained_closed_projection_search_operations_are_unavailable() {
    let remote = remote_with(&[(1, scrollback()), (2, scrollback())]);
    let found = search(&remote, 1, "needle");
    let sibling = search(&remote, 2, "needle");
    remote
        .runtime_client()
        .expect("connected")
        .feed(FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(1),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        })
        .expect("closed");
    assert_search_unavailable(&remote, 1, found.matches[0].start);
    assert_search_unavailable(&remote, 1, sibling.matches[0].start);
    remote
        .reveal_search_match(pane(2), sibling.matches[0].start)
        .expect("sibling survives close");
}

#[test]
fn query_validation_precedes_absent_projection_lookup() {
    let remote = remote_with(&[(1, scrollback())]);
    assert!(matches!(
        remote.search_projection(pane(999), String::new(), true, 0),
        Err(SearchError::EmptyQuery)
    ));
    assert!(matches!(
        remote.search_projection(pane(999), "x".repeat(SEARCH_QUERY_BYTE_LIMIT + 1), true, 0),
        Err(SearchError::QueryTooLong { limit: 4096 })
    ));
}

#[test]
fn live_wrong_terminal_and_view_anchors_remain_engine_errors() {
    let remote = remote_with(&[(1, scrollback()), (2, scrollback())]);
    let sibling = search(&remote, 2, "needle");
    assert!(matches!(
        remote.reveal_search_match(pane(1), sibling.matches[0].start),
        Err(SearchError::Engine { reason }) if reason == "document anchor belongs to another view"
    ));
    let engine = remote
        .runtime_client()
        .expect("connected")
        .engine()
        .expect("engine");
    let view = engine.create_view(&ResourceId::local(1)).expect("view");
    let found = engine
        .search_view(view, "needle".into(), true)
        .expect("view search");
    assert!(matches!(
        remote.reveal_search_match(pane(1), found[0].start),
        Err(SearchError::Engine { reason }) if reason == "document anchor belongs to another view"
    ));
}

#[test]
fn genuine_engine_and_spawn_errors_preserve_their_reasons() {
    assert!(matches!(
        SearchError::from(EngineError::Engine("render state allocation failed".into())),
        SearchError::Engine { reason } if reason == "render state allocation failed"
    ));
    assert!(matches!(
        SearchError::from(EngineError::Spawn(std::io::Error::other("thread spawn failed"))),
        SearchError::Engine { reason } if reason == "thread spawn failed"
    ));
    assert!(matches!(
        SearchError::from(EngineError::Stopped),
        SearchError::Unavailable
    ));
}

#[test]
fn retained_close_keeps_real_kernel_failures_as_engine_errors() {
    let remote = remote_with(&[(1, scrollback())]);
    let client = remote.runtime_client().expect("connected");
    let engine = client.engine().expect("engine");
    engine.set_retain_on_close(&ResourceId::local(1), true);
    client
        .feed(FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(1),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        })
        .expect("closed");
    assert!(engine.has_projection(&ResourceId::local(1)));
    assert!(matches!(
        remote.search_projection(pane(1), "needle".into(), true, 8),
        Err(SearchError::Engine { .. })
    ));
    engine.release(&ResourceId::local(1));
    assert_search_unavailable(&remote, 1, u64::MAX);
}
