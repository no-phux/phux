use super::*;
use crate::state::SharedState;
use phux_protocol::policy::{PeerIdentity, TransportType};

/// The proposal binds the server's whole inherited environment, and a
/// document past the bounded approval payload is refused (a `nix develop`
/// or direnv shell alone exceeds it). These authorization tests must not
/// depend on the developer's shell, so each starts from an empty inherited
/// environment; the budget refusal is covered on its own below.
fn hermetic_environment() {
    for (key, _) in std::env::vars_os() {
        // SAFETY: nextest runs each test in its own process, and no other
        // thread is reading the environment while setup runs.
        unsafe { std::env::remove_var(key) };
    }
}

fn setup() -> (CommandContext, ResourceKey, Revision) {
    hermetic_environment();
    let state = SharedState::new();
    let (client, resource, revision) = state.with_mut(|s| {
        let client = s.clients_mut().new_client_id();
        s.clients_mut().set_peer_identity(
            client,
            PeerIdentity {
                uid: rustix::process::geteuid().as_raw(),
                pid: Some(std::process::id()),
                exe_path: None,
                mcp_host_key: None,
                transport: TransportType::UnixSocket,
                source_addr: None,
            },
        );
        s.set_connection_grant(client, crate::policy::ConnectionGrant::owner());
        s.config_mut().attach_create_seeds_pty = false;
        let (_, id) = s
            .create_terminal(
                crate::state::TerminalTarget::NewSession {
                    name: "archived",
                    lifetime: phux_core::SessionLifetime::Persistent,
                },
                phux_core::TerminalLaunch {
                    argv: vec!["archived-untrusted-binary".into(), "--exact".into()],
                    cwd: Some("/".into()),
                    env: vec![("EXECUTABLE_INPUT".into(), "exact".into())],
                    resume: phux_core::ResumeToken::new("saved-app", "resume-123"),
                    ..Default::default()
                },
                phux_core::ExitPolicy::Retain,
                &phux_core::Actor::owner(),
            )
            .unwrap();
        let record = s.registry().record(id).unwrap().clone();
        s.commit_system(phux_core::Op::ActivationEnded {
            resource: record.key,
            activation: record.activation.unwrap().id,
            state: phux_core::ExecState::Completed,
            outcome: phux_core::Outcome::Unobserved,
            diagnostic: None,
        })
        .unwrap();
        let record = s.registry().record(id).unwrap();
        (
            client,
            ow::resource_key(record.key).unwrap(),
            Revision::new(record.revision.get()),
        )
    });
    let (sender, _) = tokio::sync::mpsc::channel(8);
    (
        CommandContext::for_test(&state, client, &sender),
        resource,
        revision,
    )
}
fn operation(n: u8) -> OperationId {
    OperationId::new([n; 16]).unwrap()
}
fn prepared(ctx: &CommandContext, resource: ResourceKey, revision: Revision) -> RelaunchApproval {
    let result = prepare(ctx, operation(61), resource, revision);
    let CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::Json(json)) = result else {
        panic!("{result:?}");
    };
    serde_json::from_str(&json).unwrap()
}

#[test]
fn preparation_is_inert_and_every_executable_input_is_bound() {
    let (ctx, resource, revision) = setup();
    let before = ctx.state.with(|s| {
        s.org()
            .resource(&ow::core_resource_key(&resource))
            .unwrap()
            .clone()
    });
    let original = prepared(&ctx, resource, revision);
    ctx.state.with(|s| {
        assert_eq!(s.org().resource(&before.key), Some(&before));
        assert!(
            s.org()
                .receipt(&ow::core_operation(&original.operation))
                .is_none()
        );
    });
    let mutations: [fn(&mut RelaunchApproval); 7] = [
        |a: &mut RelaunchApproval| a.operation = operation(62),
        |a: &mut RelaunchApproval| a.resource = ResourceKey::new([123; 16]).unwrap(),
        |a: &mut RelaunchApproval| a.expected = Revision::new(a.expected.get() + 1),
        |a: &mut RelaunchApproval| a.instance = [9; 16],
        |a: &mut RelaunchApproval| a.activation = serde_json::json!({"id":"another-activation"}),
        |a: &mut RelaunchApproval| a.recipe = serde_json::json!({"argv":["substituted"]}),
        |a: &mut RelaunchApproval| {
            a.execution = serde_json::json!({"env":{"LD_PRELOAD":"substituted"}});
        },
    ];
    for mutate in mutations {
        let mut changed = original.clone();
        mutate(&mut changed);
        assert!(matches!(approve(&ctx, &changed), CommandResult::Refused(_)));
    }
    assert!(
        ctx.state
            .with(|s| terminal(s, &before.key, before.revision))
            .is_err()
    );
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
    assert!(matches!(
        approve(&ctx, &approval),
        CommandResult::Refused(_)
    ));
    assert!(matches!(
        prepare(&ctx, operation(62), resource, revision),
        CommandResult::Refused(_)
    ));
    ctx.state
        .with_mut(|s| s.remove_peer_identity(ctx.client_id));
    assert!(
        ctx.state
            .with(|s| s.native_agents().relaunch_proposals.is_empty())
    );
    let (other, _, _) = setup();
    assert!(
        matches!(approve(&other, &approval), CommandResult::Refused(_)),
        "cold restart has no held owner decision"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exact_owner_approval_commits_once_and_identical_replay_never_starts_again() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (ctx, resource, revision) = setup();
            let approval = prepared(&ctx, resource, revision);
            let first = approve(&ctx, &approval);
            assert!(
                matches!(
                    first,
                    CommandResult::Accepted { .. } | CommandResult::OkWith(_)
                ),
                "{first:?}"
            );
            let after = ctx.state.with(|s| {
                s.org()
                    .resource(&ow::core_resource_key(&resource))
                    .unwrap()
                    .clone()
            });
            assert!(matches!(
                approve(&ctx, &approval),
                CommandResult::Accepted { .. }
            ));
            assert_eq!(
                ctx.state
                    .with(|s| s.org().resource(&after.key).unwrap().clone()),
                after
            );
            ctx.root_token.cancel();
        })
        .await;
}

#[test]
fn terminal_resume_uses_current_enabled_program_never_archived_argv_or_environment() {
    let state = SharedState::new();
    let launch = phux_core::TerminalLaunch {
        argv: vec!["old-archived-program".into()],
        resume: phux_core::ResumeToken::new("test-app", "resume-123"),
        ..Default::default()
    };
    state.with(|s| assert!(terminal_recipe(s, &launch).is_err()));
    let configure = |program: &str| {
        state.with_mut(|s| {
            s.config_mut().restore = phux_config::RestoreCfg {
                on_start: false,
                program: vec![phux_config::RestoreProgram {
                    app: "test-app".into(),
                    command: vec![
                        program.into(),
                        "--resume".into(),
                        phux_config::RESUME_TOKEN_PLACEHOLDER.into(),
                    ],
                }],
                ..phux_config::RestoreCfg::default()
            };
        });
    };
    configure("current-program");
    state.with(|s| {
        let LaunchSpec::Terminal(admitted) = terminal_recipe(s, &launch).unwrap() else {
            panic!("terminal");
        };
        assert_eq!(admitted.argv, ["current-program", "--resume", "resume-123"]);
    });
    configure("changed-current-program");
    state.with(|s| {
        let LaunchSpec::Terminal(admitted) = terminal_recipe(s, &launch).unwrap() else {
            panic!("terminal");
        };
        assert_eq!(admitted.argv[0], "changed-current-program");
        let mut with_env = launch.clone();
        with_env
            .env
            .push(("LD_PRELOAD".into(), "/archived/code".into()));
        assert!(terminal_recipe(s, &with_env).is_err());
    });
    state.with_mut(|s| s.config_mut().restore.program.clear());
    state.with(|s| {
        assert!(
            terminal_recipe(s, &launch).is_err(),
            "disabled current program never falls back"
        );
    });
}

fn resume_record(trusted: bool, cwd: Option<&str>, tried: bool) -> phux_core::ProgramResume {
    phux_core::ProgramResume {
        argv: vec!["nvim".into(), "-S".into(), "/tmp/session.vim".into()],
        cwd: cwd.map(std::path::PathBuf::from),
        pid: 74877,
        started: 1,
        uid: rustix::process::geteuid().as_raw(),
        time: 1_795_543_200,
        tried,
        trusted,
    }
}

fn admit(state: &SharedState, launch: &phux_core::TerminalLaunch) -> Result<Vec<String>, ()> {
    state.with(|server| {
        terminal_recipe(server, launch)
            .map(|spec| match spec {
                LaunchSpec::Terminal(launch) => launch.argv,
                other => panic!("{other:?}"),
            })
            .map_err(|_| ())
    })
}

#[test]
fn program_resume_runs_after_an_agent_and_before_a_7501_token_without_a_shell() {
    let state = SharedState::new();
    let uid = rustix::process::geteuid().as_raw();
    state.with_mut(|server| {
        server.config_mut().restore.program_resume = true;
        server.config_mut().restore.trust = vec![phux_config::RestoreTrust {
            command: "nvim".into(),
        }];
        server.config_mut().restore.program = vec![phux_config::RestoreProgram {
            app: "notes".into(),
            command: vec!["notes".into(), phux_config::RESUME_TOKEN_PLACEHOLDER.into()],
        }];
    });
    let trusted = phux_core::TerminalLaunch {
        program_resume: Some(Box::new(resume_record(true, Some("/"), false))),
        resume: phux_core::ResumeToken::new("notes", "legacy"),
        ..phux_core::TerminalLaunch::default()
    };
    assert_eq!(
        admit(&state, &trusted).expect("trusted"),
        ["nvim", "-S", "/tmp/session.vim"],
        "a trusted record is argv, not a shell, and it beats OSC 7501"
    );
    state.with(|server| {
        let LaunchSpec::Terminal(admitted) = terminal_recipe(server, &trusted).unwrap() else {
            panic!("terminal");
        };
        assert!(admitted.program_resume.unwrap().tried);
        assert!(admitted.env.is_empty());
    });
    let untrusted = phux_core::TerminalLaunch {
        program_resume: Some(Box::new(resume_record(false, Some("/"), false))),
        ..phux_core::TerminalLaunch::default()
    };
    assert_eq!(
        admit(&state, &untrusted).expect("shell"),
        Vec::<String>::new()
    );
    let missing = phux_core::TerminalLaunch {
        program_resume: Some(Box::new(resume_record(
            true,
            Some("/no/such/phux-resume-dir"),
            false,
        ))),
        ..phux_core::TerminalLaunch::default()
    };
    assert_eq!(
        admit(&state, &missing).expect("missing cwd is a shell"),
        Vec::<String>::new()
    );
    let tried = phux_core::TerminalLaunch {
        argv: vec!["nvim".into(), "-S".into(), "/tmp/session.vim".into()],
        program_resume: Some(Box::new(resume_record(true, Some("/"), true))),
        ..phux_core::TerminalLaunch::default()
    };
    assert_eq!(
        admit(&state, &tried).expect("no retry"),
        Vec::<String>::new()
    );
    state.with(|server| {
        assert!(
            super::program_resume_launch(server, &trusted, 0).is_none(),
            "root does not resume"
        );
        let mut other = trusted.program_resume.clone().unwrap().as_ref().clone();
        other.uid = uid.wrapping_add(1);
        let mut changed = trusted.clone();
        changed.program_resume = Some(Box::new(other));
        assert!(super::program_resume_launch(server, &changed, uid).is_none());
    });
    let legacy = phux_core::TerminalLaunch {
        program_resume: Some(Box::new(resume_record(false, Some("/"), false))),
        resume: phux_core::ResumeToken::new("notes", "legacy"),
        ..phux_core::TerminalLaunch::default()
    };
    assert_eq!(
        admit(&state, &legacy).expect("7501 after an untrusted record"),
        ["notes", "legacy"]
    );
    let mut agent = trusted;
    agent.restore = Some(b"{}".to_vec());
    assert!(
        admit(&state, &agent).is_err(),
        "agent provenance is decided before a 7577 record"
    );
}

#[test]
fn an_agent_principal_cannot_approve_even_when_it_arrives_on_an_owner_socket() {
    let (ctx, resource, revision) = setup();
    let approval = prepared(&ctx, resource, revision);
    ctx.state.with_mut(|s| {
        s.set_connection_grant(
            ctx.client_id,
            crate::policy::ConnectionGrant::owner()
                .with_kind(phux_core::actor::PrincipalKind::Agent),
        );
    });
    assert!(matches!(
        prepare(&ctx, operation(62), resource, revision),
        CommandResult::Refused(_)
    ));
    assert!(matches!(
        approve(&ctx, &approval),
        CommandResult::Refused(_)
    ));
}

#[test]
fn a_proposal_past_the_approval_payload_is_refused_and_never_held() {
    let (ctx, resource, revision) = setup();
    let inherited = "x".repeat(phux_protocol::native_agent::MAX_NATIVE_PAYLOAD_BYTES);
    // SAFETY: as in `hermetic_environment`; set after it, before any read.
    unsafe { std::env::set_var("PHUX_TEST_LARGE_INHERITED", inherited) };
    let result = prepare(&ctx, operation(61), resource, revision);
    assert!(
        matches!(
            result,
            CommandResult::Error {
                code: phux_protocol::wire::frame::ErrorCode::Unsupported,
                ..
            }
        ),
        "{result:?}"
    );
    assert!(
        ctx.state
            .with(|s| s.native_agents().relaunch_proposals.is_empty()),
        "a refused proposal is never held for approval"
    );
}

#[test]
fn shell_archive_must_match_current_host_defaults_and_relaunch_keeps_the_template() {
    let state = SharedState::new();
    let launch = state.with(|s| {
        let mut shell = phux_terminal_api::launch::default_shell_command(
            s.config().shell.as_str(),
            s.config().login_shell,
        );
        phux_terminal_api::launch::apply_term(&mut shell, s.config().term.as_str());
        phux_core::TerminalLaunch {
            argv: shell
                .get_argv()
                .iter()
                .map(|value| value.to_str().unwrap().to_owned())
                .collect(),
            env: shell
                .environment()
                .iter()
                .map(|(key, value)| {
                    (
                        key.to_str().unwrap().to_owned(),
                        value.to_str().unwrap().to_owned(),
                    )
                })
                .collect(),
            ..Default::default()
        }
    });
    state.with(|s| {
        let LaunchSpec::Terminal(admitted) = terminal_recipe(s, &launch).unwrap() else {
            panic!("terminal");
        };
        assert!(
            admitted.argv.is_empty() && admitted.env.is_empty(),
            "execution re-resolves current trusted shell defaults"
        );
        let mut altered = launch.clone();
        altered
            .env
            .push(("LD_PRELOAD".into(), "/archived/code".into()));
        assert!(terminal_recipe(s, &altered).is_err());
    });
    state.with_mut(|s| s.config_mut().shell = "different-current-shell".into());
    state.with(|s| {
        assert!(
            terminal_recipe(s, &launch).is_err(),
            "old shell argv is never an archive fallback"
        );
    });
}
