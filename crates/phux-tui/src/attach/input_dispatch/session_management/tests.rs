use super::super::test_support::CtxFixture;
use super::*;
use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{Command, ExecState, FrameKind, Refusal, SessionLifetime};
use ratatui::{buffer::Buffer, layout::Rect};
use std::collections::BTreeMap;

use crate::render::overlay::{OverlayCommand, RenderOverlay};

fn action(verb: &str) -> ResolvedAction {
    crate::attach::resolved(
        verb,
        BTreeMap::from([("name".to_owned(), toml::Value::String("work".to_owned()))]),
    )
}

async fn connect(
    fixture: OrgFixture,
) -> (
    Connection,
    tokio::task::JoinHandle<Vec<FrameKind>>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[
            ServerFeatureExt::Organization,
            ServerFeatureExt::SessionKill,
        ]))
        .organization(fixture);
    let (socket, server) = serve_one(dir.path(), spec);
    (Connection::connect(&socket).await.unwrap(), server, dir)
}

fn confirm(result: Management) -> ConfirmOverlay {
    match result {
        Management::Confirm(confirm) => *confirm,
        Management::Result(_) => panic!("expected confirmation"),
    }
}

use crate::test_util::press as key;

fn confirmed_action(confirm: &ConfirmOverlay) -> ResolvedAction {
    let OverlayCommand::Commit(action) = confirm.clone().handle_key(&key(PhysicalKey::Y)) else {
        panic!("explicit confirmation")
    };
    action
}

fn edits(seen: &[FrameKind]) -> Vec<&Command> {
    seen.iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::GetOrganization { .. },
                ..
            } => None,
            FrameKind::Command { command, .. } => Some(command),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn session_management_kill_confirms_then_sends_one_session_kill() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    fixture.resource(work, Some(ExecState::Running));
    fixture.resource(work, Some(ExecState::Completed));
    let (mut conn, server, _dir) = connect(fixture).await;
    let mut confirmation = confirm(
        manage(&mut conn, action("kill-session"), &Theme::default())
            .await
            .unwrap(),
    );
    assert_eq!(
        SessionKey::parse(&str_arg(&confirmed_action(&confirmation), "session-key").unwrap()),
        Some(work)
    );
    // The prompt names the session and all member records, including retained ones.
    let area = Rect::new(0, 0, 120, 40);
    let mut buf = Buffer::empty(area);
    confirmation.render(area, &mut buf);
    let text: String = buf
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        text.contains("work")
            && text.contains("everything in it (2 now)")
            && text.contains("Anything added before this runs")
            && text.contains("attached client")
            && text.contains("Kill session")
            && text.contains("Cancel")
            && !text.contains("any key"),
        "{text}"
    );
    assert_eq!(
        confirmation.handle_key(&key(PhysicalKey::Escape)),
        OverlayCommand::Dismiss
    );
    let mut repeated = key(PhysicalKey::Enter);
    repeated.action = KeyAction::Repeat;
    assert_eq!(confirmation.handle_key(&repeated), OverlayCommand::Stay);
    repeated.action = KeyAction::Press;
    repeated.mods = ModSet::CTRL;
    assert_eq!(confirmation.handle_key(&repeated), OverlayCommand::Stay);
    assert_eq!(
        confirmation.handle_key(&key(PhysicalKey::Enter)),
        OverlayCommand::Dismiss
    );
    let committed = confirmed_action(&confirmation);
    let Management::Result(lines) = manage(&mut conn, committed, &Theme::default())
        .await
        .unwrap()
    else {
        panic!("result")
    };
    assert!(
        lines.iter().any(|line| line.contains("\"work\"; 2 ended")),
        "{lines:?}"
    );
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        matches!(edits(&seen).as_slice(), [Command::SessionKill { session, .. }] if *session == work)
    );
}

#[tokio::test]
async fn session_management_cancel_and_delete_nonempty_never_mutate() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    fixture.resource(work, Some(ExecState::Completed));
    let (mut conn, server, _dir) = connect(fixture).await;
    let mut confirmation = confirm(
        manage(&mut conn, action("kill-session"), &Theme::default())
            .await
            .unwrap(),
    );
    assert_eq!(
        confirmation.handle_key(&key(PhysicalKey::N)),
        OverlayCommand::Dismiss
    );
    let error = manage(&mut conn, action("delete-empty-session"), &Theme::default())
        .await
        .err()
        .unwrap();
    assert!(error.contains("still holds 1") && error.contains("never ends work"));
    drop(conn);
    assert!(edits(&server.await.unwrap()).is_empty());
}

#[tokio::test]
async fn session_management_delete_empty_only_deletes() {
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    let (mut conn, server, _dir) = connect(fixture).await;
    let confirmation = confirm(
        manage(&mut conn, action("delete-empty-session"), &Theme::default())
            .await
            .unwrap(),
    );
    let Management::Result(lines) = manage(
        &mut conn,
        confirmed_action(&confirmation),
        &Theme::default(),
    )
    .await
    .unwrap() else {
        panic!("result")
    };
    assert!(lines[0].starts_with("Deleted"));
    drop(conn);
    assert!(matches!(
        edits(&server.await.unwrap()).as_slice(),
        [Command::SessionDelete { .. }]
    ));
}

#[tokio::test]
async fn session_management_rejects_changed_authority_missing_key_and_stale_live_id() {
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    let (mut conn, server, _dir) = connect(fixture).await;
    let confirmation = confirm(
        manage(&mut conn, action("kill-session"), &Theme::default())
            .await
            .unwrap(),
    );
    let mut wrong_authority = confirmed_action(&confirmation);
    wrong_authority.args.insert(
        "authority".to_owned(),
        toml::Value::String(AuthorityId::new([7; 16]).unwrap().to_string()),
    );
    assert!(
        manage(&mut conn, wrong_authority, &Theme::default())
            .await
            .err()
            .unwrap()
            .contains("authority changed")
    );
    let mut missing = confirmed_action(&confirmation);
    missing.args.insert(
        "session-key".to_owned(),
        toml::Value::String(SessionKey::new([8; 16]).unwrap().to_string()),
    );
    assert!(
        manage(&mut conn, missing, &Theme::default())
            .await
            .err()
            .unwrap()
            .contains("no longer exists")
    );
    for id in [
        toml::Value::Integer(999),
        toml::Value::String("invalid".to_owned()),
    ] {
        let mut stale = action("kill-session");
        stale.args.insert("id".to_owned(), id);
        assert!(
            manage(&mut conn, stale, &Theme::default())
                .await
                .err()
                .unwrap()
                .contains("no longer exists")
        );
    }
    drop(conn);
    assert!(edits(&server.await.unwrap()).is_empty());
}

#[tokio::test]
async fn session_management_renamed_session_keeps_its_pinned_identity() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let (mut conn, server, _dir) = connect(fixture).await;
    let confirmation = confirm(
        manage(&mut conn, action("delete-empty-session"), &Theme::default())
            .await
            .unwrap(),
    );
    phux_client::org::request(
        &mut conn,
        2,
        Command::SessionRename {
            operation: phux_client::org::mint_operation(),
            session: work,
            name: "renamed".to_owned(),
        },
    )
    .await
    .unwrap();
    let Management::Result(lines) = manage(
        &mut conn,
        confirmed_action(&confirmation),
        &Theme::default(),
    )
    .await
    .unwrap() else {
        panic!("result")
    };
    assert!(lines[0].contains("renamed"), "{lines:?}");
    drop(conn);
    let seen = server.await.unwrap();
    assert!(edits(&seen).iter().any(
        |command| matches!(command, Command::SessionDelete { session, .. } if *session == work)
    ));
}

#[tokio::test]
async fn session_management_members_added_after_confirmation_are_killed_but_never_deleted() {
    for deleting in [false, true] {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let (mut conn, server, _dir) = connect(fixture).await;
        let verb = if deleting {
            "delete-empty-session"
        } else {
            "kill-session"
        };
        let confirmation = confirm(
            manage(&mut conn, action(verb), &Theme::default())
                .await
                .unwrap(),
        );
        phux_client::org::create_resource(
            &mut conn,
            work,
            phux_protocol::ids::ResourceKind::Terminal,
            None,
            phux_protocol::wire::frame::LaunchMode::Deferred,
        )
        .await
        .unwrap();
        let result = manage(
            &mut conn,
            confirmed_action(&confirmation),
            &Theme::default(),
        )
        .await;
        if deleting {
            assert!(result.err().unwrap().contains("never ends work"));
        } else {
            let Management::Result(lines) = result.unwrap() else {
                panic!("result")
            };
            assert!(lines[0].contains("; 1 ended"), "{lines:?}");
        }
        drop(conn);
        let seen = server.await.unwrap();
        if deleting {
            assert!(matches!(
                edits(&seen).as_slice(),
                [Command::ResourceCreate { .. }]
            ));
        } else {
            assert!(
                matches!(edits(&seen).as_slice(), [Command::ResourceCreate { .. }, Command::SessionKill { session, .. }] if *session == work)
            );
        }
    }
}

#[tokio::test]
async fn session_management_refusal_does_not_claim_success_or_retry_members() {
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    fixture = fixture.refuse_next("session_kill", Refusal::ExecutionUncertain);
    let (mut conn, server, _dir) = connect(fixture).await;
    let confirmation = confirm(
        manage(&mut conn, action("kill-session"), &Theme::default())
            .await
            .unwrap(),
    );
    let Management::Result(lines) = manage(
        &mut conn,
        confirmed_action(&confirmation),
        &Theme::default(),
    )
    .await
    .unwrap() else {
        panic!("result")
    };
    assert!(lines[0].contains("Kill not confirmed"), "{lines:?}");
    assert!(lines[0].contains("lost execution"), "{lines:?}");
    drop(conn);
    assert!(matches!(
        edits(&server.await.unwrap()).as_slice(),
        [Command::SessionKill { .. }]
    ));
}

#[test]
fn session_management_missing_receipt_is_explicitly_unconfirmed() {
    let line = kill_result("work", None);
    assert!(line.contains("outcome unconfirmed"));
    assert!(!line.contains("Killed session"));
    assert!(kill_result("work", Some(0)).contains("; 0 ended"));
}

#[tokio::test]
async fn session_management_replacement_with_same_name_is_not_the_confirmed_target() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let (mut conn, server, _dir) = connect(fixture).await;
    let confirmation = confirm(
        manage(&mut conn, action("kill-session"), &Theme::default())
            .await
            .unwrap(),
    );
    phux_client::org::delete_session(&mut conn, work)
        .await
        .unwrap();
    phux_client::org::request(
        &mut conn,
        3,
        Command::SessionCreate {
            operation: phux_client::org::mint_operation(),
            name: "work".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: vec![],
            launch: None,
        },
    )
    .await
    .unwrap();
    let error = manage(
        &mut conn,
        confirmed_action(&confirmation),
        &Theme::default(),
    )
    .await
    .err()
    .unwrap();
    assert!(error.contains("no longer exists"), "{error}");
    drop(conn);
    assert!(matches!(
        edits(&server.await.unwrap()).as_slice(),
        [Command::SessionDelete { .. }, Command::SessionCreate { .. }]
    ));
}

#[test]
fn session_management_preserves_satellite_and_machine_navigation() {
    for verb in ["switch-session", "switch-host"] {
        let mut fixture = CtxFixture::default();
        let mut action = action("session-actions");
        action
            .args
            .insert("host".to_owned(), toml::Value::String("edge".to_owned()));
        action.args.insert(
            "switch-action".to_owned(),
            toml::Value::String(verb.to_owned()),
        );
        fixture.run(&action);
        let crate::render::overlay::OverlayOutcome::RunAction(selected) =
            fixture.overlays.handle_key(&key(PhysicalKey::Enter))
        else {
            panic!("switch")
        };
        assert_eq!(selected.action, verb);
        assert_eq!(str_arg(&selected, "host").as_deref(), Some("edge"));
    }
}

#[test]
fn session_management_remote_target_is_never_queued_locally() {
    let mut fixture = CtxFixture::default();
    let mut action = action("kill-session");
    action
        .args
        .insert("host".to_owned(), toml::Value::String("edge".to_owned()));
    action.args.insert("id".to_owned(), toml::Value::Integer(1));
    for verb in ["kill-session", "delete-empty-session"] {
        verb.clone_into(&mut action.action);
        let effects = fixture.run(&action);
        assert!(effects.session_management.is_none());
    }
}

#[tokio::test]
async fn session_management_effect_uses_only_the_dedicated_dial() {
    let mut org = OrgFixture::new();
    org.session("work", SessionLifetime::Persistent);
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[
            ServerFeatureExt::Organization,
            ServerFeatureExt::SessionKill,
        ]))
        .organization(org);
    let (socket, server) = serve_one(dir.path(), spec);
    let dial = crate::attach::Dial::uds(&socket);
    let mut fixture = CtxFixture::default();
    let mut ctx = fixture.ctx();
    ctx.control_dial = Some(&dial);
    apply(Some(action("kill-session")), &mut ctx).await;
    assert!(ctx.overlays.is_active());
    assert!(
        *ctx.host_refresh_request,
        "refresh the retained session inventory"
    );
    let seen = server.await.unwrap();
    assert!(edits(&seen).is_empty(), "preparation cannot mutate");
    assert!(seen.iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::GetOrganization { .. },
            ..
        }
    )));
}
