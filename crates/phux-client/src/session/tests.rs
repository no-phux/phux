use super::*;
use crate::testkit::ScriptSpec;
use phux_protocol::ids::{IdempotencyKey, WindowId};
use phux_protocol::wire::frame::{
    Command, Refusal, SESSION_CREATE_RESULT_KEY_PREFIX, SESSION_NAME_KEY, SessionLifetime,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use std::collections::BTreeMap;

#[test]
fn empty_session_result_requires_name_nonce_and_empty() {
    let ok = br#"{"name":"parked","terminal_id":null,"request_token":"tok","empty":true}"#;
    assert!(empty_session_result_matches(ok, "parked", "tok"));
    let other_nonce = br#"{"name":"parked","request_token":"x","empty":true}"#;
    assert!(!empty_session_result_matches(other_nonce, "parked", "tok"));
    let seeded = br#"{"name":"parked","terminal_id":3,"request_token":"tok"}"#;
    assert!(!empty_session_result_matches(seeded, "parked", "tok"));
}

/// The correlated read-back must match both the requested name and the
/// nonce this request minted.
#[test]
fn created_session_from_result_requires_the_right_correlation() {
    let correlated = br#"{"name":"work","session_id":7,"terminal_id":3,"request_token":"tok"}"#;
    assert_eq!(
        created_session_from_result(correlated, "work", "tok"),
        Some(CreatedSession {
            terminal_id: 3,
            session_id: Some(SessionId::new(7)),
        })
    );
    assert_eq!(
        created_session_from_result(correlated, "work", "other"),
        None,
        "a mismatched nonce must not confirm the create"
    );
    let unnonced = br#"{"name":"work","terminal_id":5}"#;
    assert_eq!(
        created_session_from_result(unnonced, "work", "tok"),
        None,
        "a result without this request's nonce must not confirm it"
    );
}

#[test]
fn legacy_create_receipt_must_match_the_terminals_current_owner() {
    let owner = SessionId::new(7);
    let impostor = SessionId::new(8);
    let window = WindowId::new(9);
    let snapshot = SessionSnapshot::new(owner, window, ResourceId::local(11))
        .with_sessions(vec![
            SessionInfo::new(owner, "renamed"),
            SessionInfo::new(impostor, "requested"),
        ])
        .with_windows(vec![WindowInfo::new(window, owner, "1")])
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(11),
            window,
            80,
            24,
        )]);

    assert_eq!(
        created_session_id_from_snapshot(&snapshot, "renamed", 11),
        Some(owner)
    );
    assert_eq!(
        created_session_id_from_snapshot(&snapshot, "requested", 11),
        None,
        "a same-named session that does not own the receipt Terminal must not be seeded"
    );
}

fn snap(names: &[(&str, u32)]) -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1)).with_sessions(
        names
            .iter()
            .map(|(name, id)| SessionInfo::new(SessionId::new(*id), *name))
            .collect(),
    )
}

#[tokio::test]
async fn rename_checked_warns_on_a_partial_view_then_writes() {
    const NOTICE: &str = "satellite edge is unreachable: timed out";
    // Pre-check sees "work"; the barrier snapshot is the applied name.
    let spec = ScriptSpec::new()
        .states([snap(&[("work", 1)]), snap(&[("play", 1)])])
        .degradation_notice(NOTICE);
    let (socket, _dir, server) = scripted(spec);
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let mut notices = Vec::new();
    rename_checked(&mut conn, "work", "play", &mut notices)
        .await
        .expect("rename");
    drop(conn);
    let seen = server.await.expect("scripted server");
    assert_eq!(notices, [NOTICE]);
    assert!(
        seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SetMetadata { key, .. } if key == SESSION_NAME_KEY
        )),
        "expected the rename write; sent {seen:?}"
    );
    let gets = seen
        .iter()
        .filter(|frame| {
            matches!(
                frame,
                FrameKind::Command {
                    command: phux_protocol::wire::frame::Command::GetState { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        gets, 2,
        "pre-check and barrier each read GET_STATE; sent {seen:?}"
    );
}

#[tokio::test]
async fn rename_checked_refuses_without_writing() {
    let spec = ScriptSpec::new().state(snap(&[("work", 1), ("play", 2)]));
    let (socket, _dir, server) = scripted(spec);
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let mut notices = Vec::new();
    let unknown = rename_checked(&mut conn, "gone", "x", &mut notices)
        .await
        .expect_err("unknown session");
    assert_eq!(unknown.reason(), "no such session");
    let taken = rename_checked(&mut conn, "work", "play", &mut notices)
        .await
        .expect_err("taken name");
    assert_eq!(taken.reason(), "\"play\" already exists");
    rename_checked(&mut conn, "work", "work", &mut notices)
        .await
        .expect("already named");
    drop(conn);
    let seen = server.await.expect("scripted server");
    assert!(
        !seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SetMetadata { key, .. } if key == SESSION_NAME_KEY
        )),
        "a refusal or a no-op must not write; sent {seen:?}"
    );
}

#[tokio::test]
async fn rename_checked_barrier_reports_a_name_the_server_did_not_apply() {
    let spec = ScriptSpec::new().states([snap(&[("work", 1)]), snap(&[("work", 1)])]);
    let (socket, _dir, server) = scripted(spec);
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let mut notices = Vec::new();
    let err = rename_checked(&mut conn, "work", "notes", &mut notices)
        .await
        .expect_err("barrier still shows the old name");
    assert!(err.reason().contains("did not rename"), "{}", err.reason());
    drop(conn);
    let seen = server.await.expect("scripted server");
    assert!(
        seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SetMetadata { key, value, .. }
                if key == SESSION_NAME_KEY && value == b"work\0notes"
        )),
        "the write is sent before the barrier can refuse it; sent {seen:?}"
    );
}

#[tokio::test]
async fn rename_checked_barrier_reports_a_session_that_disappeared() {
    let spec = ScriptSpec::new().states([snap(&[("work", 1)]), snap(&[("other", 2)])]);
    let (socket, _dir, server) = scripted(spec);
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let mut notices = Vec::new();
    let err = rename_checked(&mut conn, "work", "notes", &mut notices)
        .await
        .expect_err("session gone");
    assert_eq!(err.reason(), "the session no longer exists");
    drop(conn);
    let _ = server.await.expect("scripted server");
}

fn scripted(
    spec: ScriptSpec,
) -> (
    std::path::PathBuf,
    tempfile::TempDir,
    tokio::task::JoinHandle<Vec<FrameKind>>,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, server) = crate::testkit::serve_one(dir.path(), spec);
    (socket, dir, server)
}

fn work_request(env: &BTreeMap<String, String>) -> CreateSessionRequest<'_> {
    CreateSessionRequest {
        idempotency_key: None,
        name: "work",
        command: None,
        cwd: None,
        env,
        agent_session: None,
        lifetime: SessionLifetime::Persistent,
        attrs: &[],
    }
}

#[tokio::test]
async fn create_session_reads_back_the_correlated_result_and_surfaces_degradation() {
    let spec = ScriptSpec::new()
        .metadata(|_scope, key| {
            key.strip_prefix(SESSION_CREATE_RESULT_KEY_PREFIX)
                .map(|token| {
                    serde_json::json!({
                        "name": "work",
                        "session_id": 7,
                        "terminal_id": 3,
                        "request_token": token,
                    })
                    .to_string()
                    .into_bytes()
                })
        })
        .degradation_notice("satellite edge is unreachable: timed out");
    let (socket, _dir, server) = scripted(spec);

    let mut conn = Connection::connect(&socket).await.expect("connect");
    let env = BTreeMap::new();
    let request = work_request(&env);
    let mut notices = Vec::new();
    let outcome = create_session(&mut conn, &request, &mut notices)
        .await
        .expect("scripted server answers");
    drop(conn);
    let seen = server.await.expect("scripted server task");

    assert_eq!(outcome, CreateOutcome::Created(3));
    assert_eq!(notices, ["satellite edge is unreachable: timed out"]);
    assert!(
        seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SetMetadata { request_id: 1, scope: Scope::Global, key, .. }
                if key == SESSION_CREATE_KEY
        )),
        "expected the SESSION_CREATE_KEY write at request_id 1; sent {seen:?}"
    );
    assert!(
        seen.iter().any(|frame| matches!(
            frame,
            FrameKind::GetMetadata { request_id: 2, scope: Scope::Global, key }
                if key.starts_with(SESSION_CREATE_RESULT_KEY_PREFIX)
        )),
        "expected the correlated read-back at request_id 2; sent {seen:?}"
    );
}

#[tokio::test]
async fn create_empty_session_refuses_before_writing_when_unsupported() {
    let (socket, _dir, server) = scripted(ScriptSpec::new());
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let mut notices = Vec::new();
    let outcome = create_empty_session(&mut conn, "parked", &mut notices)
        .await
        .expect("scripted server answers");
    drop(conn);
    let seen = server.await.expect("scripted server task");

    assert_eq!(outcome, CreateEmptyOutcome::Unsupported);
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
        "an unsupported server must never see the create write; sent {seen:?}"
    );
}

#[tokio::test]
async fn create_empty_session_creates_when_the_server_confirms_it() {
    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};

    let spec = ScriptSpec::new()
        .server_features(ServerFeatureSet::with(&[ServerFeature::KeepEmptySessions]))
        .metadata(|_scope, key| {
            key.strip_prefix(SESSION_CREATE_RESULT_KEY_PREFIX)
                .map(|token| {
                    serde_json::json!({
                        "name": "parked",
                        "terminal_id": null,
                        "request_token": token,
                        "empty": true,
                    })
                    .to_string()
                    .into_bytes()
                })
        });
    let (socket, _dir, server) = scripted(spec);
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let mut notices = Vec::new();
    let outcome = create_empty_session(&mut conn, "parked", &mut notices)
        .await
        .expect("scripted server answers");
    drop(conn);
    let _ = server.await.expect("scripted server task");

    assert_eq!(outcome, CreateEmptyOutcome::Created);
}

fn org_spec(fixture: crate::testkit::OrgFixture) -> ScriptSpec {
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .organization(fixture)
}

/// Under `ORGANIZATION` a rename is one `SESSION_RENAME` naming the
/// session's key, last writer wins; no metadata write.
#[tokio::test]
async fn rename_rides_one_session_rename() {
    let mut fixture = crate::testkit::OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    fixture.session("play", SessionLifetime::Persistent);
    let (socket, _dir, server) = scripted(org_spec(fixture));
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let mut notices = Vec::new();
    rename_checked(&mut conn, "work", "notes", &mut notices)
        .await
        .expect("rename");
    let taken = rename_checked(&mut conn, "notes", "play", &mut notices)
        .await
        .expect_err("taken");
    assert_eq!(taken.reason(), "\"play\" already exists");
    drop(conn);
    let seen = server.await.expect("scripted server");
    let renames: Vec<_> = seen
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::SessionRename { session, name, .. },
                ..
            } => Some((*session, name.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(renames, vec![(work, "notes".to_owned())]);
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. }))
    );
}

/// Under `ORGANIZATION` a create is one `SESSION_CREATE` carrying the
/// seed launch and the idempotency key as its operation id.
#[tokio::test]
async fn create_rides_session_create_with_the_seed_launch() {
    let (socket, _dir, server) = scripted(org_spec(crate::testkit::OrgFixture::new()));
    let mut conn = Connection::connect(&socket).await.expect("connect");
    let env = BTreeMap::from([("A".to_owned(), "1".to_owned())]);
    let key = IdempotencyKey::new([9; 16]).expect("non-zero");
    let mut request = work_request(&env);
    request.idempotency_key = Some(key);
    request.cwd = Some("/src");
    request.agent_session = Some(b"resume");
    let mut notices = Vec::new();
    let outcome = create_session(&mut conn, &request, &mut notices)
        .await
        .expect("create");
    assert!(matches!(outcome, CreateOutcome::Created(_)), "{outcome:?}");
    let again = create_session(&mut conn, &request, &mut notices)
        .await
        .expect("same-key resend");
    assert_eq!(again, outcome, "a same-id resend answers the stored result");
    let empty = create_empty_session(&mut conn, "parked", &mut notices)
        .await
        .expect("empty");
    assert_eq!(empty, CreateEmptyOutcome::Created);
    let taken = create_empty_session(&mut conn, "parked", &mut notices)
        .await
        .expect("answered");
    assert_eq!(taken, CreateEmptyOutcome::Refused(Refusal::NameTaken));
    drop(conn);
    let seen = server.await.expect("scripted server");
    let Some(FrameKind::Command {
        command:
            Command::SessionCreate {
                operation,
                launch: Some(launch),
                lifetime,
                ..
            },
        ..
    }) = seen.iter().find(|f| {
        matches!(
            f,
            FrameKind::Command {
                command: Command::SessionCreate { .. },
                ..
            }
        )
    })
    else {
        panic!("no SESSION_CREATE in {seen:?}");
    };
    assert_eq!(*operation, key);
    assert_eq!(*lifetime, SessionLifetime::Persistent);
    assert_eq!(launch.cwd.as_deref(), Some("/src"));
    assert_eq!(launch.env, vec![("A".to_owned(), "1".to_owned())]);
    assert_eq!(launch.restore.as_deref(), Some(&b"resume"[..]));
    assert!(
        !seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SetMetadata { key, .. } if key == SESSION_CREATE_KEY
        )),
        "the create is not the legacy metadata write"
    );
}
