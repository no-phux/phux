use super::*;
use crate::attach::connection::Connection;
use crate::testkit::ScriptSpec;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::{
    Command, FrameKind, Refusal, SESSION_CREATE_KEY, SessionLifetime,
};
use std::collections::BTreeMap;

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

fn org_spec(fixture: crate::testkit::OrgFixture) -> ScriptSpec {
    ScriptSpec::new().organization(fixture)
}

/// A rename is one `SESSION_RENAME` naming the
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
    let unknown = rename_checked(&mut conn, "gone", "x", &mut notices)
        .await
        .expect_err("unknown session");
    assert_eq!(unknown.reason(), "no such session");
    rename_checked(&mut conn, "play", "play", &mut notices)
        .await
        .expect("already named");
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

/// A create is one `SESSION_CREATE` carrying the
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
