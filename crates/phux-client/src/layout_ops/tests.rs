use super::*;
use crate::attach::connection::Connection;
use crate::layout::{Group, LayoutNode, LayoutState, leaves};
use crate::layout_ops::mutation::adopt_unarranged_groups;
use crate::testkit::{ScriptSpec, ScriptedServer};
use phux_protocol::wire::frame::{ErrorCode, FrameKind, Scope};

fn tid(id: u32) -> ResourceId {
    ResourceId::local(id)
}

/// Whose layout a key names: an unattributable key is `None`, never
/// `Legacy`, and a private `--projection` prefix is never the TUI's own.
#[test]
fn layout_key_session_names_the_owner_and_ignores_private_projections() {
    let own = |id| Some(LayoutKeyOwner::Session(SessionId::new(id)));
    assert_eq!(layout_key_session(LAYOUT_KEY), Some(LayoutKeyOwner::Legacy));
    assert_eq!(layout_key_session("phux.tui.layout/v1/7"), own(7));
    assert_eq!(layout_key_session(&layout_key(SessionId::new(42))), own(42));
    for foreign in [
        "phux.tui.layout/v1/zzz",
        "phux.tui.layout/v12",
        "phux.tui.other/v1",
        "myapp.layout/v1/7",
        "a.b.c.layout/v1/7",
    ] {
        assert_eq!(layout_key_session(foreign), None, "{foreign:?}");
    }
}

#[test]
fn projection_key_must_be_a_layout_v1_key_for_the_addressed_session() {
    let session = SessionId::new(7);
    assert!(validate_projection_key("phux.tui.layout/v1/7", session).is_ok());
    assert!(validate_projection_key("myapp.layout/v1/7", session).is_ok());
    for bad in [
        "myapp.layout/v1/8",
        "not-a-layout-key",
        ".layout/v1/7",
        "myapp.layout/v1/07",
    ] {
        assert!(
            matches!(
                validate_projection_key(bad, session),
                Err(LayoutOpsError::InvalidProjectionKey(_))
            ),
            "{bad:?}"
        );
    }
}

fn split(left: u32, right: u32, dir: SplitDir, ratio: f32) -> LayoutNode {
    LayoutNode::Split {
        dir,
        ratio,
        left: Box::new(LayoutNode::Leaf(tid(left))),
        right: Box::new(LayoutNode::Leaf(tid(right))),
    }
}

fn two_window_workspace() -> ArrangementDoc {
    ArrangementDoc {
        groups: vec![
            Group::new(
                "editor".to_owned(),
                LayoutState {
                    tree: Some(split(1, 2, SplitDir::Horizontal, 0.6)),
                    focus: Some(tid(1)),
                },
            ),
            Group::new("tests".to_owned(), LayoutState::single(tid(3))),
        ],
        active: 0,
    }
}

/// An empty arrangement takes the fallback whole, so a placed spawn
/// beside an unarranged seed creates the first group instead of failing.
#[test]
fn placement_into_an_empty_arrangement_creates_a_group() {
    let mut document = ArrangementDoc::new();
    adopt_unarranged_groups(&mut document, &ArrangementDoc::single(tid(1)));
    let mutation = LayoutMutation::SplitPreservingFocus {
        target: tid(1),
        new_pane: tid(2),
        dir: SplitDir::Vertical,
        ratio: 0.5,
    };
    apply_mutation(&mut document, &mutation).expect("placement succeeds");
    assert_eq!(document.groups.len(), 1);
    assert_eq!(document.active, 0);
    let tree = document.groups[0].state.tree.as_ref().expect("tree");
    assert_eq!(leaves(tree), vec![tid(1), tid(2)]);
}

/// A fallback group is appended only when none of its panes is arranged:
/// an arranged owner keeps its place, an unarranged one gets a new group.
#[test]
fn adopt_unarranged_groups_skips_arranged_panes() {
    let mut document = two_window_workspace();
    adopt_unarranged_groups(&mut document, &ArrangementDoc::single(tid(3)));
    assert_eq!(document.groups.len(), 2);
    adopt_unarranged_groups(&mut document, &ArrangementDoc::single(tid(9)));
    assert_eq!(document.groups.len(), 3);
    assert_eq!(document.active, 0, "an existing active group is kept");
}

// Fixed bytes from the pre-window v1 encoder, kept literal so an encoder
// change cannot weaken the old-schema refusal.
const LEGACY_V1_FIXTURE: &[u8] = &[
    163, 103, 118, 101, 114, 115, 105, 111, 110, 1, 100, 114, 111, 111, 116, 165, 100, 107, 105,
    110, 100, 101, 115, 112, 108, 105, 116, 99, 100, 105, 114, 104, 118, 101, 114, 116, 105, 99,
    97, 108, 101, 114, 97, 116, 105, 111, 249, 52, 0, 100, 108, 101, 102, 116, 162, 100, 107, 105,
    110, 100, 100, 108, 101, 97, 102, 100, 112, 97, 110, 101, 162, 100, 107, 105, 110, 100, 101,
    108, 111, 99, 97, 108, 98, 105, 100, 1, 101, 114, 105, 103, 104, 116, 162, 100, 107, 105, 110,
    100, 100, 108, 101, 97, 102, 100, 112, 97, 110, 101, 162, 100, 107, 105, 110, 100, 101, 108,
    111, 99, 97, 108, 98, 105, 100, 2, 101, 102, 111, 99, 117, 115, 162, 100, 107, 105, 110, 100,
    101, 108, 111, 99, 97, 108, 98, 105, 100, 2,
];

#[test]
fn fixed_v1_fixture_is_refused_without_implicit_rewrite() {
    assert!(matches!(
        ArrangementDoc::decode_cbor(LEGACY_V1_FIXTURE),
        Err(LayoutDecodeError::UnsupportedVersion(1))
    ));
}

#[test]
fn current_fixture_supports_split_and_swap() {
    let fixture = two_window_workspace().encode_cbor().unwrap();
    let mut workspace = ArrangementDoc::decode_cbor(&fixture).unwrap();

    apply_mutation(
        &mut workspace,
        &LayoutMutation::Split {
            target: tid(3),
            new_pane: tid(4),
            dir: SplitDir::Vertical,
            ratio: 0.3,
        },
    )
    .unwrap();
    assert_eq!(workspace.groups[1].state.focus, Some(tid(4)));
    assert_eq!(
        leaves(workspace.groups[1].state.tree.as_ref().unwrap()),
        vec![tid(3), tid(4)]
    );

    apply_mutation(
        &mut workspace,
        &LayoutMutation::Swap {
            first: tid(1),
            second: tid(4),
        },
    )
    .unwrap();
    assert_eq!(
        leaves(workspace.groups[0].state.tree.as_ref().unwrap()),
        vec![tid(4), tid(2)]
    );
    assert_eq!(
        leaves(workspace.groups[1].state.tree.as_ref().unwrap()),
        vec![tid(3), tid(1)]
    );
}

#[test]
fn headless_split_preserves_serialized_focus_and_active_window() {
    let mut workspace = two_window_workspace();
    apply_mutation(
        &mut workspace,
        &LayoutMutation::SplitPreservingFocus {
            target: tid(3),
            new_pane: tid(4),
            dir: SplitDir::Vertical,
            ratio: 0.3,
        },
    )
    .unwrap();
    assert_eq!(workspace.active, 0);
    assert_eq!(workspace.groups[0].state.focus, Some(tid(1)));
    assert_eq!(workspace.groups[1].state.focus, Some(tid(3)));
    assert_eq!(
        leaves(workspace.groups[1].state.tree.as_ref().unwrap()),
        vec![tid(3), tid(4)]
    );
}

#[test]
fn close_collapses_nested_parent_and_repairs_focus() {
    let nested = LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(split(1, 2, SplitDir::Vertical, 0.4)),
        right: Box::new(LayoutNode::Leaf(tid(3))),
    };
    let mut workspace = ArrangementDoc {
        groups: vec![Group::new(
            "1".to_owned(),
            LayoutState {
                tree: Some(nested),
                focus: Some(tid(2)),
            },
        )],
        active: 0,
    };
    apply_mutation(&mut workspace, &LayoutMutation::Close { target: tid(2) }).unwrap();
    assert_eq!(
        leaves(workspace.active_group().unwrap().tree.as_ref().unwrap()),
        vec![tid(1), tid(3)]
    );
    assert_eq!(workspace.active_group().unwrap().focus, Some(tid(1)));
}

#[test]
fn move_collapses_source_and_can_remove_an_empty_window() {
    let mut workspace = two_window_workspace();
    apply_mutation(
        &mut workspace,
        &LayoutMutation::Move {
            source: tid(3),
            target: tid(2),
            dir: SplitDir::Vertical,
            ratio: 0.7,
        },
    )
    .unwrap();
    assert_eq!(workspace.groups.len(), 1);
    assert_eq!(
        leaves(workspace.groups[0].state.tree.as_ref().unwrap()),
        vec![tid(1), tid(2), tid(3)]
    );
    assert_eq!(workspace.groups[0].state.focus, Some(tid(3)));
    let LayoutNode::Split { right, .. } = workspace.groups[0].state.tree.as_ref().unwrap() else {
        panic!("expected outer split");
    };
    assert_eq!(leaves(right), vec![tid(2), tid(3)]);
}

#[test]
fn malformed_and_foreign_targets_are_rejected_without_mutation() {
    assert!(matches!(
        ArrangementDoc::decode_cbor(b"not cbor"),
        Err(LayoutDecodeError::Cbor(_))
    ));
    let mut workspace = two_window_workspace();
    let original = workspace.clone();
    let err =
        apply_mutation(&mut workspace, &LayoutMutation::Close { target: tid(99) }).unwrap_err();
    assert!(matches!(err, LayoutOpsError::ForeignTarget(id) if id == tid(99)));
    assert_eq!(workspace, original);

    let err = apply_mutation(
        &mut workspace,
        &LayoutMutation::Split {
            target: tid(1),
            new_pane: tid(2),
            dir: SplitDir::Horizontal,
            ratio: 0.5,
        },
    )
    .unwrap_err();
    assert!(matches!(err, LayoutOpsError::DuplicatePane(id) if id == tid(2)));
    assert!(matches!(
        apply_mutation(
            &mut ArrangementDoc::single(tid(1)),
            &LayoutMutation::Close { target: tid(1) }
        ),
        Err(LayoutOpsError::LastPane)
    ));

    let original = workspace.clone();
    assert!(matches!(
        apply_mutation(
            &mut workspace,
            &LayoutMutation::Move {
                source: tid(1),
                target: tid(3),
                dir: SplitDir::Horizontal,
                ratio: f32::NAN,
            }
        ),
        Err(LayoutOpsError::Layout(LayoutError::InvalidRatio(ratio))) if ratio.is_nan()
    ));
    assert_eq!(workspace, original, "a rejected move is transactional");
}

/// A raw client connection paired with a scripted server running `spec`.
fn scripted(spec: ScriptSpec) -> (Connection, tokio::task::JoinHandle<Vec<FrameKind>>) {
    let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
    let server = tokio::spawn(ScriptedServer::on_stream(server_stream, spec).run());
    (Connection::from_stream(client_stream), server)
}

#[tokio::test]
async fn mutate_correlates_replies_and_confirms_set() {
    let initial = two_window_workspace();
    // A foreign METADATA_VALUE ahead of the reply must not be taken.
    let spec = ScriptSpec::new()
        .foreign_metadata_value(999)
        .stored_metadata(
            Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
            &layout_key(SessionId::new(7)),
            initial.encode_cbor().unwrap(),
        );
    let (mut client, server_task) = scripted(spec);

    let confirmed = LayoutOps::new(&mut client, SessionId::new(7), 10)
        .mutate(LayoutMutation::Swap {
            first: tid(1),
            second: tid(2),
        })
        .await
        .unwrap();
    assert_eq!(
        leaves(confirmed.groups[0].state.tree.as_ref().unwrap()),
        vec![tid(2), tid(1)]
    );
    drop(client);

    let seen = server_task.await.unwrap();
    assert!(
        matches!(
            seen.first(),
            Some(FrameKind::GetMetadata { request_id: 10, scope, key })
                if *scope == Scope::Group(DEFAULT_LAYOUT_GROUP_ID)
                    && *key == layout_key(SessionId::new(7))
        ),
        "the read leg is a group-scoped GET on the session's layout key; got {:?}",
        seen.first()
    );
    let Some(FrameKind::SetMetadata {
        request_id: 11,
        value,
        ..
    }) = seen.get(1)
    else {
        panic!("expected SET on request 11, got {:?}", seen.get(1));
    };
    let written = ArrangementDoc::decode_cbor(value).unwrap();
    assert_eq!(
        leaves(written.groups[0].state.tree.as_ref().unwrap()),
        vec![tid(2), tid(1)]
    );
    assert!(
        matches!(
            seen.get(2),
            Some(FrameKind::GetMetadata { request_id: 12, .. })
        ),
        "the confirming GET closes the CAS; got {:?}",
        seen.get(2)
    );
}

/// ADR-0129: `--projection` writes and reads the named key it was given,
/// never the shared `phux.tui.layout/v1/<session>` default.
#[tokio::test]
async fn insert_pane_with_projection_writes_the_named_key_and_leaves_the_default_untouched() {
    let initial = two_window_workspace();
    let named_key = "myapp.layout/v1/7".to_owned();
    let spec = ScriptSpec::new().stored_metadata(
        Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        &named_key,
        initial.encode_cbor().unwrap(),
    );
    let (mut client, server_task) = scripted(spec);

    let mut ops =
        LayoutOps::with_key(&mut client, SessionId::new(7), named_key.clone(), 10).unwrap();
    let confirmed = ops
        .mutate(LayoutMutation::Swap {
            first: tid(1),
            second: tid(2),
        })
        .await
        .unwrap();
    assert_eq!(
        leaves(confirmed.groups[0].state.tree.as_ref().unwrap()),
        vec![tid(2), tid(1)]
    );
    drop(ops);
    drop(client);

    let seen = server_task.await.unwrap();
    assert!(
        seen.iter()
            .any(|frame| matches!(frame, FrameKind::GetMetadata { key, .. } if key == &named_key)),
        "the read must target the named projection key, not the default"
    );
    assert!(
        !seen.iter().any(|frame| matches!(
            frame,
            FrameKind::GetMetadata { key, .. } | FrameKind::SetMetadata { key, .. }
                if key == &layout_key(SessionId::new(7))
        )),
        "the shared default key must never be touched by a --projection request"
    );
}

/// A silently dropped SET leaves a decodable stale read-back, which must
/// not be accepted as this write's result.
#[tokio::test]
async fn write_and_confirm_refuses_a_read_back_that_does_not_match_what_was_written() {
    let stale = ArrangementDoc::single(tid(9)).encode_cbor().unwrap();
    let key = layout_key(SessionId::new(7));
    let spec = ScriptSpec::new()
        .stored_metadata(Scope::Group(DEFAULT_LAYOUT_GROUP_ID), &key, stale)
        .drop_metadata_writes(Scope::Group(DEFAULT_LAYOUT_GROUP_ID), &key);
    let (mut client, server_task) = scripted(spec);

    let result = LayoutOps::new(&mut client, SessionId::new(7), 10)
        .write_and_confirm(&ArrangementDoc::single(tid(1)))
        .await;
    assert!(
        matches!(result, Err(LayoutOpsError::NotConfirmed)),
        "a dropped write's stale read-back must not decode as success, got {result:?}"
    );
    drop(client);
    server_task.await.unwrap();
}

#[tokio::test]
async fn correlated_error_is_reported() {
    let spec = ScriptSpec::new().refuse_metadata(ErrorCode::InvalidCommand, "foreign group");
    let (mut client, server_task) = scripted(spec);
    let result = LayoutOps::in_group(&mut client, SessionId::new(1), GroupId::new(77), 5)
        .read()
        .await;
    assert!(matches!(result, Err(LayoutOpsError::Refused(message)) if message == "foreign group"));
    drop(client);
    server_task.await.unwrap();
}

#[tokio::test]
async fn read_or_seed_initializes_an_absent_layout_and_confirms_it() {
    let (mut client, server_task) = scripted(ScriptSpec::new());
    let fallback = ArrangementDoc::single(tid(7));

    let confirmed = LayoutOps::new(&mut client, SessionId::new(3), 20)
        .read_or_seed(fallback.clone())
        .await
        .unwrap();
    assert_eq!(confirmed, fallback);
    drop(client);

    let seen = server_task.await.unwrap();
    assert!(matches!(
        seen.first(),
        Some(FrameKind::GetMetadata { request_id: 20, .. })
    ));
    let Some(FrameKind::SetMetadata {
        request_id: 21,
        key,
        value,
        ..
    }) = seen.get(1)
    else {
        panic!("expected conditional seed SET, got {:?}", seen.get(1));
    };
    assert_eq!(key, &layout_key(SessionId::new(3)));
    assert_eq!(ArrangementDoc::decode_cbor(value).unwrap(), fallback);
    assert!(matches!(
        seen.get(2),
        Some(FrameKind::GetMetadata { request_id: 22, .. })
    ));
}

#[tokio::test]
async fn read_or_seed_preserves_an_existing_layout_without_writing() {
    let existing = two_window_workspace();
    let spec = ScriptSpec::new().stored_metadata(
        Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        &layout_key(SessionId::new(3)),
        existing.encode_cbor().unwrap(),
    );
    let (mut client, server_task) = scripted(spec);

    let confirmed = LayoutOps::new(&mut client, SessionId::new(3), 20)
        .read_or_seed(ArrangementDoc::single(tid(99)))
        .await
        .unwrap();
    assert_eq!(confirmed, existing);
    drop(client);

    let seen = server_task.await.unwrap();
    assert!(matches!(
        seen.as_slice(),
        [FrameKind::GetMetadata { request_id: 20, .. }]
    ));
}

mod organization;
