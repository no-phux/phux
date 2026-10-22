//! Organization replica tests.

use super::*;
use phux_protocol::ids::{ActivationId, ResourceKind, ServerInstance};
use phux_protocol::wire::frame::{ExecState, Outcome};

fn authority() -> AuthorityId {
    AuthorityId::new([1; 16]).unwrap()
}

fn skey(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}

fn rkey(n: u8) -> ResourceKey {
    ResourceKey::new([0x40 | n; 16]).unwrap()
}

fn running(n: u8, session: SessionKey) -> ResourceRecord {
    ResourceRecord::new(rkey(n), session, Revision::new(1))
        .with_activation(Some(ActivationInfo::new(
            ActivationId::new(1),
            ServerInstance::new([9; 16]),
            ExecState::Running,
        )))
        .with_live(Some(ResourceId::local(u32::from(n))))
}

fn page(
    seq: u64,
    sessions: Vec<SessionRecord>,
    resources: Vec<ResourceRecord>,
) -> OrganizationPage {
    OrganizationPage::new(authority(), Revision::new(10), seq)
        .with_sessions(sessions)
        .with_resources(resources)
}

fn seeded() -> OrganizationReplica {
    let mut replica = OrganizationReplica::new();
    replica.begin_seed(OrgFilter::All);
    let work = SessionRecord::new(skey(1), "work", Revision::new(2)).with_members(1);
    assert_eq!(
        replica.apply_page(page(5, vec![work], vec![running(1, skey(1))])),
        ReplicaAction::Seeded
    );
    replica
}

#[test]
fn a_paged_seed_installs_records_and_the_handle_map() {
    let mut replica = OrganizationReplica::new();
    assert_eq!(replica.state(), ReplicaState::Empty);
    replica.begin_seed(OrgFilter::All);
    let first = page(
        5,
        vec![SessionRecord::new(skey(1), "work", Revision::new(2))],
        vec![],
    )
    .with_next_cursor(Some(vec![1]));
    let ReplicaAction::NextPage(Command::GetOrganization { cursor, .. }) =
        replica.apply_page(first)
    else {
        panic!("expected the next page");
    };
    assert_eq!(cursor, Some(vec![1]));
    assert!(replica.seeding());
    assert_eq!(
        replica.apply_page(page(99, vec![], vec![running(1, skey(1))])),
        ReplicaAction::Seeded
    );
    assert!(replica.is_live());
    assert_eq!(replica.journal_seq(), 5, "the first page's cut");
    assert_eq!(replica.authority(), Some(authority()));
    assert_eq!(
        replica
            .resource_by_live(&ResourceId::local(1))
            .map(|r| r.key),
        Some(rkey(1))
    );
}

#[test]
fn events_apply_in_journal_order_and_old_ones_are_skipped() {
    let mut replica = seeded();
    let rename = |name: &str, revision| AgentEvent::SessionRenamed {
        session: skey(1),
        name: name.to_owned(),
        revision: Revision::new(revision),
    };
    assert_eq!(
        replica.apply_event(Some(4), &rename("old", 1)),
        ReplicaAction::None,
        "before the cut"
    );
    assert_eq!(replica.session(&skey(1)).unwrap().name, "work");
    assert_eq!(
        replica.apply_event(Some(6), &rename("notes", 3)),
        ReplicaAction::Changed
    );
    assert_eq!(replica.session(&skey(1)).unwrap().name, "notes");
    assert_eq!(replica.journal_seq(), 6);
    replica.apply_event(
        Some(7),
        &AgentEvent::ActivationChanged {
            resource: rkey(1),
            activation: ActivationId::new(1),
            state: ExecState::Completed,
            outcome: Some(Outcome::Exited(0)),
        },
    );
    assert_eq!(
        replica
            .resource(&rkey(1))
            .unwrap()
            .activation
            .unwrap()
            .state,
        ExecState::Completed
    );
    replica.apply_event(
        Some(8),
        &AgentEvent::ResourceRemoved {
            resource: rkey(1),
            session: skey(1),
            kind: ResourceKind::Terminal,
        },
    );
    assert!(replica.handles().is_empty());
    assert_eq!(replica.session(&skey(1)).unwrap().members, 0);
}

#[test]
fn a_project_event_sets_replaces_and_clears_only_the_link_attr() {
    let mut replica = OrganizationReplica::new();
    replica.begin_seed(OrgFilter::All);
    let work = SessionRecord::new(skey(1), "work", Revision::new(2)).with_attrs(vec![
        ("project".to_owned(), "checkout-dir".to_owned()),
        ("host".to_owned(), "mini".to_owned()),
    ]);
    replica.apply_page(page(5, vec![work], vec![]));
    let set = |project: Option<&str>, revision| AgentEvent::SessionProjectSet {
        session: skey(1),
        project: project.map(str::to_owned),
        revision: Revision::new(revision),
    };
    let attrs = |replica: &OrganizationReplica| replica.session(&skey(1)).unwrap().attrs.clone();
    let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());

    assert_eq!(
        replica.apply_event(Some(6), &set(Some("phux"), 3)),
        ReplicaAction::Changed
    );
    assert_eq!(
        attrs(&replica),
        vec![
            pair("project.link", "phux"),
            pair("project", "checkout-dir"),
            pair("host", "mini")
        ]
    );
    assert_eq!(
        replica.session(&skey(1)).unwrap().revision,
        Revision::new(3)
    );
    replica.apply_event(Some(7), &set(Some("ghostty"), 4));
    assert_eq!(
        attrs(&replica),
        vec![
            pair("project.link", "ghostty"),
            pair("project", "checkout-dir"),
            pair("host", "mini")
        ]
    );
    replica.apply_event(Some(8), &set(None, 5));
    assert_eq!(
        attrs(&replica),
        vec![pair("project", "checkout-dir"), pair("host", "mini")],
        "unlinking leaves the recipe's free-text label"
    );
    assert_eq!(
        replica.session(&skey(1)).unwrap().revision,
        Revision::new(5)
    );
}

#[test]
fn events_during_a_seed_are_buffered_and_replayed_past_the_cut() {
    let mut replica = OrganizationReplica::new();
    replica.begin_seed(OrgFilter::All);
    let renamed = |seq_name: &str| AgentEvent::SessionRenamed {
        session: skey(1),
        name: seq_name.to_owned(),
        revision: Revision::new(9),
    };
    replica.apply_event(Some(4), &renamed("before-cut"));
    replica.apply_event(Some(6), &renamed("after-cut"));
    replica.apply_page(page(
        5,
        vec![SessionRecord::new(skey(1), "work", Revision::new(2))],
        vec![],
    ));
    assert_eq!(replica.session(&skey(1)).unwrap().name, "after-cut");
}

#[test]
fn a_journal_gap_asks_for_a_reseed() {
    let mut replica = seeded();
    let action = replica.apply_event(
        None,
        &AgentEvent::JournalGap {
            first_missing: 6,
            last_missing: 9,
        },
    );
    assert_eq!(action, ReplicaAction::Reseed);
    assert_eq!(replica.state(), ReplicaState::Stale);
    replica.begin_seed(OrgFilter::All);
    replica.apply_page(page(20, vec![], vec![]));
    assert!(replica.is_live());
    assert!(replica.sessions().is_empty());
}

#[test]
fn incomplete_events_ask_for_a_session_refetch() {
    let mut replica = seeded();
    let action = replica.apply_event(
        Some(6),
        &AgentEvent::ResourceCreated {
            resource: rkey(2),
            session: skey(1),
            kind: ResourceKind::Terminal,
        },
    );
    assert_eq!(action, ReplicaAction::RefetchSession(skey(1)));
    assert_eq!(replica.members(&skey(1)).count(), 2);
    // The session refetch replaces only that session's records.
    replica.begin_seed(OrgFilter::Session(skey(1)));
    replica.apply_page(page(
        7,
        vec![SessionRecord::new(skey(1), "work", Revision::new(3)).with_members(2)],
        vec![running(1, skey(1)), running(2, skey(1))],
    ));
    assert_eq!(
        replica.journal_seq(),
        6,
        "a session refetch does not move the cursor"
    );
    assert_eq!(
        replica
            .resource_by_live(&ResourceId::local(2))
            .map(|r| r.key),
        Some(rkey(2))
    );
}

#[test]
fn membership_moves_and_the_inbox_projection() {
    let mut replica = OrganizationReplica::new();
    replica.begin_seed(OrgFilter::All);
    let first = OrganizationPage::new(authority(), Revision::new(1), 1)
        .with_inbox(Some(skey(9)))
        .with_sessions(vec![
            SessionRecord::new(skey(9), "inbox", Revision::new(1)),
            SessionRecord::new(skey(2), "imported", Revision::new(1))
                .with_awaiting_organization(true)
                .with_members(1),
            SessionRecord::new(skey(1), "work", Revision::new(1)),
        ])
        .with_resources(vec![running(1, skey(2))]);
    replica.apply_page(first);
    let view = replica.intake_view();
    assert_eq!(view, replica.inbox_view());
    assert_eq!(view.intake(), replica.intake());
    assert_eq!(replica.intake(), replica.inbox());
    assert_eq!(
        view.sessions.iter().map(|s| s.key).collect::<Vec<_>>(),
        vec![skey(9), skey(2)]
    );
    assert_eq!(view.resources.len(), 1);
    replica.apply_event(
        Some(2),
        &AgentEvent::MembershipChanged {
            resource: rkey(1),
            from: skey(2),
            to: skey(1),
            from_revision: Revision::new(2),
            to_revision: Revision::new(2),
        },
    );
    replica.apply_event(
        Some(3),
        &AgentEvent::IntakeChanged {
            session: skey(2),
            awaiting_organization: false,
            revision: Revision::new(3),
        },
    );
    let view = replica.intake_view();
    assert_eq!(view.sessions.len(), 1);
    assert!(view.resources.is_empty());
    assert_eq!(replica.members(&skey(1)).count(), 1);
    assert_eq!(replica.session(&skey(1)).unwrap().members, 1);
}
