use super::*;
use crate::c::client::SessionSummary;
use crate::c::test_support::{feed, negotiated_client, new_client};
use crate::c::types::ABI_VERSION;
use phux_protocol::wire::frame::{Command, ErrorCode};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use phux_protocol::{ResourceId, SessionId, WindowId};

/// Seeds the session catalogue a real attach or query would have delivered.
fn with_sessions(client: *mut PhuxClient, names: &[&str]) -> *mut PhuxClient {
    let sessions = names
        .iter()
        .zip(1..)
        .map(|(name, id)| SessionSummary {
            session_id: id,
            name: name.as_bytes().to_vec(),
            created_at_unix_secs: 0,
            window_count: 1,
            attached_client_count: 0,
            focused: id == 1,
            ..SessionSummary::default()
        })
        .collect();
    unsafe { (*client).inner.sessions = sessions };
    client
}

fn negotiated(names: &[&str]) -> *mut PhuxClient {
    with_sessions(negotiated_client(&[]), names)
}

/// A client that has queued HELLO and not yet heard `HELLO_OK`.
fn hello_pending() -> *mut PhuxClient {
    let client = new_client();
    assert_eq!(
        unsafe { crate::c::phux_client_queue_hello(client, span("test")) },
        PhuxClientResult::Ok
    );
    unsafe { (*client).inner.outgoing.clear() };
    client
}

fn span(text: &str) -> PhuxBytes {
    PhuxBytes {
        data: text.as_ptr(),
        len: text.len(),
    }
}

fn rename(client: *mut PhuxClient, id: u32, from: &str, to: &str) -> PhuxClientResult {
    unsafe { phux_client_rename_session(client, id, span(from), span(to)) }
}

fn info(client: *mut PhuxClient) -> (u32, u32, u64, String) {
    let mut out = PhuxSessionRenameInfo {
        size: mem::size_of::<PhuxSessionRenameInfo>(),
        version: ABI_VERSION,
        request_id: 0,
        status: u32::MAX,
        session_id: 0,
        sessions_revision: 0,
        message: PhuxBytes::default(),
    };
    assert_eq!(
        unsafe { phux_client_session_rename_info(client, &raw mut out) },
        PhuxClientResult::Ok
    );
    let message = unsafe { bytes_in(out.message.data, out.message.len) }.expect("span");
    (
        out.request_id,
        out.status,
        out.sessions_revision,
        String::from_utf8(message.to_vec()).expect("UTF-8"),
    )
}

fn sent(client: *mut PhuxClient) -> Vec<FrameKind> {
    let queued = std::mem::take(unsafe { &mut (*client).inner.outgoing });
    queued
        .iter()
        .map(|bytes| FrameKind::decode(bytes).expect("queued frame decodes").0)
        .collect()
}

fn names(client: *mut PhuxClient) -> Vec<String> {
    unsafe { &(*client).inner.sessions }
        .iter()
        .map(|s| String::from_utf8(s.name.clone()).expect("UTF-8"))
        .collect()
}

fn changed(value: &[u8]) -> FrameKind {
    FrameKind::MetadataChanged {
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
        value: Some(value.to_vec()),
        actor: None,
    }
}

fn state(request_id: u32, names: &[&str]) -> FrameKind {
    let sessions = names
        .iter()
        .zip(1..)
        .map(|(name, id)| SessionInfo::new(SessionId::new(id), *name))
        .collect();
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(CommandValue::State(
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                .with_sessions(sessions),
        )),
    }
}

fn barrier_of(frames: &[FrameKind]) -> u32 {
    frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetState { .. },
            } => Some(*request_id),
            _ => None,
        })
        .expect("a confirmation GET_STATE")
}

#[test]
fn a_rename_subscribes_writes_and_confirms_and_the_list_follows_the_broadcast() {
    let client = negotiated(&["build", "deploy"]);
    assert_eq!(rename(client, 5, "build", "ship"), PhuxClientResult::Ok);
    assert_eq!(info(client).1, STATUS_PENDING);
    let frames = sent(client);
    assert_eq!(frames.len(), 3, "subscribe, write, confirmation read");
    assert_eq!(
        frames[0],
        FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: SESSION_NAME_KEY.to_owned()
        }
    );
    assert_eq!(
        frames[1],
        FrameKind::SetMetadata {
            request_id: 5,
            scope: Scope::Global,
            key: SESSION_NAME_KEY.to_owned(),
            value: b"build\0ship".to_vec(),
        }
    );
    let barrier = barrier_of(&frames);
    assert!(barrier >= crate::c::arrangement::INTERNAL_START);
    assert!(!frames.iter().any(|f| matches!(f, FrameKind::Attach { .. })));

    // The broadcast renames the list in place and settles the rename.
    assert_eq!(feed(client, &changed(b"build\0ship")), PhuxClientResult::Ok);
    assert_eq!(names(client), ["ship", "deploy"]);
    let (request, status, revision, _) = info(client);
    assert_eq!((request, status, revision), (5, STATUS_RENAMED, 1));
    // The confirmation read is consumed here, not read as a stray reply.
    assert_eq!(
        feed(client, &state(barrier, &["ship", "deploy"])),
        PhuxClientResult::Ok
    );
    assert_eq!(info(client).1, STATUS_RENAMED);
    assert_eq!(info(client).2, 1, "nothing new to apply");

    // A second rename does not subscribe twice.
    assert_eq!(rename(client, 6, "ship", "sail"), PhuxClientResult::Ok);
    let again = sent(client);
    assert_eq!(again.len(), 2);
    assert!(matches!(
        again[0],
        FrameKind::SetMetadata { request_id: 6, .. }
    ));
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn a_duplicate_or_unknown_name_is_refused_with_a_reason_and_nothing_is_sent() {
    let client = negotiated(&["build", "deploy"]);
    assert_eq!(rename(client, 1, "build", "deploy"), PhuxClientResult::Ok);
    let (_, status, _, message) = info(client);
    assert_eq!(status, STATUS_REFUSED);
    assert_eq!(message, "\"deploy\" already exists");
    assert!(sent(client).is_empty());

    assert_eq!(rename(client, 2, "gone", "x"), PhuxClientResult::Ok);
    let (_, status, _, message) = info(client);
    assert_eq!(status, STATUS_REFUSED);
    assert_eq!(message, "no such session");
    assert!(sent(client).is_empty());

    // The same name is nothing to do.
    assert_eq!(rename(client, 3, "build", "build"), PhuxClientResult::Ok);
    assert_eq!(info(client).1, STATUS_RENAMED);
    assert!(sent(client).is_empty());
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn a_silent_server_refusal_is_read_from_the_confirmation_and_an_error_is_its_reason() {
    let client = negotiated(&["build", "deploy"]);
    assert_eq!(rename(client, 1, "build", "ship"), PhuxClientResult::Ok);
    let barrier = barrier_of(&sent(client));
    // Someone took "ship" first: no broadcast, the name is unchanged.
    assert_eq!(
        feed(client, &state(barrier, &["build", "deploy"])),
        PhuxClientResult::Ok
    );
    let (_, status, _, message) = info(client);
    assert_eq!(status, STATUS_REFUSED);
    assert!(message.contains("did not rename"), "{message}");
    assert_eq!(names(client), ["build", "deploy"]);

    // A correlated ERROR on the write names the server's reason.
    assert_eq!(rename(client, 2, "build", "ship"), PhuxClientResult::Ok);
    let barrier = barrier_of(&sent(client));
    let refused = FrameKind::Error {
        request_id: Some(2),
        code: ErrorCode::PermissionDenied,
        message: "not allowed".to_owned(),
    };
    assert_eq!(feed(client, &refused), PhuxClientResult::Ok);
    assert_eq!(info(client).1, STATUS_REFUSED);
    assert_eq!(info(client).3, "not allowed");
    assert_eq!(
        feed(client, &state(barrier, &["build", "deploy"])),
        PhuxClientResult::Ok,
        "the late confirmation is still consumed"
    );
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn another_clients_rename_moves_the_list_and_a_disconnect_makes_ours_unknown() {
    let client = negotiated(&["build", "deploy"]);
    assert_eq!(rename(client, 1, "build", "ship"), PhuxClientResult::Ok);
    let _ = sent(client);
    // Someone else renamed deploy meanwhile: the list follows, ours waits.
    assert_eq!(
        feed(client, &changed(b"deploy\0prod")),
        PhuxClientResult::Ok
    );
    assert_eq!(names(client), ["build", "prod"]);
    assert_eq!(info(client).1, STATUS_PENDING);
    // A malformed broadcast changes nothing and fails nothing.
    assert_eq!(
        feed(client, &changed(b"no separator")),
        PhuxClientResult::Ok
    );
    assert_eq!(
        unsafe { crate::c::phux_client_disconnect(client) },
        PhuxClientResult::Ok
    );
    assert_eq!(info(client).1, STATUS_UNKNOWN_OUTCOME);
    unsafe { crate::c::phux_client_free(client) };
}

fn hello_ok() -> FrameKind {
    crate::c::test_support::hello_ok(
        phux_protocol::caps::ServerCapabilities::new(),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    )
}

fn subscription() -> FrameKind {
    FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
    }
}

#[test]
fn a_follower_subscribes_right_after_hello_ok_and_hears_other_clients_renames() {
    let client = hello_pending();
    assert_eq!(
        unsafe { phux_client_follow_session_names(client) },
        PhuxClientResult::Ok
    );
    assert!(sent(client).is_empty(), "nothing before HELLO_OK");
    assert_eq!(feed(client, &hello_ok()), PhuxClientResult::Ok);
    // One read-only subscription, and never an attach.
    assert_eq!(sent(client), [subscription()]);
    // Populate the catalog on this negotiated connection, not before its
    // initial ConnectionOpened boundary retires connection-owned state.
    with_sessions(client, &["build", "deploy"]);

    // Another client's rename moves the list, though this one renamed
    // nothing; no rename of ours is pending or settled by it.
    assert_eq!(feed(client, &changed(b"build\0ship")), PhuxClientResult::Ok);
    assert_eq!(names(client), ["ship", "deploy"]);
    let (_, status, revision, _) = info(client);
    assert_eq!((status, revision), (STATUS_NONE, 1));

    // Following again, or renaming, does not subscribe a second time.
    assert_eq!(
        unsafe { phux_client_follow_session_names(client) },
        PhuxClientResult::Ok
    );
    assert!(sent(client).is_empty());
    assert_eq!(rename(client, 1, "ship", "sail"), PhuxClientResult::Ok);
    let frames = sent(client);
    assert_eq!(frames.len(), 2, "write and confirmation read only");
    assert!(!frames.contains(&subscription()));
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn following_on_a_negotiated_client_subscribes_at_once_and_not_after_detach() {
    let client = negotiated(&["build"]);
    assert_eq!(
        unsafe { phux_client_follow_session_names(client) },
        PhuxClientResult::Ok
    );
    assert_eq!(sent(client), [subscription()]);
    unsafe { crate::c::phux_client_free(client) };

    // A client that never follows subscribes only at its first rename.
    let lazy = hello_pending();
    assert_eq!(feed(lazy, &hello_ok()), PhuxClientResult::Ok);
    assert!(sent(lazy).is_empty());
    unsafe { crate::c::phux_client_free(lazy) };

    let ended = negotiated(&["build"]);
    let detached = FrameKind::Detached {
        reason: None,
        message: String::new(),
    };
    assert_eq!(feed(ended, &detached), PhuxClientResult::Ok);
    assert_eq!(
        unsafe { phux_client_follow_session_names(ended) },
        PhuxClientResult::InvalidState
    );
    assert!(sent(ended).is_empty());
    unsafe { crate::c::phux_client_free(ended) };
}

#[test]
fn a_rename_is_refused_outside_its_lifecycle_or_with_a_bad_name() {
    let fresh = with_sessions(new_client(), &["build"]);
    assert_eq!(
        rename(fresh, 1, "build", "x"),
        PhuxClientResult::InvalidState
    );
    unsafe { crate::c::phux_client_free(fresh) };
    let client = negotiated(&["build"]);
    assert_eq!(
        rename(client, 1, "build", ""),
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(
        rename(client, 1, "build", "a\0b"),
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(rename(client, 1, "build", "x"), PhuxClientResult::Ok);
    assert_eq!(
        rename(client, 2, "build", "y"),
        PhuxClientResult::InvalidState,
        "one rename at a time"
    );
    unsafe { crate::c::phux_client_free(client) };
}

/// The new name is judged by core's one rule: a name the server would not
/// store is an invalid argument, and one no selector could address is
/// refused with core's reason. Nothing is sent either way.
#[test]
fn a_new_name_is_judged_by_cores_session_name_rule() {
    use phux_client_core::rename::{MAX_SESSION_NAME_BYTES, SessionNameError};
    let client = negotiated(&["build"]);
    let too_long = "a".repeat(MAX_SESSION_NAME_BYTES + 1);
    for (id, bad) in (1..).zip([too_long.as_str(), "a\tb", "a\u{7f}b"]) {
        assert_eq!(
            rename(client, id, "build", bad),
            PhuxClientResult::InvalidArgument,
            "{bad:?}"
        );
    }
    assert_eq!(rename(client, 1, "build", "x:y"), PhuxClientResult::Ok);
    let (_, status, _, message) = info(client);
    assert_eq!(status, STATUS_REFUSED);
    assert_eq!(message, SessionNameError::Colon.to_string());
    assert!(sent(client).is_empty());

    let at_limit = "a".repeat(MAX_SESSION_NAME_BYTES);
    assert_eq!(rename(client, 2, "build", &at_limit), PhuxClientResult::Ok);
    assert_eq!(info(client).1, STATUS_PENDING);
    unsafe { crate::c::phux_client_free(client) };
}

/// A rename answered with an unreadable reply, or a newer server's error
/// code, does not say whether the session was renamed: unknown outcome,
/// never refused.
#[test]
fn an_unreadable_rename_answer_is_an_unknown_outcome() {
    let client = negotiated(&["build", "deploy"]);
    assert_eq!(rename(client, 1, "build", "ship"), PhuxClientResult::Ok);
    let _ = sent(client);
    let unreadable = FrameKind::Error {
        request_id: Some(1),
        code: ErrorCode::UNREADABLE_REPLY,
        message: "unreadable".to_owned(),
    };
    assert_eq!(feed(client, &unreadable), PhuxClientResult::Ok);
    assert_eq!(info(client).1, STATUS_UNKNOWN_OUTCOME);
    unsafe { crate::c::phux_client_free(client) };
}
