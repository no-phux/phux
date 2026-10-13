//! The C lane against a scripted `ORGANIZATION` server: the replica seed,
//! enriched session and resource records, typed operations with per-target
//! results, typed create and rename, close by terminate, and the stored
//! arrangement's compare-and-swap.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use super::*;
use crate::c::arrangement::{
    PhuxArrangementInfo, PhuxArrangementMutation, phux_client_arrangement_info,
    phux_client_arrangement_mutate,
};
use crate::c::test_support::{self, attached_frame, feed, feed_bootstrap, hello_ok, new_client};
use crate::c::{
    PhuxAttachOptions, PhuxOperationResult, PhuxSessionCreateInfo, PhuxSessionInfo,
    PhuxSessionRenameInfo, phux_client_create_session, phux_client_operation_count,
    phux_client_operation_get, phux_client_queue_attach, phux_client_queue_close_resource,
    phux_client_queue_hello, phux_client_rename_session, phux_client_session_create_info,
    phux_client_session_get, phux_client_session_rename_info, terminal_id_out,
};
use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{ActivationId, AuthorityId, Revision, ServerInstance};
use phux_protocol::wire::frame::{
    ActivationInfo, ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, ArrangementOwner,
    ArrangementValue, Command, CommandResult, CommandValue, EntryRef, EntryTarget, FrameKind,
    OrganizationPage, RecordRevision, SessionRecord,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};
use phux_protocol::{BootstrapProfile, SessionId, WindowId};

const ATTACH: u32 = 1;

fn authority() -> AuthorityId {
    AuthorityId::new([1; 16]).unwrap()
}

fn work() -> SessionKey {
    SessionKey::new([2; 16]).unwrap()
}

fn inbox() -> SessionKey {
    SessionKey::new([3; 16]).unwrap()
}

fn shell() -> ResourceKey {
    ResourceKey::new([4; 16]).unwrap()
}

fn finished() -> ResourceKey {
    ResourceKey::new([5; 16]).unwrap()
}

fn layout() -> ArrangementKey {
    ArrangementKey::new([6; 16]).unwrap()
}

fn instance() -> ServerInstance {
    ServerInstance::new([9; 16])
}

struct Harness(Box<PhuxClient>);

impl Harness {
    fn ptr(&mut self) -> *mut PhuxClient {
        &raw mut *self.0
    }

    fn client(&self) -> &Client {
        &self.0.inner
    }

    /// Decode and clear every queued outgoing frame.
    fn take_outgoing(&mut self) -> Vec<FrameKind> {
        std::mem::take(&mut self.0.inner.outgoing)
            .into_iter()
            .map(|bytes| FrameKind::decode(&bytes).unwrap().0)
            .collect()
    }

    fn feed(&mut self, frame: &FrameKind) {
        assert_eq!(feed(self.ptr(), frame), PhuxClientResult::Ok);
    }

    /// The request id of the one queued command `pick` accepts.
    fn command(&mut self, pick: impl Fn(&Command) -> bool) -> (u32, Command) {
        let frames = self.take_outgoing();
        frames
            .into_iter()
            .find_map(|frame| match frame {
                FrameKind::Command {
                    request_id,
                    command,
                } if pick(&command) => Some((request_id, command)),
                _ => None,
            })
            .expect("the command was queued")
    }

    fn answer(&mut self, request_id: u32, result: CommandResult) {
        self.feed(&FrameKind::CommandResult { request_id, result });
    }

    fn results(&self) -> Vec<(PhuxOrgResult, Vec<PhuxOrgTarget>)> {
        // SAFETY: the harness owns the live client; outputs are initialized.
        unsafe {
            let count = phux_client_org_result_count(&raw const *self.0);
            (0..count)
                .map(|index| {
                    let mut result = result_out();
                    assert_eq!(
                        phux_client_org_result_get(&raw const *self.0, index, &raw mut result),
                        PhuxClientResult::Ok
                    );
                    let targets = (0..result.target_count as usize)
                        .map(|target| {
                            let mut out = target_out();
                            assert_eq!(
                                phux_client_org_target_get(
                                    &raw const *self.0,
                                    index,
                                    target,
                                    &raw mut out
                                ),
                                PhuxClientResult::Ok
                            );
                            out
                        })
                        .collect();
                    (result, targets)
                })
                .collect()
        }
    }
}

fn result_out() -> PhuxOrgResult {
    PhuxOrgResult {
        size: mem::size_of::<PhuxOrgResult>(),
        version: crate::c::ABI_VERSION,
        request_id: 0,
        kind: 0,
        status: 0,
        refusal: 0,
        undoable: false,
        revision: 0,
        conflict_revision: 0,
        target_count: 0,
        message: PhuxBytes::default(),
    }
}

fn target_out() -> PhuxOrgTarget {
    PhuxOrgTarget {
        size: mem::size_of::<PhuxOrgTarget>(),
        version: crate::c::ABI_VERSION,
        key: [0; 16],
        is_session: false,
        status: 0,
        refusal: 0,
        message: PhuxBytes::default(),
    }
}

fn running(key: ResourceKey, session: SessionKey, live: u32) -> ResourceRecord {
    ResourceRecord::new(key, session, Revision::new(3))
        .with_name(Some("shell".to_owned()))
        .with_activation(Some(ActivationInfo::new(
            ActivationId::new(1),
            instance(),
            ExecState::Running,
        )))
        .with_live(Some(ResourceId::local(live)))
}

fn page(arrangement: Option<ArrangementKey>) -> OrganizationPage {
    let retained = ResourceRecord::new(finished(), work(), Revision::new(7))
        .with_on_exit(ExitPolicy::Retain)
        .with_activation(Some(
            ActivationInfo::new(ActivationId::new(2), instance(), ExecState::Completed)
                .with_outcome(Some(Outcome::Exited(3))),
        ));
    OrganizationPage::new(authority(), Revision::new(10), 1)
        .with_inbox(Some(inbox()))
        .with_sessions(vec![
            SessionRecord::new(work(), "work", Revision::new(5))
                .with_members(2)
                .with_arrangement(arrangement)
                .with_live(Some(SessionId::new(1))),
            SessionRecord::new(inbox(), "inbox", Revision::new(1))
                .with_awaiting_organization(true)
                .with_live(Some(SessionId::new(2))),
        ])
        .with_resources(vec![running(shell(), work(), 1), retained])
}

fn snapshot() -> SessionSnapshot {
    let window = WindowId::new(1);
    SessionSnapshot::new(SessionId::new(1), window, ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work")
                .with_key(Some(work()))
                .with_lifetime(Some(SessionLifetime::Persistent)),
            SessionInfo::new(SessionId::new(2), "inbox")
                .with_key(Some(inbox()))
                .with_lifetime(Some(SessionLifetime::Persistent)),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), window, 80, 24)
                .with_session(Some(SessionId::new(1))),
        ])
}

/// Negotiate with `ORGANIZATION` and answer the replica's seed.
fn negotiated(arrangement: Option<ArrangementKey>) -> Harness {
    let mut h = Harness(
        // SAFETY: the fixture returns a uniquely owned heap client.
        unsafe { Box::from_raw(new_client()) },
    );
    // SAFETY: the harness owns the live client; the literal is readable.
    assert_eq!(
        unsafe { phux_client_queue_hello(h.ptr(), bytes_out(b"test")) },
        PhuxClientResult::Ok
    );
    let caps = test_support::caps(&[])
        .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]));
    let _ = h.take_outgoing();
    h.feed(&hello_ok(caps, BootstrapProfile::SynthesizedVtRaw));
    let (seed, _) = h.command(|command| matches!(command, Command::GetOrganization { .. }));
    h.answer(
        seed,
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page(arrangement)))),
    );
    h
}

/// `negotiated`, then attached to session 1 with terminal 1 bootstrapped.
fn attached(arrangement: Option<ArrangementKey>) -> Harness {
    let mut h = negotiated(arrangement);
    let options = PhuxAttachOptions {
        size: mem::size_of::<PhuxAttachOptions>(),
        version: crate::c::ABI_VERSION,
        attach_id: ATTACH,
        target_kind: 0,
        session_id: 0,
        name: PhuxBytes::default(),
        cols: 80,
        rows: 24,
        has_pixel_size: false,
        pixel_width: 0,
        pixel_height: 0,
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    // SAFETY: the harness owns the live client; options outlive the call.
    assert_eq!(
        unsafe { phux_client_queue_attach(h.ptr(), &raw const options) },
        PhuxClientResult::Ok
    );
    let _ = h.take_outgoing();
    h.feed(&attached_frame(ATTACH, snapshot()));
    let generation = h.client().control().connection_epoch();
    feed_bootstrap(
        h.ptr(),
        &ResourceId::local(1),
        (1, generation),
        (80, 24),
        b"",
    );
    h.feed(&FrameKind::AttachReady { attach_id: ATTACH });
    h
}

fn org_info(h: &Harness) -> PhuxOrganizationInfo {
    let mut out = PhuxOrganizationInfo {
        size: mem::size_of::<PhuxOrganizationInfo>(),
        version: crate::c::ABI_VERSION,
        supported: false,
        state: 0,
        authority: [0; 16],
        inbox: [0; 16],
        revision: 0,
        record_count: 0,
        message: PhuxBytes::default(),
    };
    // SAFETY: the harness owns the live client; the output is initialized.
    assert_eq!(
        unsafe { phux_client_organization_info(&raw const *h.0, &raw mut out) },
        PhuxClientResult::Ok
    );
    out
}

fn session_record(h: &Harness, index: usize) -> PhuxSessionRecord {
    let mut out = PhuxSessionRecord {
        size: mem::size_of::<PhuxSessionRecord>(),
        version: crate::c::ABI_VERSION,
        session_id: 0,
        key: [0; 16],
        arrangement_key: [0; 16],
        revision: 0,
        created_at_unix_secs: 0,
        lifetime: 0,
        flags: 0,
        member_count: 0,
        running_count: 0,
        completed_count: 0,
        lost_count: 0,
        attached_client_count: 0,
        name: PhuxBytes::default(),
        key_text: PhuxBytes::default(),
    };
    // SAFETY: the harness owns the live client; the output is initialized.
    assert_eq!(
        unsafe { phux_client_session_record_get(&raw const *h.0, index, &raw mut out) },
        PhuxClientResult::Ok
    );
    out
}

fn span(bytes: PhuxBytes) -> Vec<u8> {
    if bytes.len == 0 {
        return Vec::new();
    }
    // SAFETY: a borrowed span the client keeps alive until its next mutation.
    unsafe { std::slice::from_raw_parts(bytes.data, bytes.len) }.to_vec()
}

#[allow(
    clippy::cognitive_complexity,
    reason = "one walk over every projected field"
)]
#[test]
fn negotiation_seeds_the_replica_and_records_reach_the_abi() {
    let h = attached(None);
    let info = org_info(&h);
    assert!(info.supported);
    assert_eq!(info.state, 2, "LIVE");
    assert_eq!(info.authority, *authority().as_bytes());
    assert_eq!(info.inbox, *inbox().as_bytes());
    assert_eq!(info.record_count, 2);

    let work_record = session_record(&h, 0);
    assert_eq!(work_record.session_id, 1);
    assert_eq!(work_record.key, *work().as_bytes());
    assert_eq!(span(work_record.key_text), work().to_string().into_bytes());
    assert_eq!(work_record.lifetime, LIFETIME_PERSISTENT);
    assert_eq!(work_record.revision, 5);
    assert_eq!(work_record.member_count, 2);
    assert_eq!(
        (
            work_record.running_count,
            work_record.completed_count,
            work_record.lost_count
        ),
        (1, 1, 0)
    );
    assert_ne!(work_record.flags & PHUX_SESSION_RECORD_ORGANIZED, 0);
    assert_ne!(work_record.flags & PHUX_SESSION_RECORD_FOCUSED, 0);
    assert_eq!(work_record.flags & PHUX_SESSION_RECORD_INBOX, 0);
    let inbox_record = session_record(&h, 1);
    assert_ne!(inbox_record.flags & PHUX_SESSION_RECORD_INBOX, 0);
    assert_ne!(
        inbox_record.flags & PHUX_SESSION_RECORD_AWAITING_ORGANIZATION,
        0
    );
    assert_ne!(inbox_record.flags & PHUX_SESSION_RECORD_EMPTY, 0);

    // The frozen session summary still reads.
    let mut summary = PhuxSessionInfo::default();
    // SAFETY: the harness owns the live client; the output is writable.
    assert_eq!(
        unsafe { phux_client_session_get(&raw const *h.0, 0, &raw mut summary) },
        PhuxClientResult::Ok
    );
    assert_eq!(summary.session_id, 1);

    // Records: one live, one completed and retained without a handle.
    let records: Vec<PhuxResourceRecord> = (0..2)
        .map(|index| {
            let mut out = PhuxResourceRecord {
                size: mem::size_of::<PhuxResourceRecord>(),
                version: crate::c::ABI_VERSION,
                kind: 0,
                key: [0; 16],
                session_key: [0; 16],
                revision: 0,
                activation_id: 0,
                exec_state: 0,
                outcome_kind: 0,
                outcome_code: 0,
                on_exit: 0,
                live: false,
                live_id: PhuxResourceId::default(),
                session_id: 0,
                name: PhuxBytes::default(),
                key_text: PhuxBytes::default(),
            };
            // SAFETY: the harness owns the live client; the output is initialized.
            assert_eq!(
                unsafe { phux_client_record_get(&raw const *h.0, index, &raw mut out) },
                PhuxClientResult::Ok
            );
            out
        })
        .collect();
    assert_eq!(records[0].key, *shell().as_bytes());
    assert!(records[0].live);
    assert_eq!(records[0].live_id.id, 1);
    assert_eq!(records[0].exec_state, 2, "RUNNING");
    assert_eq!(records[0].session_id, 1);
    assert_eq!(span(records[0].name), b"shell");
    assert!(!records[1].live);
    assert_eq!(records[1].exec_state, 4, "COMPLETED");
    assert_eq!((records[1].outcome_kind, records[1].outcome_code), (1, 3));
    assert_eq!(records[1].on_exit, 1, "RETAIN");

    // The live resource summary carries its key, session, and dims.
    let resource = &h.client().resources[0];
    assert_eq!(resource.key, *shell().as_bytes());
    assert_eq!(
        (resource.session_id, resource.cols, resource.rows),
        (1, 80, 24)
    );
}

#[test]
fn without_organization_nothing_is_seeded_and_operations_refuse() {
    let client = test_support::negotiated_client(&[]);
    // SAFETY: the fixture returns a uniquely owned heap client.
    let mut h = Harness(unsafe { Box::from_raw(client) });
    assert!(!org_info(&h).supported);
    // SAFETY: the harness owns the live client; the keys are 16 readable bytes.
    assert_eq!(
        unsafe {
            phux_client_org_move(
                h.ptr(),
                1,
                shell().as_bytes().as_ptr(),
                work().as_bytes().as_ptr(),
            )
        },
        PhuxClientResult::InvalidState
    );
    assert!(h.take_outgoing().is_empty());
    assert_eq!(h.results().len(), 0);
}

#[test]
fn a_move_commits_with_a_receipt_and_undoes_by_it() {
    let mut h = attached(None);
    let _ = h.take_outgoing();
    // SAFETY: the harness owns the live client; the keys are 16 readable bytes.
    assert_eq!(
        unsafe {
            phux_client_org_move(
                h.ptr(),
                5,
                shell().as_bytes().as_ptr(),
                inbox().as_bytes().as_ptr(),
            )
        },
        PhuxClientResult::Ok
    );
    let (id, command) = h.command(|command| matches!(command, Command::ResourceMove { .. }));
    let Command::ResourceMove {
        operation,
        resource,
        from,
        from_expected,
        to,
        to_expected,
    } = command
    else {
        unreachable!()
    };
    assert_eq!((resource, from, to), (shell(), work(), inbox()));
    assert_eq!((from_expected.get(), to_expected.get()), (5, 1));
    let (result, targets) = h.results().remove(0);
    assert_eq!((result.request_id, result.kind, result.status), (5, 1, 1));
    assert_eq!(targets[0].status, 1, "pending");
    let receipt = Committed::new(operation, Revision::new(6)).with_revisions(vec![
        RecordRevision {
            key: *work().as_bytes(),
            revision: Revision::new(6),
        },
        RecordRevision {
            key: *inbox().as_bytes(),
            revision: Revision::new(2),
        },
    ]);
    h.answer(
        id,
        CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))),
    );
    let (result, targets) = h.results().remove(0);
    assert_eq!(result.status, 2, "COMMITTED");
    assert!(result.undoable);
    assert_eq!(result.revision, 6);
    assert_eq!(targets[0].key, *shell().as_bytes());
    assert_eq!(targets[0].status, 2, "DONE");

    // SAFETY: the harness owns the live client.
    assert_eq!(
        unsafe { phux_client_org_undo(h.ptr(), 6, 5) },
        PhuxClientResult::Ok
    );
    let (_, command) = h.command(|command| matches!(command, Command::OrganizationUndo { .. }));
    let Command::OrganizationUndo { undo, expected, .. } = command else {
        unreachable!()
    };
    assert_eq!(undo, operation);
    assert_eq!(expected.len(), 2);
    // A pending result cannot be released; a settled one can.
    // SAFETY: the harness owns the live client.
    unsafe {
        assert_eq!(
            phux_client_org_result_release(h.ptr(), 6),
            PhuxClientResult::InvalidState
        );
        assert_eq!(
            phux_client_org_result_release(h.ptr(), 5),
            PhuxClientResult::Ok
        );
    }
    assert_eq!(h.results().len(), 1);
}

#[test]
fn a_refused_delete_reports_its_refusal_per_target() {
    let mut h = attached(None);
    let _ = h.take_outgoing();
    // SAFETY: the harness owns the live client; the key is 16 readable bytes.
    assert_eq!(
        unsafe { phux_client_org_delete_session(h.ptr(), 3, work().as_bytes().as_ptr()) },
        PhuxClientResult::Ok
    );
    let (id, _) = h.command(|command| matches!(command, Command::SessionDelete { .. }));
    h.answer(
        id,
        CommandResult::Refused(phux_protocol::wire::frame::Refusal::NotEmpty { members: 2 }),
    );
    let (result, targets) = h.results().remove(0);
    assert_eq!(result.status, 4, "REFUSED");
    assert_eq!(result.refusal, 5, "NOT_EMPTY");
    assert!(!result.undoable);
    assert!(targets[0].is_session);
    assert_eq!(targets[0].status, 4);
    // An all-zero key is refused before anything is queued.
    // SAFETY: the harness owns the live client; the key is 16 readable bytes.
    assert_eq!(
        unsafe { phux_client_org_delete_session(h.ptr(), 4, [0u8; 16].as_ptr()) },
        PhuxClientResult::InvalidArgument
    );
}

#[test]
fn killing_an_empty_session_deletes_it_and_reports_the_session() {
    let mut h = attached(None);
    let _ = h.take_outgoing();
    // SAFETY: the harness owns the live client; the key is 16 readable bytes.
    assert_eq!(
        unsafe { phux_client_kill_session(h.ptr(), 9, inbox().as_bytes().as_ptr()) },
        PhuxClientResult::Ok
    );
    let (result, _) = h.results().remove(0);
    assert_eq!((result.kind, result.status), (10, 1));
    let (id, command) = h.command(|command| matches!(command, Command::SessionDelete { .. }));
    let Command::SessionDelete { operation, .. } = command else {
        unreachable!()
    };
    h.answer(
        id,
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            operation,
            Revision::new(2),
        )))),
    );
    let (result, targets) = h.results().remove(0);
    assert_eq!(result.status, 2, "COMMITTED");
    assert_eq!(targets.len(), 1);
    assert!(targets[0].is_session);
    assert_eq!(targets[0].key, *inbox().as_bytes());
    assert_eq!(targets[0].status, 2, "DONE");
}

#[test]
fn typed_create_binds_the_committed_key_to_its_live_session() {
    let mut h = attached(None);
    let _ = h.take_outgoing();
    // SAFETY: the harness owns the live client; the name is readable.
    assert_eq!(
        unsafe { phux_client_create_session(h.ptr(), 4, bytes_out(b"notes"), LIFETIME_PERSISTENT) },
        PhuxClientResult::Ok
    );
    let (id, command) = h.command(|command| matches!(command, Command::SessionCreate { .. }));
    let Command::SessionCreate {
        operation,
        name,
        lifetime,
        launch,
        ..
    } = command
    else {
        unreachable!()
    };
    assert_eq!(name, "notes");
    assert_eq!(lifetime, SessionLifetime::Persistent);
    assert!(
        launch.is_none(),
        "a persistent session without a directory is empty"
    );
    let created = SessionKey::new([8; 16]).unwrap();
    h.answer(
        id,
        CommandResult::OkWith(CommandValue::Committed(Box::new(
            Committed::new(operation, Revision::new(1)).with_session(Some(created)),
        ))),
    );
    let (read, _) = h.command(|command| matches!(command, Command::GetState { .. }));
    let mut state = snapshot();
    state.sessions.push(
        SessionInfo::new(SessionId::new(7), "notes")
            .with_key(Some(created))
            .with_lifetime(Some(SessionLifetime::Persistent)),
    );
    h.answer(read, CommandResult::OkWith(CommandValue::State(state)));
    let mut info = PhuxSessionCreateInfo {
        size: mem::size_of::<PhuxSessionCreateInfo>(),
        version: crate::c::ABI_VERSION,
        request_id: 0,
        status: 0,
        session_id: 0,
        message: PhuxBytes::default(),
    };
    // SAFETY: the harness owns the live client; the output is initialized.
    assert_eq!(
        unsafe { phux_client_session_create_info(&raw const *h.0, 4, &raw mut info) },
        PhuxClientResult::Ok
    );
    assert_eq!((info.status, info.session_id), (2, 7));
}

#[test]
fn an_ephemeral_create_launches_its_first_shell() {
    let mut h = attached(None);
    let _ = h.take_outgoing();
    // SAFETY: the harness owns the live client; the name is readable.
    assert_eq!(
        unsafe {
            phux_client_create_session(h.ptr(), 4, bytes_out(b"scratch"), LIFETIME_EPHEMERAL)
        },
        PhuxClientResult::Ok
    );
    let (_, command) = h.command(|command| matches!(command, Command::SessionCreate { .. }));
    let Command::SessionCreate {
        lifetime, launch, ..
    } = command
    else {
        unreachable!()
    };
    assert_eq!(lifetime, SessionLifetime::Ephemeral);
    assert!(launch.is_some());
}

#[test]
fn typed_rename_settles_on_the_commit() {
    let mut h = attached(None);
    let _ = h.take_outgoing();
    // SAFETY: the harness owns the live client; both names are readable.
    assert_eq!(
        unsafe { phux_client_rename_session(h.ptr(), 2, bytes_out(b"work"), bytes_out(b"desk")) },
        PhuxClientResult::Ok
    );
    let (id, command) = h.command(|command| matches!(command, Command::SessionRename { .. }));
    let Command::SessionRename {
        operation,
        session,
        name,
    } = command
    else {
        unreachable!()
    };
    assert_eq!((session, name.as_str()), (work(), "desk"));
    h.answer(
        id,
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            operation,
            Revision::new(6),
        )))),
    );
    let mut info = PhuxSessionRenameInfo {
        size: mem::size_of::<PhuxSessionRenameInfo>(),
        version: crate::c::ABI_VERSION,
        request_id: 0,
        status: 0,
        session_id: 0,
        sessions_revision: 0,
        message: PhuxBytes::default(),
    };
    // SAFETY: the harness owns the live client; the output is initialized.
    assert_eq!(
        unsafe { phux_client_session_rename_info(&raw const *h.0, &raw mut info) },
        PhuxClientResult::Ok
    );
    assert_eq!(info.status, 2, "RENAMED");
    assert_eq!(h.client().sessions[0].name, b"desk");
}

#[test]
fn close_terminates_and_completes_on_resource_closed() {
    let mut h = attached(None);
    let _ = h.take_outgoing();
    let terminal = terminal_id_out(&ResourceId::local(1));
    // SAFETY: the harness owns the live client; the id is readable.
    assert_eq!(
        unsafe { phux_client_queue_close_resource(h.ptr(), 3, &raw const terminal) },
        PhuxClientResult::Ok
    );
    let (id, command) = h.command(|command| matches!(command, Command::ResourceTerminate { .. }));
    assert_eq!(id, 3);
    let Command::ResourceTerminate { target, .. } = command else {
        unreachable!()
    };
    assert_eq!(target.resource, ResourceId::local(1));
    assert_eq!(target.instance, instance());
    h.answer(
        id,
        CommandResult::Accepted {
            operation: phux_protocol::ids::OperationId::new([7; 16]).unwrap(),
        },
    );
    // SAFETY: the harness owns the live client.
    assert_eq!(unsafe { phux_client_operation_count(&raw const *h.0) }, 0);
    h.feed(&FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(1),
        exit_status: Some(0),
        signal: None,
        reason: phux_protocol::wire::frame::CloseReason::Killed,
    });
    let mut out = PhuxOperationResult {
        size: mem::size_of::<PhuxOperationResult>(),
        version: crate::c::ABI_VERSION,
        request_id: 0,
        kind: 0,
        status: 0,
        error_domain: 0,
        error_code: 0,
        terminal_id: PhuxResourceId::default(),
        message: PhuxBytes::default(),
    };
    // SAFETY: the harness owns the live client; the output is initialized.
    assert_eq!(
        unsafe { phux_client_operation_get(&raw const *h.0, 0, &raw mut out) },
        PhuxClientResult::Ok
    );
    assert_eq!((out.request_id, out.kind, out.status), (3, 5, 1));
}

fn stored(revision: u64, name: &str) -> ArrangementValue {
    ArrangementValue::new(
        layout(),
        ArrangementOwner::Session(work()),
        Revision::new(revision),
        WireDoc {
            groups: vec![ArrangementGroup {
                id: [0x11; 16],
                name: Some(name.to_owned()),
                tree: ArrangementNode::Leaf(EntryRef {
                    entry: [0x22; 16],
                    target: EntryTarget::Member(shell()),
                }),
            }],
        },
    )
}

fn arrangement_info(h: &Harness) -> PhuxArrangementInfo {
    let mut info = PhuxArrangementInfo::default();
    // SAFETY: the harness owns the live client; the output is initialized.
    assert_eq!(
        unsafe { phux_client_arrangement_info(&raw const *h.0, &raw mut info) },
        PhuxClientResult::Ok
    );
    info
}

/// Answer the stored-arrangement read and the registry read in flight.
fn answer_arrangement_read(h: &mut Harness, value: &ArrangementValue) {
    let frames = h.take_outgoing();
    for frame in frames {
        match frame {
            FrameKind::Command {
                request_id,
                command: Command::ArrangementGet { arrangement },
            } => {
                assert_eq!(arrangement, layout());
                h.answer(
                    request_id,
                    CommandResult::OkWith(CommandValue::Arrangement(Box::new(value.clone()))),
                );
            }
            FrameKind::Command {
                request_id,
                command: Command::GetState { .. },
            } => h.answer(
                request_id,
                CommandResult::OkWith(CommandValue::State(snapshot())),
            ),
            _ => {}
        }
    }
}

#[test]
fn the_stored_arrangement_is_read_and_replaced_by_compare_and_swap() {
    let mut h = attached(Some(layout()));
    answer_arrangement_read(&mut h, &stored(4, "edit"));
    let info = arrangement_info(&h);
    assert_eq!(info.state, 2, "AUTHORITATIVE");
    assert_eq!(info.backend, 2, "ORGANIZATION");
    assert_eq!(info.server_revision, 4);
    assert_eq!(info.arrangement_key, *layout().as_bytes());
    assert_eq!(info.session_key, *work().as_bytes());
    assert_eq!(info.group_count, 1);
    assert_eq!(info.unarranged_count, 0);
    let group = h.client().arrangement.topology.groups[0].id;
    assert_eq!(group, [0x11; 16]);

    // Rename the group: a replace at revision 4, refused as a conflict.
    let rename = PhuxArrangementMutation {
        request_id: 7,
        expected_revision: info.revision,
        session_id: 1,
        kind: 6,
        group_id: group,
        name: bytes_out(b"code"),
        ..PhuxArrangementMutation::default()
    };
    // SAFETY: the harness owns the live client; the mutation and its spans are readable.
    assert_eq!(
        unsafe { phux_client_arrangement_mutate(h.ptr(), &raw const rename) },
        PhuxClientResult::Ok
    );
    let frames = h.take_outgoing();
    let replace = frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command:
                    Command::ArrangementReplace {
                        expected, document, ..
                    },
            } => Some((*request_id, *expected, document.clone())),
            _ => None,
        })
        .expect("ARRANGEMENT_REPLACE");
    assert_eq!(replace.1.get(), 4, "compare-and-swap at the read revision");
    assert_eq!(replace.2.groups[0].name.as_deref(), Some("code"));
    let ArrangementNode::Leaf(entry) = &replace.2.groups[0].tree else {
        panic!("one leaf")
    };
    assert_eq!(
        entry.entry, [0x22; 16],
        "entry identity survives the round trip"
    );
    h.answer(
        replace.0,
        CommandResult::Refused(phux_protocol::wire::frame::Refusal::Conflict {
            current: Revision::new(5),
        }),
    );
    for frame in frames {
        match frame {
            FrameKind::Command {
                request_id,
                command: Command::ArrangementGet { .. },
            } => h.answer(
                request_id,
                CommandResult::OkWith(CommandValue::Arrangement(Box::new(stored(5, "other")))),
            ),
            FrameKind::Command {
                request_id,
                command: Command::GetState { .. },
            } => h.answer(
                request_id,
                CommandResult::OkWith(CommandValue::State(snapshot())),
            ),
            _ => {}
        }
    }
    let info = arrangement_info(&h);
    assert_eq!(info.status, 3, "refused: the other writer won");
    assert_eq!(info.server_revision, 5);
    assert!(String::from_utf8_lossy(&span(info.message)).contains("revision 5"));
    assert_eq!(h.client().arrangement.topology.groups[0].name, "other");
}

#[test]
fn record_layouts_are_pinned() {
    assert_eq!(mem::size_of::<PhuxSessionRecord>(), 128);
    assert_eq!(mem::offset_of!(PhuxSessionRecord, key), 16);
    assert_eq!(mem::offset_of!(PhuxSessionRecord, revision), 48);
    assert_eq!(mem::offset_of!(PhuxSessionRecord, name), 96);
    assert_eq!(mem::offset_of!(PhuxSessionRecord, key_text), 112);
    assert_eq!(mem::size_of::<PhuxResourceRecord>(), 152);
    assert_eq!(mem::offset_of!(PhuxResourceRecord, live_id), 88);
    assert_eq!(mem::offset_of!(PhuxResourceRecord, key_text), 136);
    assert_eq!(mem::size_of::<PhuxOrganizationInfo>(), 88);
    assert_eq!(mem::size_of::<PhuxOrgResult>(), 72);
    assert_eq!(mem::size_of::<PhuxOrgTarget>(), 56);
    assert_eq!(mem::size_of::<crate::c::PhuxResourceInfo>(), 128);
}
