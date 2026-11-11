//! Sessions: extracted from the shared module.
use super::*;

// ---- sessions --------------------------------------------------------------

#[test]
pub(super) fn session_create_commits_session_resource_and_launch_together() {
    let mut h = Harness::new();
    let applied = h
        .run(create("work", SessionLifetime::Persistent, Some(shell())))
        .expect("create");
    let (s, r) = created(&applied);
    let (s, r) = (s.expect("session"), r.expect("resource"));
    assert_eq!(h.session(s).members, vec![r]);
    assert_eq!(h.session(s).revision, Revision::FIRST);
    assert!(matches!(
        applied.effects.as_slice(),
        [Effect::Launch { resource, activation, .. }] if *resource == r && *activation == A1
    ));
    assert!(matches!(
        applied.transition.events.as_slice(),
        [
            Event::SessionCreated { .. },
            Event::ResourceCreated { .. },
            Event::ActivationChanged {
                state: ExecState::Starting,
                ..
            },
            Event::ArrangementChanged { .. }
        ]
    ));
    assert_eq!(h.resource(r).provenance.operation, applied.operation);
    // The arrangement comes with the session, its first member placed.
    let ak = h.session(s).arrangement.expect("arranged at creation");
    let arrangement = h.reg.org().arrangement(&ak).expect("committed").clone();
    assert_eq!(arrangement.revision, Revision::FIRST);
    assert_eq!(arrangement.groups.len(), 1);
    assert_eq!(arrangement.groups[0].name, None);
    assert_eq!(arrangement.resources(), vec![r]);
    assert!(matches!(
        applied.reply,
        Reply::Ok { created, .. } if created.arrangement == Some(ak)
    ));

    // A bare create launches nothing and arranges nothing.
    let bare = h
        .run(create("empty", SessionLifetime::Persistent, None))
        .expect("bare");
    assert!(bare.effects.is_empty());
    let (empty, _) = created(&bare);
    let ak = h
        .session(empty.expect("session"))
        .arrangement
        .expect("arranged at creation");
    assert!(
        h.reg
            .org()
            .arrangement(&ak)
            .expect("committed")
            .groups
            .is_empty()
    );
}

#[test]
pub(super) fn a_first_group_gives_every_member_an_even_share() {
    let members: Vec<_> = (1..=3_u8)
        .map(|n| ResourceKey::from_bytes([n; 16]))
        .collect();
    let mut keys = phux_core::SequentialKeys::new(*b"groupkey");
    let group = phux_core::first_group(&members, &mut keys).expect("a group");
    let LayoutNode::Split {
        ratio, ref left, ..
    } = group.tree
    else {
        panic!("three members split: {:?}", group.tree);
    };
    assert!((ratio - 2.0 / 3.0).abs() < f32::EPSILON);
    assert!(matches!(
        **left,
        LayoutNode::Split { ratio, .. } if (ratio - 0.5).abs() < f32::EPSILON
    ));
    let leaves: Vec<ResourceKey> = group
        .tree
        .leaves()
        .into_iter()
        .filter_map(EntryRef::resource)
        .collect();
    assert_eq!(leaves, members);
    assert!(phux_core::first_group(&[], &mut keys).is_none());
}

#[test]
pub(super) fn session_create_refusals() {
    let mut h = Harness::new();
    h.create_session("work", SessionLifetime::Persistent);
    assert_eq!(
        h.run(create("work", SessionLifetime::Persistent, None))
            .err(),
        Some(Refusal::NameTaken)
    );
    assert_eq!(
        h.run(create("intake", SessionLifetime::Persistent, None))
            .err(),
        Some(Refusal::NameTaken)
    );
    for bad in ["", "   ", "tab\there", &"x".repeat(257)] {
        assert!(matches!(
            h.run(create(bad, SessionLifetime::Persistent, None)),
            Err(Refusal::InvalidArgument { field: "name", .. })
        ));
    }
    assert!(matches!(
        h.run(create("quick", SessionLifetime::Ephemeral, None)),
        Err(Refusal::InvalidArgument {
            field: "resource",
            ..
        })
    ));
    // A failed first resource refuses the whole create.
    let no_launch = NewResource {
        launch: None,
        ..shell()
    };
    assert!(matches!(
        h.run(create("x", SessionLifetime::Persistent, Some(no_launch))),
        Err(Refusal::InvalidArgument {
            field: "launch",
            ..
        })
    ));
    assert!(h.reg.org().session_by_name("x").is_none());
}

/// A project link is set, kept on an identical write, refused when blank,
/// padded, over-long or holding a control character, cleared with `None`
/// without touching the free-text label, and has no inverse, like a rename.
#[test]
pub(super) fn session_set_project_links_and_clears_only_the_link() {
    let mut h = Harness::new();
    // A recipe's free-text label, which a link must never touch.
    let recipe = Attrs {
        project: Some("checkout-dir".to_owned()),
        ..Attrs::default()
    };
    let created = h
        .run(Op::SessionCreate {
            name: "a".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: recipe,
            resource: None,
        })
        .expect("create");
    let s = crate::support::created(&created).0.expect("session");
    let project = |project: Option<&str>| Op::SessionSetProject {
        session: s,
        project: project.map(str::to_owned),
    };
    let r1 = h.session(s).revision;
    let linked = h.run(project(Some("phux"))).expect("link");
    assert_eq!(h.session(s).attrs.project_link.as_deref(), Some("phux"));
    assert_eq!(h.session(s).revision, r1.next());
    assert!(matches!(
        linked.transition.events.as_slice(),
        [Event::SessionProjectSet { key, project: Some(p), revision }]
            if *key == s && p == "phux" && *revision == r1.next()
    ));
    let same = h.run(project(Some("phux"))).expect("idempotent link");
    assert!(same.transition.events.is_empty());
    assert_eq!(h.session(s).revision, r1.next());
    for bad in [
        "",
        "  ",
        "a\nb",
        " phux",
        "phux ",
        &"x".repeat(phux_core::project_identity::MAX_ATTR_BYTES + 1),
    ] {
        assert!(matches!(
            h.run(project(Some(bad))),
            Err(Refusal::InvalidArgument {
                field: "project",
                ..
            })
        ));
    }
    assert_eq!(
        h.run(Op::SessionSetProject {
            session: SessionKey::from_bytes([7; 16]),
            project: None,
        })
        .err(),
        Some(Refusal::NotFound)
    );
    assert_eq!(
        h.session(s).attrs.project.as_deref(),
        Some("checkout-dir"),
        "linking leaves the recipe label"
    );
    let link_op = h.run(project(None)).expect("unlink");
    assert_eq!(h.session(s).attrs.project_link, None);
    assert_eq!(h.session(s).attrs.project.as_deref(), Some("checkout-dir"));
    assert!(matches!(
        link_op.transition.events.as_slice(),
        [Event::SessionProjectSet { project: None, .. }]
    ));
    assert!(matches!(
        h.run(Op::OrganizationUndo {
            original: link_op.operation,
            expected: vec![],
        }),
        Err(Refusal::Unsupported { .. })
    ));
}

#[test]
pub(super) fn session_rename_and_lifetime() {
    let mut h = Harness::new();
    let s = h.create_session("a", SessionLifetime::Persistent);
    h.create_session("b", SessionLifetime::Persistent);
    let r1 = h.session(s).revision;

    assert_eq!(
        h.run(Op::SessionRename {
            session: s,
            name: "b".to_owned(),
        })
        .err(),
        Some(Refusal::NameTaken)
    );
    assert_eq!(
        h.run(Op::SessionRename {
            session: SessionKey::from_bytes([7; 16]),
            name: "z".to_owned(),
        })
        .err(),
        Some(Refusal::NotFound)
    );
    h.run(Op::SessionRename {
        session: s,
        name: "c".to_owned(),
    })
    .expect("rename");
    assert_eq!(h.session(s).name, "c");
    assert_eq!(h.session(s).revision, r1.next());

    // Ephemeral on an empty session deletes it.
    h.run(Op::SessionSetLifetime {
        session: s,
        lifetime: SessionLifetime::Ephemeral,
    })
    .expect("ephemeral");
    assert!(h.reg.org().session(&s).is_none());

    let inbox = h.inbox();
    assert!(matches!(
        h.run(Op::SessionSetLifetime {
            session: inbox,
            lifetime: SessionLifetime::Ephemeral,
        }),
        Err(Refusal::InvalidArgument {
            field: "lifetime",
            ..
        })
    ));
}

#[test]
pub(super) fn session_delete_refuses_members_and_never_deletes_inbox() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let t = h.create_resource(s, job());
    assert_eq!(
        h.run(Op::SessionDelete { session: s }).err(),
        Some(Refusal::NotEmpty { members: 1 }),
        "work added after the caller looked survives"
    );
    h.report(ended(t, exited())).expect("ended");
    h.run(Op::ResourceRemove { resource: t }).expect("remove");
    h.run(Op::SessionDelete { session: s }).expect("delete");
    assert!(h.reg.org().session(&s).is_none());

    let inbox = h.inbox();
    assert!(matches!(
        h.run(Op::SessionDelete { session: inbox }),
        Err(Refusal::InvalidArgument { .. })
    ));
}

#[test]
pub(super) fn session_kill_removes_every_member_terminates_the_running_and_deletes() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let running = h.shell_in(s);
    let done = h.create_resource(s, job());
    h.report(ended(done, exited())).expect("ended");
    let applied = h.run(Op::SessionKill { session: s }).expect("kill");
    assert!(matches!(
        applied.effects.as_slice(),
        [Effect::Terminate { resource, mode: TerminateMode::Graceful, .. }] if *resource == running
    ));
    assert!(h.reg.org().session(&s).is_none());
    assert!(h.reg.org().resource(&running).is_none());
    assert!(h.reg.org().resource(&done).is_none());

    // A retry replays; a session already gone is killed.
    let replay = h
        .run_cmd(
            &Actor::owner(),
            &Command {
                operation: applied.operation,
                op: Op::SessionKill { session: s },
            },
        )
        .expect("replay");
    assert!(replay.replayed);
    h.run(Op::SessionKill { session: s }).expect("already gone");

    let inbox = h.inbox();
    assert!(matches!(
        h.run(Op::SessionKill { session: inbox }),
        Err(Refusal::InvalidArgument { .. })
    ));
}
