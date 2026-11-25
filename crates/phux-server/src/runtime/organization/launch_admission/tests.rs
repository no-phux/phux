use super::*;
use crate::state::SharedState;
use phux_protocol::policy::{PeerIdentity, TransportType};

fn setup() -> (CommandContext, ResourceKey, Revision) {
    let state = SharedState::new();
    let (client, resource, revision) = state.with_mut(|s| {
        let client = s.clients_mut().new_client_id();
        s.clients_mut().set_peer_identity(client, PeerIdentity {
            uid: rustix::process::geteuid().as_raw(), pid: Some(std::process::id()),
            exe_path: None, mcp_host_key: None, transport: TransportType::UnixSocket, source_addr: None,
        });
        s.set_connection_grant(client, crate::policy::ConnectionGrant::owner());
        s.config_mut().attach_create_seeds_pty = false;
        let (_, id) = s.create_terminal(crate::state::TerminalTarget::NewSession {
            name: "archived", lifetime: phux_core::SessionLifetime::Persistent,
        }, phux_core::TerminalLaunch { argv: vec!["archived-untrusted-binary".into(), "--exact".into()],
            cwd: Some("/".into()), env: vec![("EXECUTABLE_INPUT".into(), "exact".into())],
            resume: phux_core::ResumeToken::new("saved-app", "resume-123"), ..Default::default()
        }, phux_core::ExitPolicy::Retain, &phux_core::Actor::owner()).unwrap();
        let record = s.registry().record(id).unwrap().clone();
        s.commit_system(phux_core::Op::ActivationEnded {
            resource: record.key, activation: record.activation.unwrap().id,
            state: phux_core::ExecState::Completed, outcome: phux_core::Outcome::Unobserved, diagnostic: None,
        }).unwrap();
        let record = s.registry().record(id).unwrap();
        (client, ow::resource_key(record.key).unwrap(), Revision::new(record.revision.get()))
    });
    let (sender, _) = tokio::sync::mpsc::channel(8);
    (CommandContext::for_test(&state, client, &sender), resource, revision)
}
fn operation(n: u8) -> OperationId { OperationId::new([n; 16]).unwrap() }
fn prepared(ctx: &CommandContext, resource: ResourceKey, revision: Revision) -> RelaunchApproval {
    let result = prepare(ctx, operation(61), resource, revision);
    let CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::Json(json)) = result else { panic!("{result:?}"); };
    serde_json::from_str(&json).unwrap()
}

#[test]
fn preparation_is_inert_and_every_executable_input_is_bound() {
    let (ctx, resource, revision) = setup();
    let before = ctx.state.with(|s| s.org().resource(&ow::core_resource_key(&resource)).unwrap().clone());
    let original = prepared(&ctx, resource, revision);
    ctx.state.with(|s| {
        assert_eq!(s.org().resource(&before.key), Some(&before));
        assert!(s.org().receipt(&ow::core_operation(&original.operation)).is_none());
    });
    let mutations: [fn(&mut RelaunchApproval); 7] = [
        |a: &mut RelaunchApproval| a.operation = operation(62),
        |a: &mut RelaunchApproval| a.resource = ResourceKey::new([123;16]).unwrap(),
        |a: &mut RelaunchApproval| a.expected = Revision::new(a.expected.get() + 1),
        |a: &mut RelaunchApproval| a.instance = [9; 16],
        |a: &mut RelaunchApproval| a.activation = serde_json::json!({"id":"another-activation"}),
        |a: &mut RelaunchApproval| a.recipe = serde_json::json!({"argv":["substituted"]}),
        |a: &mut RelaunchApproval| a.execution = serde_json::json!({"env":{"LD_PRELOAD":"substituted"}}),
    ];
    for mutate in mutations {
        let mut changed = original.clone(); mutate(&mut changed);
        assert!(matches!(approve(&ctx, changed), CommandResult::Refused(_)));
    }
    assert!(ctx.state.with(|s| terminal(s, &before.key, before.revision)).is_err());
}

#[test]
fn remote_delegated_disconnect_and_new_incarnation_cannot_reuse_a_proposal() {
    let (ctx, resource, revision) = setup();
    let approval = prepared(&ctx, resource, revision);
    ctx.state.with_mut(|s| {
        let mut peer = s.clients().peer_identity(ctx.client_id).unwrap().clone();
        peer.transport = TransportType::Localhost;
        s.clients_mut().set_peer_identity(ctx.client_id, peer);
    });
    assert!(matches!(approve(&ctx, approval.clone()), CommandResult::Refused(_)));
    assert!(matches!(prepare(&ctx, operation(62), resource, revision), CommandResult::Refused(_)));
    ctx.state.with_mut(|s| s.remove_peer_identity(ctx.client_id));
    assert!(ctx.state.with(|s| s.native_agents().relaunch_proposals.is_empty()));
    let (other, _, _) = setup();
    assert!(matches!(approve(&other, approval), CommandResult::Refused(_)), "cold restart has no held owner decision");
}

#[tokio::test(flavor = "current_thread")]
async fn exact_owner_approval_commits_once_and_identical_replay_never_starts_again() {
    tokio::task::LocalSet::new().run_until(async {
        let (ctx, resource, revision) = setup();
        let approval = prepared(&ctx, resource, revision);
        let first = approve(&ctx, approval.clone());
        assert!(matches!(first, CommandResult::Accepted { .. } | CommandResult::OkWith(_)), "{first:?}");
        let after = ctx.state.with(|s| s.org().resource(&ow::core_resource_key(&resource)).unwrap().clone());
        assert!(matches!(approve(&ctx, approval), CommandResult::Accepted { .. }));
        assert_eq!(ctx.state.with(|s| s.org().resource(&after.key).unwrap().clone()), after);
        ctx.root_token.cancel();
    }).await;
}

#[test]
fn terminal_resume_uses_current_enabled_program_never_archived_argv_or_environment() {
    let state = SharedState::new();
    let launch = phux_core::TerminalLaunch {
        argv: vec!["old-archived-program".into()],
        resume: phux_core::ResumeToken::new("test-app", "resume-123"), ..Default::default()
    };
    state.with(|s| assert!(terminal_recipe(s, &launch).is_err()));
    let configure = |program: &str| state.with_mut(|s| {
        s.config_mut().restore = phux_config::RestoreCfg { on_start: false, program: vec![
            phux_config::RestoreProgram { app: "test-app".into(),
                command: vec![program.into(), "--resume".into(), phux_config::RESUME_TOKEN_PLACEHOLDER.into()] }
        ], ..phux_config::RestoreCfg::default() };
    });
    configure("current-program");
    state.with(|s| {
        let LaunchSpec::Terminal(admitted) = terminal_recipe(s, &launch).unwrap() else { panic!("terminal"); };
        assert_eq!(admitted.argv, ["current-program", "--resume", "resume-123"]);
    });
    configure("changed-current-program");
    state.with(|s| {
        let LaunchSpec::Terminal(admitted) = terminal_recipe(s, &launch).unwrap() else { panic!("terminal"); };
        assert_eq!(admitted.argv[0], "changed-current-program");
        let mut with_env = launch.clone();
        with_env.env.push(("LD_PRELOAD".into(), "/archived/code".into()));
        assert!(terminal_recipe(s, &with_env).is_err());
    });
    state.with_mut(|s| s.config_mut().restore.program.clear());
    state.with(|s| assert!(terminal_recipe(s, &launch).is_err(), "disabled current program never falls back"));
}

#[test]
fn an_agent_principal_cannot_approve_even_when_it_arrives_on_an_owner_socket() {
    let (ctx, resource, revision) = setup();
    let approval = prepared(&ctx, resource, revision);
    ctx.state.with_mut(|s| s.set_connection_grant(ctx.client_id,
        crate::policy::ConnectionGrant::owner().with_kind(phux_core::actor::PrincipalKind::Agent)));
    assert!(matches!(prepare(&ctx, operation(62), resource, revision), CommandResult::Refused(_)));
    assert!(matches!(approve(&ctx, approval), CommandResult::Refused(_)));
}

#[test]
fn shell_archive_must_match_current_host_defaults_and_relaunch_keeps_the_template() {
    let state = SharedState::new();
    let launch = state.with(|s| {
        let mut shell = phux_terminal_api::launch::default_shell_command(s.config().shell.as_str(), s.config().login_shell);
        phux_terminal_api::launch::apply_term(&mut shell, s.config().term.as_str());
        phux_core::TerminalLaunch {
            argv: shell.get_argv().iter().map(|value| value.to_str().unwrap().to_owned()).collect(),
            env: shell.environment().iter().map(|(key, value)| (key.to_str().unwrap().to_owned(), value.to_str().unwrap().to_owned())).collect(),
            ..Default::default()
        }
    });
    state.with(|s| {
        let LaunchSpec::Terminal(admitted) = terminal_recipe(s, &launch).unwrap() else { panic!("terminal"); };
        assert!(admitted.argv.is_empty() && admitted.env.is_empty(), "execution re-resolves current trusted shell defaults");
        let mut altered = launch.clone();
        altered.env.push(("LD_PRELOAD".into(), "/archived/code".into()));
        assert!(terminal_recipe(s, &altered).is_err());
    });
    state.with_mut(|s| s.config_mut().shell = "different-current-shell".into());
    state.with(|s| assert!(terminal_recipe(s, &launch).is_err(), "old shell argv is never an archive fallback"));
}
