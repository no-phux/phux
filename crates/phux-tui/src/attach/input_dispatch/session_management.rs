//! Session management uses the attached server's dedicated control dial, never
//! correlated requests on the output-bearing attach connection. A confirmation
//! pins the authority and persistent session key, not a display name or member set.

use phux_config::keybind::ResolvedAction;
use phux_protocol::ids::{AuthorityId, SessionKey};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::OrgFilter;
use ratatui::{buffer::Buffer, layout::Rect};

use crate::attach::connection::Connection;
use crate::render::overlay::{OverlayCommand, RenderOverlay, SelectItem, SelectList, ToastOverlay};
use crate::render::{ChromeBreakpoints, Theme};

use super::args::{session_id_arg, str_arg};
use super::ctx::DispatchCtx;
use super::effects::ActionEffects;

/// Tab on a session row offers operations without first attaching to it.
pub(super) fn push_actions(action: &ResolvedAction, ctx: &mut DispatchCtx<'_>) {
    let Some(name) = str_arg(action, "name") else {
        return;
    };
    // Satellite rows switch through the relay; other-machine rows leave the
    // attach. Preserve the picker row's navigation instead of guessing by host.
    let switch = str_arg(action, "switch-action");
    let switch = if switch.as_deref() == Some("switch-host") {
        "switch-host"
    } else {
        "switch-session"
    };
    let items = [
        ("Switch to session", switch),
        ("Kill session…", "kill-session"),
        ("Delete empty session…", "delete-empty-session"),
    ]
    .into_iter()
    .map(|(label, verb)| {
        SelectItem::new(
            label,
            ResolvedAction {
                action: verb.to_owned(),
                args: action.args.clone(),
            },
        )
    })
    .collect();
    ctx.overlays.push(Box::new(SelectList::new(
        format!("Session {name:?}"),
        items,
        ctx.theme,
    )));
}

pub(super) fn queue(
    action: &ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    // Even a remote row with a colliding local id/name must never reach our dial.
    if action.args.contains_key("host") {
        notice(
            ctx,
            "Session unchanged",
            vec![
                "Manage this session by attaching to its host first; no local operation was sent."
                    .to_owned(),
            ],
        );
        return;
    }
    let mut action = action.clone();
    if action.args.contains_key("id") && session_id_arg(&action).is_none() {
        notice(
            ctx,
            "Session unchanged",
            vec!["Invalid session identity.".to_owned()],
        );
        return;
    }
    if !action.args.contains_key("id") && !action.args.contains_key("name") {
        let Some(id) = ctx.peers.focused_session else {
            notice(
                ctx,
                "Session unchanged",
                vec!["No current session.".to_owned()],
            );
            return;
        };
        action
            .args
            .insert("id".to_owned(), toml::Value::Integer(i64::from(id.get())));
    }
    effects.session_management = Some(action);
}

#[allow(clippy::future_not_send, reason = "TUI state is current-thread bound")]
pub(super) async fn apply(action: Option<ResolvedAction>, ctx: &mut DispatchCtx<'_>) {
    let Some(action) = action else {
        return;
    };
    let result = async {
        let dial = ctx
            .control_dial
            .ok_or_else(|| "No dedicated session control dial; nothing sent.".to_owned())?;
        let mut conn = Connection::connect_dial(dial)
            .await
            .map_err(|err| err.to_string())?;
        manage(&mut conn, action, ctx.theme).await
    }
    .await;
    // Refresh the retained picker via the driver, never consuming attach output.
    *ctx.host_refresh_request = true;
    match result {
        Ok(Management::Confirm(confirm)) => ctx.overlays.push(Box::new(confirm)),
        Ok(Management::Result(lines)) => notice(ctx, "Session result", lines),
        Err(error) => notice(ctx, "Session management failed", vec![error]),
    }
}

fn notice(ctx: &mut DispatchCtx<'_>, title: &str, lines: Vec<String>) {
    ctx.overlays
        .push(Box::new(ToastOverlay::new(title, lines, ctx.theme)));
}

enum Management {
    Confirm(SessionConfirm),
    Result(Vec<String>),
}

async fn manage(
    conn: &mut Connection,
    action: ResolvedAction,
    theme: &Theme,
) -> Result<Management, String> {
    if action.args.contains_key("host") {
        return Err("Attach to this session's host first; no local operation was sent.".to_owned());
    }
    let deleting = action.action == "delete-empty-session";
    if !deleting && !phux_client::kill::session_kill_supported(conn) {
        return Err("The server does not advertise SESSION_KILL; nothing sent.".to_owned());
    }
    let view = phux_client::org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(|err| err.to_string())?;
    let confirmed = action.args.contains_key("session-key");
    // Once confirmed, resolve ONLY the persistent key. A rename, stale live id,
    // or replacement with the same display name cannot redirect the operation.
    let record = if confirmed {
        let authority = str_arg(&action, "authority").and_then(|text| AuthorityId::parse(&text));
        if authority != Some(view.authority) {
            return Err(
                "The server authority changed; nothing sent. Reopen session management.".to_owned(),
            );
        }
        let key = str_arg(&action, "session-key")
            .and_then(|text| SessionKey::parse(&text))
            .ok_or_else(|| "Invalid persistent session identity; nothing sent.".to_owned())?;
        view.session(&key)
    } else if action.args.contains_key("id") {
        // A painted live id is authoritative; absence never falls back to name.
        session_id_arg(&action).and_then(|id| view.session_by_live(id))
    } else {
        str_arg(&action, "name").and_then(|name| view.session_named(&name))
    }
    .ok_or_else(|| "The selected session no longer exists; nothing changed.".to_owned())?;
    if deleting && record.members != 0 {
        return Err(format!(
            "Session {:?} is not empty ({} members); delete never terminates work. Use Kill session… to end it.",
            record.name, record.members
        ));
    }
    if !confirmed {
        return Ok(confirmation(action, &view, record, theme));
    }
    if deleting {
        // SESSION_DELETE checks emptiness atomically, including work that joins
        // after this inventory read. It never terminates anything.
        phux_client::org::delete_session(conn, record.key)
            .await
            .map_err(|err| err.to_string())?;
        Ok(Management::Result(vec![format!(
            "Deleted empty session {:?} (0 members).",
            record.name
        )]))
    } else {
        let mut lines = match phux_client::kill::kill_session(conn, record.key, &record.name).await
        {
            Ok(selected) => {
                let mut lines = vec![kill_result(&record.name, selected.killed)];
                lines.extend(selected.interleaved);
                lines
            }
            Err(error) => {
                let mut lines = vec![format!("Kill not confirmed: {error}")];
                lines.extend(error.interleaved().iter().cloned());
                lines
            }
        };
        lines.extend(view.degradation.notices().iter().cloned());
        Ok(Management::Result(lines))
    }
}

fn kill_result(name: &str, killed: Option<u32>) -> String {
    // The shared helper also uses None when the receipt was lost. Never turn
    // a lost acknowledgement into a claim of success.
    killed.map_or_else(
        || "Kill request sent; outcome unconfirmed. Refresh the session list before taking further action.".to_owned(),
        |count| format!("Killed session {name:?}; {count} members ended."),
    )
}

fn confirmation(
    mut action: ResolvedAction,
    view: &phux_client::org::OrgInventory,
    record: &phux_protocol::wire::frame::SessionRecord,
    theme: &Theme,
) -> Management {
    let deleting = action.action == "delete-empty-session";
    action.args.insert(
        "authority".to_owned(),
        toml::Value::String(view.authority.to_string()),
    );
    action.args.insert(
        "session-key".to_owned(),
        toml::Value::String(record.key.to_string()),
    );
    action
        .args
        .insert("name".to_owned(), toml::Value::String(record.name.clone()));
    if let Some(id) = record.live {
        action
            .args
            .insert("id".to_owned(), toml::Value::Integer(i64::from(id.get())));
    }
    let mut lines = vec![format!(
        "Session {:?}: {} members now.",
        record.name, record.members
    )];
    lines.push(if deleting {
        "Delete this empty session. No work will be terminated.".to_owned()
    } else {
        "End ALL work in this session, INCLUDING resources added meanwhile, and delete the session. This affects other attached clients too.".to_owned()
    });
    lines.extend(view.degradation.notices().iter().cloned());
    lines.push("Enter confirm · Esc cancel".to_owned());
    Management::Confirm(SessionConfirm {
        notice: ToastOverlay::new(
            if deleting {
                "Delete empty session?"
            } else {
                "Kill session?"
            },
            lines,
            theme,
        ),
        action,
    })
}

/// Unlike a dismiss-on-any-key toast, only an unmodified confirmation press
/// can commit this pinned operation. Resize reflows the same target, not focus.
struct SessionConfirm {
    notice: ToastOverlay,
    action: ResolvedAction,
}

impl RenderOverlay for SessionConfirm {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.notice.render(area, buf);
    }
    fn bounds(&self, area: Rect) -> Option<Rect> {
        self.notice.bounds(area)
    }
    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.notice.set_breakpoints(bp);
    }
    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press || key.mods != ModSet::empty() {
            return OverlayCommand::Stay;
        }
        match key.key {
            PhysicalKey::Enter | PhysicalKey::Y => OverlayCommand::Commit(self.action.clone()),
            PhysicalKey::Escape | PhysicalKey::N => OverlayCommand::Dismiss,
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::super::test_support::CtxFixture;
    use super::*;
    use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    use phux_protocol::wire::frame::{Command, ExecState, FrameKind, Refusal, SessionLifetime};
    use std::collections::BTreeMap;

    fn action(verb: &str) -> ResolvedAction {
        ResolvedAction {
            action: verb.to_owned(),
            args: BTreeMap::from([("name".to_owned(), toml::Value::String("work".to_owned()))]),
        }
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

    fn confirm(result: Management) -> SessionConfirm {
        match result {
            Management::Confirm(confirm) => confirm,
            Management::Result(_) => panic!("expected confirmation"),
        }
    }

    fn key(key: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
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
            SessionKey::parse(&str_arg(&confirmation.action, "session-key").unwrap()),
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
                && text.contains("2 members now")
                && text.contains("INCLUDING resources added")
                && text.contains("meanwhile")
                && text.contains("attached clients too"),
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
        let OverlayCommand::Commit(committed) = confirmation.handle_key(&key(PhysicalKey::Enter))
        else {
            panic!("confirmation")
        };
        let Management::Result(lines) = manage(&mut conn, committed, &Theme::default())
            .await
            .unwrap()
        else {
            panic!("result")
        };
        assert!(
            lines.iter().any(|line| line.contains("2 members ended")),
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
        assert!(error.contains("not empty (1 members)") && error.contains("never terminates"));
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
        let Management::Result(lines) = manage(&mut conn, confirmation.action, &Theme::default())
            .await
            .unwrap()
        else {
            panic!("result")
        };
        assert!(lines[0].contains("0 members"));
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
        let mut wrong_authority = confirmation.action.clone();
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
        let mut missing = confirmation.action;
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
        let Management::Result(lines) = manage(&mut conn, confirmation.action, &Theme::default())
            .await
            .unwrap()
        else {
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
            let result = manage(&mut conn, confirmation.action, &Theme::default()).await;
            if deleting {
                assert!(result.err().unwrap().contains("never terminates"));
            } else {
                let Management::Result(lines) = result.unwrap() else {
                    panic!("result")
                };
                assert!(lines[0].contains("1 members ended"), "{lines:?}");
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
        let Management::Result(lines) = manage(&mut conn, confirmation.action, &Theme::default())
            .await
            .unwrap()
        else {
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
        assert!(kill_result("work", Some(0)).contains("0 members ended"));
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
        let error = manage(&mut conn, confirmation.action, &Theme::default())
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

    #[tokio::test]
    async fn session_management_legacy_server_refuses_without_commands() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, server) = serve_one(dir.path(), ScriptSpec::new());
        let mut conn = Connection::connect(&socket).await.unwrap();
        assert!(
            manage(&mut conn, action("kill-session"), &Theme::default())
                .await
                .err()
                .unwrap()
                .contains("SESSION_KILL")
        );
        drop(conn);
        assert!(edits(&server.await.unwrap()).is_empty());
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
            let mut ctx = fixture.ctx();
            push_actions(&action, &mut ctx);
            let crate::render::overlay::OverlayOutcome::RunAction(selected) =
                ctx.overlays.handle_key(&key(PhysicalKey::Enter))
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
            let mut effects = ActionEffects::default();
            queue(&action, &mut fixture.ctx(), &mut effects);
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
}
