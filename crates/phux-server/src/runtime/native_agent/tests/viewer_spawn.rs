#[tokio::test(flavor = "current_thread")]
async fn viewer_native_spawn_cannot_acquire_input_authority() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use std::os::unix::fs::PermissionsExt;
            let fx = Fixture::new();
            // A real inert guardian process, not a provider or protocol echo.
            let executable = fx.dir.path().join("guardian");
            std::fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let (tx, _rx) = mpsc::channel(32);
            let spec = fx.state.with_mut(|s| {
                s.attach_default_caps(fx.owner, "alpha", tx).unwrap();
                s.set_attached_viewer(fx.owner, true);
                s.set_native_host_executable(Some(executable));
                s.native_agents().bindings[&fx.agent].launch.clone()
            });
            let spawned: SpawnedNativeAgent =
                decode(fx.request(fx.owner, NativeRequest::Spawn { spec }).await);
            assert!(fx.state.with(|s| s.is_viewer(fx.owner, &spawned.parent)));
            assert!(fx.state.with(|s| s.is_viewer(fx.owner, &spawned.agent)));
            fx.state.with_mut(|s| {
                let binding = s
                    .native_agents_mut()
                    .bindings
                    .get_mut(&spawned.agent)
                    .unwrap();
                binding.view.connected = true;
                binding.view.capabilities.prompt = true.into();
            });
            let request = NativeRequest::Command {
                agent_session_id: spawned.agent.clone(),
                command: NativeCommand {
                    operation_id: 120,
                    activation: spawned.activation,
                    intent: prompt("viewer must not send"),
                },
            };
            let command = Command::NativeAgent {
                request: NativeRequestPayload::new(request.clone()).unwrap(),
            };
            assert!(
                fx.state
                    .with(|s| crate::policy::authorize_command(s, fx.owner, &command))
                    .is_err(),
                "viewer input must be denied by the public dispatch guard"
            );
            let result: NativeCommandResult = decode(handle(&fx.context(fx.owner), request).await);
            assert_eq!(result.disposition, NativeDisposition::Refused);
            assert!(fx.state.with(|s| {
                s.native_agents().bindings[&spawned.agent]
                    .pending
                    .is_empty()
            }));
            let parent = fx
                .state
                .with(|s| s.terminal_from_wire(&spawned.parent).unwrap());
            fx.state.with_mut(|s| {
                s.reap_terminal(parent);
            });
        })
        .await;
}
