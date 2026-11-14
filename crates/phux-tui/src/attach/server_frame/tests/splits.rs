//! Parked splits: a spawn reply, a satellite pane's attach, and the
//! refusals and races between them.

use super::*;

/// A split of pane 1 parked with `host` and `adopt`.
fn parked_split(host: SplitHost, adopt: Option<ResourceId>) -> PendingSplit {
    PendingSplit {
        focused_at_request: tid(1),
        dir: SplitDir::Horizontal,
        zoom_on_spawn: false,
        host,
        adopt: adopt.map(|pane| Adopt::Spawned(SpawnedPane::unbound(pane))),
    }
}

fn satellite_split(adopt: Option<ResourceId>) -> PendingSplit {
    parked_split(SplitHost::Satellite(SatelliteHost::new("edge")), adopt)
}

/// Drive one reply with `split` parked under request 9, against `ws`
/// focused on `focused`.
fn split_reply_in(
    ws: ArrangementDoc,
    focused: ResourceId,
    split: PendingSplit,
    frame: FrameKind,
) -> (FrameOutcome, H) {
    let mut h = H::on(ws, &[&tid(1)]);
    h.mirror.focused_resource = Some(focused);
    h.mirror.pending_splits.insert(9, split);
    let outcome = h.send(frame);
    (outcome, h)
}

fn split_reply(split: PendingSplit, frame: FrameKind) -> (FrameOutcome, H) {
    split_reply_in(ArrangementDoc::single(tid(1)), tid(1), split, frame)
}

/// A parked split's spawn reply zooms the new pane under `zoom_on_spawn`,
/// else clears zoom; focus follows the spawned pane and the anchor becomes
/// the MRU target.
#[test]
fn a_split_reply_zooms_only_under_zoom_on_spawn() {
    for zoom_on_spawn in [true, false] {
        let mut split = parked_split(SplitHost::Attached, None);
        split.zoom_on_spawn = zoom_on_spawn;
        let mut h = H::on(ArrangementDoc::single(tid(1)), &[&tid(1)]);
        h.mirror.zoomed = Some(tid(1));
        h.mirror.pending_splits.insert(9, split);
        let mut history = crate::attach::focus::FocusHistory::default();
        let before = h.mirror.focused_resource.clone();
        assert!(h.send(spawned(tid(2))).layout_replaced);
        history.observe(before, h.mirror.focused_resource.as_ref());
        history.repair(h.mirror.focused_resource.as_ref(), &h.mirror.workspace);
        assert_eq!(h.mirror.focused_resource, Some(tid(2)));
        assert_eq!(
            history.target(h.mirror.focused_resource.as_ref(), &h.mirror.workspace),
            Some(tid(1))
        );
        assert_eq!(h.mirror.zoomed, zoom_on_spawn.then(|| tid(2)));
    }
}

/// A split spawned on a satellite does not apply until its attach succeeds;
/// the reply parks it, keeping a bound spawn's instance token. A local pane
/// answered bound (tolerated) still splits in place with no notice.
#[test]
fn a_split_spawned_on_a_satellite_waits_for_its_attach() {
    for (result, instance) in [
        (SpawnResult::Ok(edge_pane()), None),
        (
            SpawnResult::OkBound {
                id: edge_pane(),
                instance: edge_token(),
            },
            Some(edge_token()),
        ),
    ] {
        let (outcome, h) = split_reply(
            satellite_split(None),
            FrameKind::ResourceSpawned {
                request_id: 9,
                result,
            },
        );
        assert_eq!(h.leaves(), vec![tid(1)], "nothing splits before the attach");
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
        assert!(!outcome.layout_replaced && !outcome.emit_set_metadata && !h.belled());
        let [ParkedAdopt::Split(split)] = outcome.adopt_spawned.as_slice() else {
            panic!("expected one parked split: {:?}", outcome.adopt_spawned);
        };
        assert_eq!(
            split.adopt,
            Some(Adopt::Spawned(SpawnedPane {
                id: edge_pane(),
                instance
            }))
        );
        assert_eq!(split.host, SplitHost::Satellite(SatelliteHost::new("edge")));
        assert_eq!(split.focused_at_request, tid(1));
    }

    let (outcome, h) = split_reply(
        parked_split(SplitHost::Attached, None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::OkBound {
                id: tid(2),
                instance: edge_token(),
            },
        },
    );
    assert_eq!(h.leaves(), vec![tid(1), tid(2)]);
    assert!(outcome.notices.is_empty(), "a local split raises no notice");
}

/// The parked satellite split, or an opened existing satellite pane, lands
/// beside the local leaf, focused and saved, once the attach succeeds.
#[test]
fn a_satellite_split_applies_when_its_attach_succeeds() {
    let mut existing = satellite_split(None);
    existing.adopt = Some(Adopt::Existing(edge_pane()));
    for split in [satellite_split(Some(edge_pane())), existing] {
        let (outcome, h) = split_reply(split, command_ok(9));
        assert_eq!(h.leaves(), vec![tid(1), edge_pane()]);
        assert_eq!(h.mirror.focused_resource, Some(edge_pane()));
        assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
        assert!(outcome.notices.is_empty() && !h.belled() && outcome.kill_orphans.is_empty());
        assert!(h.mirror.pending_splits.is_empty());
    }
}

/// A refused open of an existing satellite pane leaves the layout alone and
/// does not kill a pane this client did not spawn.
#[test]
fn a_refused_open_of_an_existing_satellite_pane_does_not_kill_it() {
    let mut split = satellite_split(None);
    split.adopt = Some(Adopt::Existing(edge_pane()));
    let (outcome, h) = split_reply(split, unreachable_refusal(9));
    assert_eq!(h.leaves(), vec![tid(1)]);
    assert!(outcome.kill_orphans.is_empty());
    assert!(h.belled());
    assert!(h.mirror.pending_splits.is_empty());
}

/// A refused satellite split attach leaves no dead split, bells, names the
/// host, and kills the spawned pane unless no kill could reach it.
#[test]
fn a_spawned_satellite_split_refusal_bells_and_names_the_host() {
    for (frame, killed) in [
        (unreachable_refusal(9), Vec::new()),
        (
            FrameKind::Error {
                request_id: Some(9),
                code: ErrorCode::TerminalNotFound,
                message: "no such terminal".to_owned(),
            },
            vec![edge_pane()],
        ),
    ] {
        let (outcome, h) = split_reply(satellite_split(Some(edge_pane())), frame);
        assert_eq!(h.leaves(), vec![tid(1)]);
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
        assert!(!outcome.emit_set_metadata && !outcome.layout_replaced);
        assert!(h.belled());
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("could not open on edge"),
            "{}",
            outcome.notices[0].text
        );
        assert_eq!(outcome.kill_orphans, killed);
        assert!(h.mirror.pending_splits.is_empty());
    }
}

/// A satellite split whose relayed spawn is refused bells and names the host.
#[test]
fn a_satellite_split_whose_spawn_is_refused_names_the_host() {
    use phux_protocol::wire::frame::SpawnError;
    let (outcome, h) = split_reply(
        satellite_split(None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::Err(SpawnError::SatelliteUnreachable(
                "satellite edge link is down".to_owned(),
            )),
        },
    );
    assert_eq!(h.leaves(), vec![tid(1)]);
    assert!(h.belled());
    assert_eq!(outcome.notices.len(), 1);
    assert!(
        outcome.notices[0]
            .text
            .contains("could not open on edge: satellite edge link is down"),
        "{}",
        outcome.notices[0].text
    );
    assert!(outcome.adopt_spawned.is_empty());
}

/// A hub without host-aware spawns opened the split on itself: it applies
/// locally and the notice says the pane is on this host.
#[test]
fn a_satellite_split_on_an_older_hub_says_it_opened_on_this_host() {
    let (outcome, h) = split_reply(
        parked_split(
            SplitHost::AttachedInsteadOf(SatelliteHost::new("edge")),
            None,
        ),
        spawned(tid(2)),
    );
    assert_eq!(h.leaves(), vec![tid(1), tid(2)]);
    assert!(outcome.adopt_spawned.is_empty());
    assert_eq!(outcome.notices.len(), 1);
    let text = &outcome.notices[0].text;
    assert!(
        text.contains("on this host") && text.contains("edge"),
        "{text}"
    );
}

/// A split parked across a window switch lands beside its source, in that
/// pane's window, without moving the visible window or focus.
#[test]
fn a_split_parked_across_a_window_switch_lands_beside_its_source() {
    let mut ws = ArrangementDoc::single(tid(1));
    ws.add_group("2".to_owned(), tid(5));
    let (outcome, h) = split_reply_in(
        ws,
        tid(5),
        satellite_split(Some(edge_pane())),
        command_ok(9),
    );
    assert_eq!(
        window_leaves(&h.mirror.workspace, 0),
        vec![tid(1), edge_pane()]
    );
    assert_eq!(window_leaves(&h.mirror.workspace, 1), vec![tid(5)]);
    assert_eq!(h.mirror.workspace.active, 1, "the window on screen stays");
    assert_eq!(
        h.mirror.focused_resource,
        Some(tid(5)),
        "focus is not stolen"
    );
    assert!(outcome.emit_set_metadata);
    assert!(!h.belled());
}

/// The source pane closed while its split waited: the split is dropped with
/// a bell and notice (satellite attach or local spawn alike) and the
/// unreferenced spawned pane is killed.
#[test]
fn a_split_whose_source_pane_closed_is_dropped() {
    for (split, frame, spawned_pane) in [
        (
            satellite_split(Some(edge_pane())),
            command_ok(9),
            edge_pane(),
        ),
        (
            parked_split(SplitHost::Attached, None),
            spawned(tid(2)),
            tid(2),
        ),
    ] {
        let (outcome, h) = split_reply_in(ArrangementDoc::single(tid(3)), tid(3), split, frame);
        assert_eq!(h.leaves(), vec![tid(3)]);
        assert_eq!(h.mirror.focused_resource, Some(tid(3)));
        assert!(!outcome.emit_set_metadata && !outcome.layout_replaced);
        assert!(h.belled());
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("split dropped"),
            "{}",
            outcome.notices[0].text
        );
        assert_eq!(outcome.kill_orphans, vec![spawned_pane]);
        assert!(h.mirror.pending_splits.is_empty());
    }
}
