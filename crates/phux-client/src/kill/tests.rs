use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::frame::{ExecState, FrameKind, Refusal, Scope, SessionLifetime};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use super::*;
use crate::kill::selection::session_is_empty;
use crate::testkit::{OrgFixture, ScriptSpec};

const NOTICE: &str = "satellite build-box is unreachable: link is down";

/// A per-id `KILL_RESOURCES` outcome is `Killed` only when no id failed,
/// and a refusal names each id that did.
#[test]
fn a_merged_kill_outcome_names_every_id_that_was_not_killed() {
    let json = |doc: &str| CommandResult::OkWith(CommandValue::Json(doc.to_owned()));
    let all = r#"{"schema_version":1,"killed":["@1","devbox/@2"],"not_found":[],"failed":[]}"#;
    assert_eq!(KillOutcome::from_result(json(all)), KillOutcome::Killed);
    let partial = r#"{"schema_version":1,"killed":["@1"],"not_found":[],
            "failed":[{"id":"devbox/@2","code":107,"message":"link is down"}]}"#;
    assert_eq!(
        KillOutcome::from_result(json(partial)),
        KillOutcome::Partial(serde_json::from_str(partial).expect("partial document"))
    );
    assert!(matches!(
        KillOutcome::from_result(json("x")),
        KillOutcome::Unexpected(_)
    ));
}

/// The kill verbs send their frames, and an interleaved degradation
/// notice reaches the caller beside the outcome.
#[tokio::test]
async fn kill_verbs_send_the_expected_frames_and_keep_degradation() {
    let (_dir, mut conn, server) =
        crate::testkit::connect_one(ScriptSpec::new().degradation_notice(NOTICE)).await;
    let ids = vec![ResourceId::local(1), ResourceId::local(2)];
    let (outcome, degradation) = kill_resources(&mut conn, 1, ids.clone())
        .await
        .expect("scripted server");
    assert_eq!(outcome, KillOutcome::Killed);
    assert_eq!(degradation.notices(), [NOTICE.to_owned()]);
    let (outcome, _) = kill_resource(&mut conn, 2, ResourceId::local(3))
        .await
        .expect("scripted server");
    assert_eq!(outcome, KillOutcome::Killed);
    drop(conn);
    let seen = server.await.expect("scripted server task");

    assert!(seen.iter().any(|frame| matches!(
        frame,
        FrameKind::Command { command: Command::KillResources { ids: seen_ids, .. }, .. }
            if *seen_ids == ids
    )));
    assert!(killed_one(&seen, 3), "sent {seen:?}");
}

/// A keyed kill carries its key.
#[tokio::test]
async fn a_keyed_kill_carries_its_key() {
    let (_dir, mut conn, server) = crate::testkit::connect_one(ScriptSpec::new()).await;
    let key = IdempotencyKey::new([7; 16]).expect("non-zero");
    kill_resource_keyed(&mut conn, 1, ResourceId::local(3), key)
        .await
        .expect("scripted server");
    drop(conn);
    let seen = server.await.expect("scripted server task");
    assert!(seen.iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::KillResource {
                operation_id: Some(_),
                ..
            },
            ..
        }
    )));
}

fn pane_state() -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    SessionSnapshot::new(session, window, ResourceId::local(1))
        .with_sessions(vec![SessionInfo::new(session, "work").with_window_count(1)])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), window, 80, 24),
            ResourceInfo::new(ResourceId::local(2), window, 80, 24),
        ])
}

async fn run_selected(
    spec: ScriptSpec,
    target: &str,
) -> (Result<Selected, KillError>, Vec<String>, Vec<FrameKind>) {
    run_selected_through(spec.state(pane_state()), target).await
}

async fn run_selected_through(
    spec: ScriptSpec,
    target: &str,
) -> (Result<Selected, KillError>, Vec<String>, Vec<FrameKind>) {
    let (_dir, mut conn, server) = crate::testkit::connect_one(spec).await;
    let selector = crate::selector::parse(target).expect("selector");
    let mut notices = Vec::new();
    let result = selected(&mut conn, &selector, target, None, &mut notices).await;
    drop(conn);
    (result, notices, server.await.expect("scripted server task"))
}

fn killed_one(seen: &[FrameKind], id: u32) -> bool {
    seen.iter().any(|frame| {
        matches!(
            frame,
            FrameKind::Command { command: Command::KillResource { terminal_id, .. }, .. }
                if *terminal_id == ResourceId::local(id)
        )
    })
}

/// A pane target rides `KILL_RESOURCE`; a whole-session target rides
/// one `KILL_RESOURCES` for every pane in the session.
#[tokio::test]
async fn selected_kills_a_pane_or_a_whole_session() {
    let (result, notices, seen) = run_selected(ScriptSpec::new(), "@2").await;
    assert!(result.is_ok() && notices.is_empty(), "{result:?}");
    assert!(killed_one(&seen, 2), "sent {seen:?}");

    let (result, notices, seen) = run_selected(ScriptSpec::new(), "work").await;
    assert!(result.is_ok() && notices.is_empty(), "{result:?}");
    assert!(seen.iter().any(|frame| matches!(
        frame,
        FrameKind::Command { command: Command::KillResources { ids, .. }, .. }
            if ids.len() == 2
    )));
}

/// `%name` (ADR-0075) kills exactly the named pane, and every refusal is
/// a typed [`KillError::Agent`] with nothing killed — not the plain
/// "no such target" the set-valued path reported for a live name.
#[tokio::test]
async fn selected_resolves_percent_name_through_the_agent_resolver() {
    use crate::agent_meta::{AgentRecord, RESOURCE_AGENT_KEY};
    use crate::selector::AgentResolveError;
    fn named(records: &[(u32, &'static str, Option<&'static str>)]) -> ScriptSpec {
        records
            .iter()
            .fold(ScriptSpec::new(), |spec, (id, name, kind)| {
                let record = AgentRecord {
                    name: (*name).to_owned(),
                    kind: kind.map(str::to_owned),
                    ..AgentRecord::default()
                };
                spec.stored_metadata(
                    Scope::Resource(ResourceId::local(*id)),
                    RESOURCE_AGENT_KEY,
                    record.encode(),
                )
            })
    }
    let killed_any = |seen: &[FrameKind]| {
        seen.iter().any(|frame| {
            matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResource { .. } | Command::KillResources { .. },
                    ..
                }
            )
        })
    };

    let (result, _, seen) = run_selected(named(&[(2, "build", None)]), "%build").await;
    assert!(result.is_ok(), "{result:?}");
    assert!(killed_one(&seen, 2), "sent {seen:?}");

    for (spec, target, want) in [
        (named(&[(2, "build", None)]), "%ghost", "no_such_target"),
        (
            named(&[(1, "build", None), (2, "build", None)]),
            "%build",
            "selector_not_single",
        ),
    ] {
        let (result, _, seen) = run_selected(spec, target).await;
        let Err(KillError::Agent(err)) = &result else {
            panic!("{target}: expected an agent refusal, got {result:?}");
        };
        assert_eq!(err.code(), want, "{target}: {err}");
        assert!(
            !killed_any(&seen),
            "{target}: a refusal kills nothing: {seen:?}"
        );
    }

    let (result, _, seen) = run_selected(
        named(&[(2, "build", None)]).refuse_metadata(ErrorCode::PermissionDenied, "store offline"),
        "%build",
    )
    .await;
    assert!(
        matches!(
            result,
            Err(KillError::Agent(AgentResolveError::PartialIndex { .. }))
        ),
        "an unreadable index is partial (exit 3), not a miss: {result:?}"
    );
    assert!(!killed_any(&seen), "sent {seen:?}");
}

/// A miss against a complete view is absence; a miss against a partial
/// fleet is unresolved. A hit under degradation still kills and reports
/// the notices for the caller's warning.
#[tokio::test]
async fn selected_splits_a_complete_miss_from_a_partial_view() {
    let (result, notices, _) = run_selected(ScriptSpec::new(), "@9").await;
    assert!(
        matches!(result, Err(KillError::NoSuchTarget { ref target }) if target == "@9"),
        "{result:?}"
    );
    assert!(notices.is_empty());

    let degraded = || ScriptSpec::new().degradation_notice(NOTICE);
    let (result, notices, _) = run_selected(degraded(), "@9").await;
    assert!(
        matches!(
            result,
            Err(KillError::Unresolved { ref target, ref degradation })
                if target == "@9" && degradation.notices() == [NOTICE]
        ),
        "{result:?}"
    );
    assert!(notices.is_empty(), "a miss does not warn as a hit");

    let (result, notices, seen) = run_selected(degraded(), "@2").await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(notices, [NOTICE]);
    assert!(killed_one(&seen, 2), "a partial-view hit still kills");
}

fn org_spec(fixture: OrgFixture) -> ScriptSpec {
    ScriptSpec::new().organization(fixture)
}

fn commands(seen: &[FrameKind], pick: fn(&Command) -> bool) -> usize {
    seen.iter()
        .filter(|frame| matches!(frame, FrameKind::Command { command, .. } if pick(command)))
        .count()
}

/// `phux kill work`: one `SESSION_KILL`, whatever the
/// members are doing; no per-member traffic and no `KILL_RESOURCES`.
#[tokio::test]
async fn a_session_kill_is_one_command() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    fixture.shell(work);
    fixture.resource(work, Some(ExecState::Running));
    fixture.resource(work, Some(ExecState::Completed));
    let (result, _, seen) = run_selected_through(org_spec(fixture), "work").await;
    assert_eq!(result.expect("killed").killed, Some(3));
    assert_eq!(
        commands(&seen, |c| matches!(c, Command::SessionKill { .. })),
        1
    );
    assert_eq!(
        commands(&seen, |c| !matches!(
            c,
            Command::SessionKill { .. } | Command::GetState { .. }
        )),
        0,
        "{seen:?}"
    );
}

/// An empty session is deleted outright: no keep-empty write exists any
/// more.
#[tokio::test]
async fn an_empty_session_is_killed_by_the_same_command() {
    let mut fixture = OrgFixture::new();
    fixture.session("parked", SessionLifetime::Persistent);
    let (result, _, seen) = run_selected_through(org_spec(fixture), "parked").await;
    assert_eq!(result.expect("deleted").killed, Some(0));
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
        "{seen:?}"
    );
}

/// A refusal names the session and the reason.
#[tokio::test]
async fn a_refused_session_kill_reports_the_refusal() {
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    let fixture = fixture.refuse_next(
        "session_kill",
        Refusal::Unauthorized {
            verb: phux_protocol::kinds::Verb::Terminate,
        },
    );
    let (result, _, _) = run_selected_through(org_spec(fixture), "work").await;
    assert!(
        matches!(result, Err(KillError::Refused { ref label, .. }) if label == "session \"work\""),
        "{result:?}"
    );
}

/// Emptiness comes from the session facet when the server reports it,
/// never from the synthesized window count.
#[test]
fn emptiness_reads_the_session_facet() {
    let (busy, parked, agent) = (SessionId::new(1), SessionId::new(2), SessionId::new(3));
    let state = SessionSnapshot::new(busy, WindowId::new(10), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(busy, "busy").with_window_count(0),
            SessionInfo::new(parked, "parked").with_window_count(1),
            SessionInfo::new(agent, "agent").with_window_count(1),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(99), 80, 24)
                .with_session(Some(busy)),
            ResourceInfo::new(ResourceId::local(2), WindowId::new(99), 0, 0)
                .with_kind(phux_protocol::ids::ResourceKind::AgentSession)
                .with_session(Some(agent)),
        ]);
    assert!(!session_is_empty(&state, "busy"));
    assert!(session_is_empty(&state, "parked"));
    assert!(session_is_empty(&state, "agent"), "no Terminal");
}

/// A snapshot without the session's key cannot delete an empty session,
/// and nothing is written.
#[tokio::test]
async fn an_unkeyed_empty_session_is_refused_without_writing() {
    let session = SessionId::new(1);
    let state = SessionSnapshot::new(session, WindowId::new(10), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(session, "parked").with_window_count(0),
        ]);
    let (result, _, seen) = run_selected_through(ScriptSpec::new().state(state), "parked").await;
    assert!(
        matches!(result, Err(KillError::EmptySessionUnsupported { .. })),
        "{result:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
        "{seen:?}"
    );
}

#[tokio::test]
async fn lost_close_replies_never_become_success_and_keep_the_original_identity() {
    use crate::testkit::ScriptedServer;
    use tokio::net::UnixListener;
    for mode in 0..4 {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("drop.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let server = tokio::spawn(async move {
            ScriptedServer::accept_then_drop_reply(&listener, ScriptSpec::new()).await
        });
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let key = IdempotencyKey::new([19; 16]).expect("key");
        let result = match mode {
            0 => {
                kill_session(
                    &mut conn,
                    SessionKey::new([17; 16]).expect("session"),
                    "work",
                )
                .await
            }
            1 => super::session::kill_keyed(&mut conn, "@1", vec![ResourceId::local(1)], key).await,
            2 => {
                super::session::kill_whole_session(
                    &mut conn,
                    "work",
                    vec![ResourceId::local(1), ResourceId::local(2)],
                    Some(key),
                )
                .await
            }
            _ => {
                super::session::kill_each_terminal(
                    &mut conn,
                    vec![ResourceId::local(1), ResourceId::local(2)],
                    "#all",
                )
                .await
            }
        };
        let error = result.expect_err("EOF cannot confirm destruction");
        let document = error.outcome_document().expect("typed outcome");
        assert_eq!(document["error"]["code"], "outcome_unknown");
        assert_eq!(document["error"]["recovery"]["effect_certainty"], "unknown");
        assert_eq!(document["error"]["recovery"]["next_action"], "reconcile");
        if mode == 3 {
            assert_eq!(document["error"]["target"], "#all");
            assert_eq!(
                document["error"]["results"]["not_sent"],
                serde_json::json!(["@2"])
            );
            assert_eq!(
                document["error"]["results"]["accepted"],
                serde_json::json!([])
            );
        }
        drop(conn);
        let frames = server.await.expect("server");
        let command = frames
            .iter()
            .find_map(|frame| match frame {
                FrameKind::Command { command, .. } => Some(command),
                _ => None,
            })
            .expect("one close command");
        let original = match command {
            Command::SessionKill { operation, .. } => Some(operation.to_hex()),
            Command::KillResource { operation_id, .. }
            | Command::KillResources { operation_id, .. } => operation_id.map(|key| key.to_hex()),
            other => panic!("unexpected command {other:?}"),
        };
        assert_eq!(
            document["error"]["recovery"]["operation_id"].as_str(),
            original.as_deref()
        );
        assert_eq!(
            frames
                .iter()
                .filter(|frame| matches!(frame, FrameKind::Command { .. }))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn keyed_session_close_uses_the_callers_operation_without_reminting() {
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    let (_dir, mut conn, server) = crate::testkit::connect_one(org_spec(fixture)).await;
    let operation = IdempotencyKey::new([23; 16]).expect("operation");
    let selector = crate::selector::parse("work").expect("selector");
    let done = selected(
        &mut conn,
        &selector,
        "work",
        Some(operation),
        &mut Vec::new(),
    )
    .await
    .expect("close accepted");
    assert_eq!(
        done.recovery.expect("operation").operation_id,
        Some(operation.to_hex())
    );
    drop(conn);
    let seen = server.await.expect("server");
    assert!(seen.iter().any(|frame| matches!(
        frame, FrameKind::Command { command: Command::SessionKill { operation: actual, .. }, .. }
            if *actual == operation
    )));
}

