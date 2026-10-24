//! Duplicate placements of one terminal over the real embedded runtime.
//!
//! Each placement owns a runtime view: scroll, search and pin stay in that
//! view, output reaches every view, and closing one placement leaves the
//! other attached with its process alive.

use super::*;
use crate::connection::DeterministicDial;
use phux_client_runtime::engine::Scroll;
use phux_protocol::{
    caps::BootstrapStreamProfile,
    ids::{BootstrapId, StreamId},
    wire::frame::{Command, FrameKind},
};

use std::sync::Arc;

mod fixture;

use fixture::{SESSION, bootstrap, reconnect, stream, terminal};
pub(crate) use fixture::{attached, attached_with, opened};

fn output(connection: &mut Connection, seq: u64, bytes: &[u8]) {
    connection
        .client
        .feed(FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            seq,
            bytes: bytes.to_vec().into(),
        })
        .unwrap();
    connection.drain();
}

fn leaves(workspace: &Workspace) -> Vec<Placement> {
    workspace
        .tabs
        .iter()
        .flat_map(|tab| placements(&tab.root).into_iter().cloned())
        .collect()
}

fn view(placement: &Placement) -> ViewId {
    placement.view_id.expect("placement owns a view")
}

#[test]
fn placed_foreign_spawn_reconnects_before_bootstrap_without_inventory() {
    use phux_protocol::wire::frame::SpawnResult;
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let mut workspace = opened(&mut connection);
    let foreign = ResourceId::local(9);
    let id = workspace
        .launch_capture(&connection, SpawnRequest::default(), true)
        .unwrap();
    connection
        .client
        .feed(FrameKind::ResourceSpawned {
            request_id: id,
            result: SpawnResult::Ok(foreign.clone()),
        })
        .unwrap();
    let events = connection.drain();
    workspace.handle(&events, &connection);
    workspace.settle(&connection);
    assert_eq!(workspace.views_of(&foreign), 1);
    assert!(workspace.focused().unwrap().view_id.is_none());
    assert!(connection.pane(&foreign).is_none());
    reconnect(&mut connection);
    workspace.settle(&connection);
    let outbound: Vec<_> = connection
        .client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    assert!(
        outbound.iter().any(|frame| matches!(frame,
            FrameKind::Command { command: Command::AttachResource { terminal_id, .. }, .. }
                if terminal_id == &foreign
        )),
        "placed foreign spawn must reattach after reconnect: {outbound:?}"
    );
    assert_eq!(workspace.views_of(&foreign), 1, "no duplicate placement");
}

#[test]
fn awaiting_foreign_capture_rearms_a_stream_without_respawning_or_needing_inventory() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let mut workspace = opened(&mut connection);
    let before = workspace.tabs.clone();
    let foreign = ResourceId::local(9);
    // State after a same-incarnation reconnect: spawn was acknowledged,
    // but no bootstrap/placement arrived and own-spawn admission was reset.
    // The home-session snapshot does not include the foreign terminal.
    workspace.spawns.insert(42, Destination::Tab);
    workspace.answered(42, Some(&foreign), None);
    assert!(connection.pane(&foreign).is_none());
    let _ = connection.client.take_outbound();
    workspace.settle(&connection);
    let outbound: Vec<_> = connection
        .client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    assert!(
        outbound.iter().any(|frame| matches!(frame,
            FrameKind::Command { command: Command::AttachResource { terminal_id, .. }, .. }
                if terminal_id == &foreign
        )),
        "must recover the acknowledged stream: {outbound:?}"
    );
    assert!(
        outbound
            .iter()
            .all(|frame| !matches!(frame, FrameKind::SpawnResource { .. }))
    );
    assert_eq!(workspace.tabs.len(), before.len() + 1);
    assert_eq!(workspace.views_of(&foreign), 1);
    let placed = workspace.focused().expect("answered spawn is visible");
    assert_eq!(placed.terminal_id.as_ref(), Some(&foreign));
    assert!(placed.view_id.is_none(), "view still waits for bootstrap");
    assert!(workspace.unresolved.contains(&placed.id));
    workspace.settle(&connection);
    assert!(
        connection.client.take_outbound().is_empty(),
        "pending attach is deduplicated"
    );
    let request_id = outbound
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::AttachResource { terminal_id, .. },
            } if terminal_id == &foreign => Some(*request_id),
            _ => None,
        })
        .unwrap();
    let stream_id = StreamId::new(2).unwrap();
    let bootstrap_id = BootstrapId::new(2).unwrap();
    for frame in [
        FrameKind::CommandResult {
            request_id,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        },
        FrameKind::BootstrapBegin {
            terminal_id: foreign.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: foreign.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"recovered agent".to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: foreign.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        connection.client.feed(frame).unwrap();
    }
    let events = connection.drain();
    workspace.handle(&events, &connection);
    workspace.settle(&connection);
    assert!(
        connection.ready(&foreign),
        "successful explicit attach is admitted before inventory"
    );
    assert!(
        connection.pane(&foreign).is_none(),
        "no inventory update was needed"
    );
    assert_eq!(workspace.views_of(&foreign), 1);
    assert!(workspace.awaiting.is_empty());
}

fn frame(
    connection: &Connection,
    view: ViewId,
) -> Arc<phux_client_runtime::publication::GridFrame> {
    connection.client.acquire_view(view).expect("view frame")
}

fn rows(connection: &Connection, view: ViewId) -> Vec<String> {
    let frame = frame(connection, view);
    (0..frame.rows).map(|row| frame.row_text(row)).collect()
}

/// Frames that would detach the terminal from this client or end its process.
fn releases_terminal(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::Command {
            command: Command::DetachResource { .. }
                | Command::KillResource { .. }
                | Command::KillResources { .. }
                | Command::KillResourceIf { .. }
                | Command::ResourceTerminate { .. }
                | Command::SignalTerminal { .. }
                | Command::CloseTabResources { .. }
                | Command::DetachClients { .. },
            ..
        }
    )
}

/// Placement, view, and terminal of a duplicated home pane.
fn duplicated(connection: &mut Connection) -> (Workspace, Placement, Placement) {
    let mut workspace = opened(connection);
    let original = workspace.focused().cloned().unwrap();
    workspace.duplicate_view(connection);
    let mirror = workspace.focused().cloned().unwrap();
    (workspace, original, mirror)
}

#[test]
fn duplicate_view_adds_a_placement_with_its_own_view_of_the_same_terminal() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let (workspace, original, mirror) = duplicated(&mut connection);
    let all = leaves(&workspace);
    assert_eq!(all.len(), 2, "one tab, two placements");
    assert_eq!(workspace.tabs.len(), 1);
    assert_ne!(mirror.id, original.id, "a new placement");
    assert_ne!(
        view(&mirror),
        view(&original),
        "a duplicate never shares the original's ViewId"
    );
    assert_eq!(mirror.terminal_id, Some(terminal()));
    assert_eq!(original.terminal_id, Some(terminal()));
    assert_eq!(mirror.terminal_label, original.terminal_label);
    assert_eq!(workspace.views_of(&terminal()), 2);
    assert_eq!(
        rows(&connection, view(&mirror)),
        rows(&connection, view(&original))
    );
}

#[test]
fn scroll_search_and_pin_stay_in_one_view_while_output_reaches_both() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let (_workspace, original, mirror) = duplicated(&mut connection);
    let (left, right) = (view(&original), view(&mirror));
    let tail = rows(&connection, right);
    assert_eq!(tail, ["two", "three", "four", "five"]);
    let right_generation = frame(&connection, right).generation;

    connection.client.scroll_view(left, Scroll::Top).unwrap();
    assert_eq!(
        rows(&connection, left)[0],
        "zero",
        "the scrolled view moved"
    );
    assert_eq!(rows(&connection, right), tail, "the other view did not");
    assert_eq!(
        connection.client.view_generation(right),
        Some(right_generation),
        "scrolling one view never republishes the other"
    );

    connection.client.follow_live_view(left).unwrap();
    let engine = connection.client.engine().unwrap();
    let found = engine.search_view(left, "one".into(), true).unwrap();
    assert_eq!(found.len(), 1);
    connection
        .client
        .pin_viewport_view(left, found[0].start)
        .unwrap();
    assert_eq!(
        rows(&connection, left)[0],
        "one",
        "pinned to the search hit"
    );
    assert_eq!(rows(&connection, right), tail);
    assert_eq!(
        connection.client.view_generation(right),
        Some(right_generation),
        "search and pin in one view never republish the other"
    );

    output(&mut connection, 1, b"\r\nsix");
    let (left_frame, right_frame) = (frame(&connection, left), frame(&connection, right));
    assert_eq!(left_frame.last_seq, 1, "output reached the pinned view");
    assert_eq!(right_frame.last_seq, 1, "output reached the tailing view");
    assert_eq!(rows(&connection, right), ["three", "four", "five", "six"]);
    assert_eq!(
        rows(&connection, left)[0],
        "one",
        "the pinned view stays put"
    );

    connection.client.follow_live_view(left).unwrap();
    assert_eq!(rows(&connection, left), rows(&connection, right));
}

#[test]
fn closing_one_placement_keeps_the_other_attached_and_the_process_alive() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let (mut workspace, original, mirror) = duplicated(&mut connection);
    let _ = connection.client.take_outbound();

    workspace.close_pane(&connection, Some(&mirror.id));

    let sent: Vec<FrameKind> = connection
        .client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    assert!(
        !sent.iter().any(releases_terminal),
        "closing a duplicate must not detach or end the terminal: {sent:?}"
    );
    assert!(
        connection.client.acquire_view(view(&mirror)).is_none(),
        "the closed placement's view is released"
    );
    let remaining = leaves(&workspace);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, original.id);
    assert_eq!(
        workspace.focused().map(|leaf| leaf.id.clone()),
        Some(original.id.clone())
    );
    assert_eq!(workspace.views_of(&terminal()), 1);
    assert!(connection.ready(&terminal()), "terminal still attached");

    output(&mut connection, 1, b"\r\nsix");
    assert_eq!(connection.status, Status::Attached);
    assert_eq!(
        rows(&connection, view(&original)),
        ["three", "four", "five", "six"],
        "the surviving view keeps receiving output"
    );
}

#[test]
fn saved_duplicate_placements_restore_as_two_views_of_the_same_terminal() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let (workspace, original, mirror) = duplicated(&mut connection);
    let display = crate::settings::default_display();
    let saved = snapshot_layout(&workspace, &connection, &display).expect("layout");
    assert_eq!(saved.server_id, "01".repeat(16));
    let saved = persist::parse_layout(&persist::layout_value(&saved)).expect("round trip");

    let mut restored = Workspace::new(SESSION.into(), WorkspaceOptions::default());
    restored.restore(Some(saved));
    restored.handle(&[], &connection);
    restored.settle(&connection);

    assert!(restored.restored());
    assert_eq!(restored.server_id(), workspace.server_id());
    let after = leaves(&restored);
    assert_eq!(
        after
            .iter()
            .map(|leaf| leaf.id.as_str())
            .collect::<Vec<_>>(),
        [original.id.as_str(), mirror.id.as_str()],
        "both placements return; duplicate terminals are not collapsed"
    );
    assert!(
        after
            .iter()
            .all(|leaf| leaf.terminal_id == Some(terminal()))
    );
    let views: HashSet<ViewId> = after.iter().map(view).collect();
    assert_eq!(views.len(), 2, "each restored placement owns its own view");
    assert!(!views.contains(&view(&original)) && !views.contains(&view(&mirror)));
    assert_eq!(
        restored.focused().map(|leaf| leaf.id.clone()),
        Some(mirror.id.clone()),
        "focus returns to the same duplicate placement"
    );
}

#[test]
fn reveal_focuses_an_existing_placement_and_never_duplicates_it() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let (mut workspace, original, mirror) = duplicated(&mut connection);
    assert_eq!(
        workspace.focused().map(|leaf| leaf.id.clone()),
        Some(mirror.id.clone())
    );

    assert!(workspace.reveal(&original.terminal_label));
    assert_eq!(
        workspace.focused().map(|leaf| leaf.id.clone()),
        Some(original.id.clone()),
        "reveal focuses the first existing placement"
    );
    assert_eq!(leaves(&workspace).len(), 2, "reveal never adds a placement");

    let pane = connection.pane(&terminal()).cloned().unwrap();
    workspace.open(&connection, &pane);
    let after = leaves(&workspace);
    assert_eq!(after.len(), 2, "opening a shown terminal reveals it");
    assert_eq!(
        after.iter().map(view).collect::<Vec<_>>(),
        [view(&original), view(&mirror)],
        "reveal keeps the existing views"
    );
    assert!(!workspace.reveal(&terminal_label(&ResourceId::local(99))));
}

#[test]
fn exactly_one_visible_duplicate_proposes_the_pty_size_across_focus_tabs_and_close() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached();
    let (mut workspace, original, mirror) = duplicated(&mut connection);
    let owners = |workspace: &Workspace| -> Vec<String> {
        leaves(workspace)
            .iter()
            .filter(|leaf| workspace.size_owner(leaf))
            .map(|leaf| leaf.id.clone())
            .collect()
    };
    assert_eq!(
        owners(&workspace),
        [mirror.id.as_str()],
        "the new view owns size"
    );
    let tab = workspace.active_id.clone();
    workspace.focus_placement(&tab, &original.id);
    assert_eq!(
        owners(&workspace),
        [original.id.as_str()],
        "focus hands it back"
    );

    // A third placement of the same terminal in another tab. While that tab
    // is hidden its placement never owns size, even when it was claimed last.
    workspace.land(&connection, &original.terminal_label, Destination::Tab);
    let hidden = workspace.focused().cloned().unwrap();
    assert_ne!(workspace.active_id, tab);
    assert_eq!(workspace.views_of(&terminal()), 3);
    assert_eq!(
        owners(&workspace),
        [hidden.id.as_str()],
        "the only visible placement owns size"
    );
    workspace.select_tab(&tab);
    assert_eq!(
        owners(&workspace),
        [original.id.as_str()],
        "a hidden duplicate never owns size, even as the last claim"
    );

    // Closing the owner hands size to the surviving visible placement.
    workspace.focus_placement(&tab, &mirror.id);
    assert_eq!(owners(&workspace), [mirror.id.as_str()]);
    workspace.close_pane(&connection, Some(&mirror.id));
    assert_eq!(
        owners(&workspace),
        [original.id.as_str()],
        "the survivor owns size after the owner closes"
    );
    workspace.close_pane(&connection, Some(&original.id));
    assert_eq!(workspace.tabs.len(), 1, "the emptied tab closed");
    assert_eq!(
        owners(&workspace),
        [hidden.id.as_str()],
        "the other tab's placement owns size once it is the one shown"
    );
}
