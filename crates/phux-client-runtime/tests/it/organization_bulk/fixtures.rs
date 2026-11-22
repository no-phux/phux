//! The fixture server, inventories and chunk assertions the bulk-move
//! tests share.

use super::*;
use crate::embedded::{opened, reopen};
use phux_wire_fixtures::HelloOk;

pub(super) const fn session(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}

pub(super) fn resource(n: usize) -> ResourceKey {
    let mut key = [0x40; 16];
    key[..8].copy_from_slice(&(n as u64).to_le_bytes());
    ResourceKey::new(key).unwrap()
}

/// A bare server, advertising `ORGANIZATION` and `SESSION_KILL` when
/// `supported`.
pub(super) fn hello(supported: bool) -> FrameKind {
    let ext = if supported {
        ServerFeatureExtSet::with(&[
            ServerFeatureExt::Organization,
            ServerFeatureExt::SessionKill,
        ])
    } else {
        ServerFeatureExtSet::new()
    };
    HelloOk::new([0xAB; 16], ServerCapabilities::new())
        .features_ext(ext)
        .build()
}

pub(super) fn inventory(
    count: usize,
    moved: usize,
    revision: u64,
    authority: u8,
) -> OrganizationPage {
    OrganizationPage::new(
        AuthorityId::new([authority; 16]).unwrap(),
        Revision::new(revision),
        5,
    )
    .with_sessions(
        (1..=3)
            .map(|n| {
                SessionRecord::new(session(n), format!("session-{n}"), Revision::new(revision))
                    .with_lifetime(SessionLifetime::Persistent)
                    .with_members(
                        u32::try_from(
                            (0..count)
                                .filter(|index| {
                                    let owner = if *index < moved {
                                        session(3)
                                    } else {
                                        source(*index)
                                    };
                                    owner == session(n)
                                })
                                .count(),
                        )
                        .unwrap(),
                    )
            })
            .collect(),
    )
    .with_resources(
        (0..count)
            .map(|n| {
                ResourceRecord::new(
                    resource(n),
                    if n < moved { session(3) } else { source(n) },
                    Revision::new(1),
                )
                .with_live(Some(ResourceId::local(u32::try_from(n + 1).unwrap())))
                .with_activation(Some(ActivationInfo::new(
                    ActivationId::new(1),
                    ServerInstance::new([9; 16]),
                    ExecState::Running,
                )))
            })
            .collect(),
    )
}

pub(super) const fn source(n: usize) -> SessionKey {
    if n.is_multiple_of(2) {
        session(1)
    } else {
        session(2)
    }
}

pub(super) fn seed(
    plane: &mut ControlPlane,
    count: usize,
    moved: usize,
    revision: u64,
    authority: u8,
) {
    let mut all = inventory(count, moved, revision, authority);
    let resources = std::mem::take(&mut all.resources);
    let page_count = resources.len().div_ceil(128);
    for (index, chunk) in resources.chunks(128).enumerate() {
        let request = commands(plane)
            .into_iter()
            .find_map(|(id, command)| {
                matches!(command, Command::GetOrganization { .. }).then_some(id)
            })
            .expect("inventory read");
        let mut page = all.clone().with_resources(chunk.to_vec());
        if index > 0 {
            page.sessions.clear();
        }
        if index + 1 < page_count {
            page.next_cursor = Some(vec![u8::try_from(index + 1).unwrap()]);
        }
        answer(
            plane,
            request,
            CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        );
    }
}

pub(super) fn plane(count: usize) -> ControlPlane {
    let mut plane = opened(ControlOptions::default(), hello(true));
    seed(&mut plane, count, 0, 2, 1);
    commands(&mut plane);
    plane.take_events();
    plane
}

pub(super) fn selection(count: usize) -> Vec<ResourceKey> {
    (0..count).map(resource).collect()
}

pub(super) fn chunk(
    plane: &mut ControlPlane,
    expected: OperationId,
    start: usize,
    end: usize,
    revision: u64,
) -> (u32, Command) {
    let sent = commands(plane);
    let [
        (
            request,
            command @ Command::ResourceMoveMany {
                operation,
                items,
                to,
                to_expected,
            },
        ),
    ] = sent.as_slice()
    else {
        panic!("one move chunk expected: {sent:?}");
    };
    assert_eq!(*operation, expected);
    assert_eq!(*to, session(3));
    assert_eq!(*to_expected, Revision::new(revision));
    assert_eq!(items.len(), end - start);
    for (n, item) in (start..end).zip(items) {
        assert_eq!(item.resource, resource(n));
        assert_eq!(item.from, source(n));
        assert_eq!(item.from_expected, Revision::new(revision));
    }
    (*request, command.clone())
}

pub(super) fn commit(
    plane: &mut ControlPlane,
    request: u32,
    operation: OperationId,
    revision: u64,
) {
    answer(
        plane,
        request,
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            operation,
            Revision::new(revision),
        )))),
    );
}

pub(super) fn outcomes(plane: &mut ControlPlane) -> Vec<(OperationId, OrgOutcome)> {
    plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::OrganizationOperation { operation, outcome } => Some((operation, outcome)),
            _ => None,
        })
        .collect()
}

pub(super) fn reconnect(plane: &mut ControlPlane, supported: bool) {
    plane.connection_lost(None);
    reopen(plane, hello(supported));
}

pub(super) fn answer_page(plane: &mut ControlPlane, request: u32, page: OrganizationPage) {
    answer(
        plane,
        request,
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
    );
}

pub(super) fn expired_cursor() -> CommandResult {
    CommandResult::Refused(Refusal::InvalidArgument {
        field: "cursor".to_owned(),
        reason: "expired".to_owned(),
    })
}

pub(super) fn read_failed() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: "read failed".to_owned(),
    }
}

pub(super) fn ids(results: &[(OperationId, OrgOutcome)]) -> Vec<OperationId> {
    results.iter().map(|(id, _)| *id).collect()
}

/// A 257-resource move of everything to session 3, its first chunk sent.
pub(super) fn first_chunk_sent() -> (ControlPlane, Vec<OperationId>, u32) {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    (plane, operations, request)
}

/// [`first_chunk_sent`] with that chunk committed and the post-receipt
/// inventory read queued; returns that read's request id.
pub(super) fn first_chunk_committed() -> (ControlPlane, Vec<OperationId>, u32) {
    let (mut plane, operations, request) = first_chunk_sent();
    commit(&mut plane, request, operations[0], 3);
    let (read, _) = commands(&mut plane).remove(0);
    (plane, operations, read)
}

/// The first chunk committed and the second never sent.
pub(super) fn assert_tail_cancelled(plane: &mut ControlPlane) {
    assert_tail_cancelled_in(plane, "");
}

/// [`assert_tail_cancelled`], naming table case `label` on failure.
pub(super) fn assert_tail_cancelled_in(plane: &mut ControlPlane, label: &str) {
    let results = outcomes(plane);
    assert!(
        matches!(
            results.as_slice(),
            [
                (_, OrgOutcome::Committed(_)),
                (_, OrgOutcome::NotSent { .. })
            ]
        ),
        "{label}: {results:?}"
    );
}
