//! Organization: extracted from the shared module.
use super::*;

// ---- membership ------------------------------------------------------------

#[test]
pub(super) fn resource_move_refusals_and_ephemeral_source_deletion() {
    let mut h = Harness::new();
    let quick = h.create_session("quick", SessionLifetime::Ephemeral);
    let dest = h.create_session("dest", SessionLifetime::Persistent);
    let r = h.session(quick).members[0];
    let (rq, rd) = (h.session(quick).revision, h.session(dest).revision);

    assert_eq!(
        h.run(Op::ResourceMove {
            resource: r,
            from: dest,
            from_expected: rd,
            to: quick,
            to_expected: rq,
        })
        .err(),
        Some(Refusal::Conflict {
            current: h.resource(r).revision
        }),
        "the caller's idea of the source is stale"
    );
    assert_eq!(
        h.run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: dest,
            to_expected: rd.next(),
        })
        .err(),
        Some(Refusal::Conflict { current: rd })
    );
    assert_eq!(
        h.run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: SessionKey::from_bytes([7; 16]),
            to_expected: rd,
        })
        .err(),
        Some(Refusal::NotFound)
    );
    let applied = h
        .run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: dest,
            to_expected: rd,
        })
        .expect("move");
    assert!(applied.effects.is_empty(), "a move never touches execution");
    assert!(h.reg.org().session(&quick).is_none(), "emptied ephemeral");
    assert_eq!(h.session(dest).members, vec![r]);
    assert!(
        applied
            .transition
            .events
            .iter()
            .any(|e| matches!(e, Event::SessionDeleted { key, .. } if *key == quick))
    );
}

#[test]
pub(super) fn moving_out_prunes_the_source_arrangement() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let r1 = h.shell_in(a);
    let r2 = h.shell_in(a);
    let tree = LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
        right: Box::new(LayoutNode::Leaf(EntryRef::member([2; 16], r2))),
    };
    h.run(Op::ArrangementReplace {
        owner: ArrangementOwner::Session(a),
        expected: Revision::FIRST,
        groups: vec![Group {
            id: [5; 16],
            name: None,
            tree,
        }],
    })
    .expect("arrangement");
    let ak = h.session(a).arrangement.expect("linked");
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);
    h.run(Op::ResourceMove {
        resource: r1,
        from: a,
        from_expected: ra,
        to: b,
        to_expected: rb,
    })
    .expect("move");
    let arrangement = h.reg.org().arrangement(&ak).expect("kept");
    assert_eq!(arrangement.resources(), vec![r2]);
    assert_eq!(arrangement.revision, Revision(3));
}

#[test]
pub(super) fn undoing_a_move_restores_the_source_placement() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let r1 = h.shell_in(a);
    let r2 = h.shell_in(a);
    let groups = vec![Group {
        id: [5; 16],
        name: None,
        tree: LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.3,
            left: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
            right: Box::new(LayoutNode::Leaf(EntryRef::member([2; 16], r2))),
        },
    }];
    h.run(Op::ArrangementReplace {
        owner: ArrangementOwner::Session(a),
        expected: Revision::FIRST,
        groups: groups.clone(),
    })
    .expect("arrangement");
    let ak = h.session(a).arrangement.expect("linked");
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);
    let moved = h
        .run(Op::ResourceMove {
            resource: r1,
            from: a,
            from_expected: ra,
            to: b,
            to_expected: rb,
        })
        .expect("move");
    h.run(Op::OrganizationUndo {
        original: moved.operation,
        expected: vec![],
    })
    .expect("undo");
    assert_eq!(h.resource(r1).session, a);
    let arrangement = h.reg.org().arrangement(&ak).expect("kept");
    assert_eq!(arrangement.groups, groups, "the pane is back where it was");
}

#[test]
pub(super) fn undoing_a_move_out_of_a_deleted_session_refuses() {
    let mut h = Harness::new();
    let quick = h.create_session("quick", SessionLifetime::Ephemeral);
    let dest = h.create_session("dest", SessionLifetime::Persistent);
    let r = h.session(quick).members[0];
    let (rq, rd) = (h.session(quick).revision, h.session(dest).revision);
    let moved = h
        .run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: dest,
            to_expected: rd,
        })
        .expect("move");
    assert!(
        h.reg.org().session(&quick).is_none(),
        "the empty ephemeral went"
    );
    assert_eq!(
        h.run(Op::OrganizationUndo {
            original: moved.operation,
            expected: vec![],
        })
        .err(),
        Some(Refusal::InvalidArgument {
            field: "undo",
            reason: "the original session no longer exists".to_owned(),
        })
    );
    assert_eq!(h.resource(r).session, dest, "nothing is resurrected");
}

#[test]
pub(super) fn move_many_is_atomic_and_bounded() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let dest = h.create_session("dest", SessionLifetime::Persistent);
    let ra = h.shell_in(a);
    let rb = h.shell_in(b);
    let pins = |h: &Harness| {
        vec![
            SourcePin {
                session: a,
                expected: h.session(a).revision,
            },
            SourcePin {
                session: b,
                expected: h.session(b).revision,
            },
        ]
    };
    let rd = h.session(dest).revision;

    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, ra],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ResourceKey::from_bytes([0; 16]); 300],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, rb],
            sources: pins(&h)[..1].to_vec(),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument {
            field: "sources",
            ..
        })
    ));
    let mut stale = pins(&h);
    stale[1].expected = stale[1].expected.next();
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, rb],
            sources: stale,
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::Conflict { .. })
    ));
    assert_eq!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, ResourceKey::from_bytes([0; 16])],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        })
        .err(),
        Some(Refusal::NotFound),
        "one missing target refuses the whole batch"
    );

    h.run(Op::ResourceMoveMany {
        resources: vec![ra, rb],
        sources: pins(&h),
        to: dest,
        to_expected: rd,
    })
    .expect("move many");
    assert_eq!(h.session(dest).members, vec![ra, rb]);
    assert_eq!(h.session(dest).revision, rd.next(), "one bump per record");
}

// ---- intake and undo -------------------------------------------------------

#[test]
pub(super) fn undo_restores_membership_only_while_revisions_match() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let r = h.shell_in(a);
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);
    let moved = h
        .run(Op::ResourceMove {
            resource: r,
            from: a,
            from_expected: ra,
            to: b,
            to_expected: rb,
        })
        .expect("move");
    let receipt = moved.transition.receipt.clone().expect("receipt");
    assert!(receipt.undo.is_some());

    let undone = h
        .run(Op::OrganizationUndo {
            original: moved.operation,
            expected: vec![],
        })
        .expect("undo");
    assert_eq!(h.resource(r).session, a);
    assert!(undone.effects.is_empty(), "undo never touches execution");

    // The state moved on, so undoing the move again conflicts.
    assert!(matches!(
        h.run(Op::OrganizationUndo {
            original: moved.operation,
            expected: vec![],
        }),
        Err(Refusal::Conflict { .. })
    ));
    // Undoing the undo redoes the move.
    h.run(Op::OrganizationUndo {
        original: undone.operation,
        expected: vec![],
    })
    .expect("redo");
    assert_eq!(h.resource(r).session, b);

    assert_eq!(
        h.run(Op::OrganizationUndo {
            original: phux_core::OperationId::from_bytes([0; 16]),
            expected: vec![],
        })
        .err(),
        Some(Refusal::NotFound)
    );
    let rename = h
        .run(Op::SessionRename {
            session: a,
            name: "renamed".to_owned(),
        })
        .expect("rename");
    assert!(matches!(
        h.run(Op::OrganizationUndo {
            original: rename.operation,
            expected: vec![],
        }),
        Err(Refusal::Unsupported { .. })
    ));
}

#[test]
pub(super) fn inbox_resolve_clears_intake_and_undo_restores_it() {
    let mut h = Harness::new();
    let s = h.create_session("imported", SessionLifetime::Persistent);
    // Mark it imported the way migration does: a committed record write.
    let mut record = h.session(s);
    record.awaiting_organization = true;
    record.revision = record.revision.next();
    h.reg.commit(&phux_core::Transition {
        deltas: vec![phux_core::Delta::PutSession(record)],
        events: vec![],
        receipt: None,
    });
    let rev = h.session(s).revision;
    assert_eq!(
        h.run(Op::InboxResolve {
            session: s,
            expected: rev.next(),
        })
        .err(),
        Some(Refusal::Conflict { current: rev })
    );
    let resolved = h
        .run(Op::InboxResolve {
            session: s,
            expected: rev,
        })
        .expect("resolve");
    assert!(!h.session(s).awaiting_organization);
    assert!(resolved.transition.events.iter().any(|e| matches!(
        e,
        Event::SessionIntakeChanged {
            awaiting_organization: false,
            ..
        }
    )));
    h.run(Op::OrganizationUndo {
        original: resolved.operation,
        expected: vec![],
    })
    .expect("undo");
    assert!(h.session(s).awaiting_organization);
}

// ---- arrangements ----------------------------------------------------------

#[test]
pub(super) fn arrangement_replace_validates_members_and_revisions() {
    let mut h = Harness::new();
    // The Inbox of a bare organization has no arrangement yet, so the first
    // replace creates it.
    let a = h.inbox();
    let b = h.create_session("b", SessionLifetime::Persistent);
    let mine = h.shell_in(a);
    let theirs = h.shell_in(b);
    let group = |resource, entry| Group {
        id: [entry; 16],
        name: Some("main".to_owned()),
        tree: LayoutNode::Leaf(EntryRef::member([entry; 16], resource)),
    };
    let owner = ArrangementOwner::Session(a);

    assert!(matches!(
        h.run(Op::ArrangementReplace {
            owner,
            expected: Revision::ZERO,
            groups: vec![group(theirs, 1)],
        }),
        Err(Refusal::InvalidArgument {
            field: "groups",
            ..
        })
    ));
    assert!(matches!(
        h.run(Op::ArrangementReplace {
            owner,
            expected: Revision::ZERO,
            groups: vec![group(mine, 1), group(mine, 1)],
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert_eq!(
        h.run(Op::ArrangementReplace {
            owner,
            expected: Revision::FIRST,
            groups: vec![group(mine, 1)],
        })
        .err(),
        Some(Refusal::Conflict {
            current: Revision::ZERO
        })
    );
    let applied = h
        .run(Op::ArrangementReplace {
            owner,
            expected: Revision::ZERO,
            groups: vec![group(mine, 1)],
        })
        .expect("create");
    let Reply::Ok { created, revision } = applied.reply else {
        panic!("expected Ok");
    };
    assert_eq!(revision, Some(Revision::FIRST));
    assert_eq!(h.session(a).arrangement, created.arrangement);
    h.run(Op::ArrangementReplace {
        owner,
        expected: Revision::FIRST,
        groups: vec![],
    })
    .expect("replace");
    assert!(matches!(
        h.run(Op::ArrangementReplace {
            owner: ArrangementOwner::Workspace(phux_core::WorkspaceKey::from_bytes([1; 16])),
            expected: Revision::ZERO,
            groups: vec![],
        }),
        Err(Refusal::Unsupported { .. })
    ));
}

// ---- retries ---------------------------------------------------------------

#[test]
pub(super) fn retries_replay_the_receipt_and_reject_a_changed_payload() {
    let mut h = Harness::new();
    let operation = h.next_op();
    let cmd = Command {
        operation,
        op: create("work", SessionLifetime::Persistent, Some(shell())),
    };
    let owner = Actor::owner();
    let first = h.run_cmd(&owner, &cmd).expect("first");
    let again = h.run_cmd(&owner, &cmd).expect("retry");
    assert!(again.replayed);
    assert_eq!(again.reply, first.reply);
    assert!(again.effects.is_empty() && again.transition.is_empty());
    assert_eq!(h.reg.org().sessions().count(), 2, "no second session");

    let changed = Command {
        operation,
        op: create("other", SessionLifetime::Persistent, None),
    };
    assert_eq!(
        h.run_cmd(&owner, &changed).err(),
        Some(Refusal::DuplicateOperation)
    );
}
