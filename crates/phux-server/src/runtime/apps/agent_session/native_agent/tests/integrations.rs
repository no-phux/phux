use super::*;

fn launch(fx: &Fixture) -> NativeRequest {
    let session = fx.state.with(|s| {
        crate::org_wire::session_key(
            s.registry()
                .session_key(s.find_session_by_name("alpha").unwrap())
                .unwrap(),
        )
        .unwrap()
    });
    NativeRequest::LaunchIntegration {
        integration: "opencode".into(),
        session,
        cwd: "/".into(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn integration_revalidation_refuses_an_upgrade_sealed_during_resolution() {
    tokio::task::LocalSet::new().run_until(async {
    let fx = Fixture::new();
    let request = launch(&fx);
    let NativeRequest::LaunchIntegration { session, .. } = &request else {
        unreachable!()
    };
    let ctx = fx.context(fx.owner);
    assert!(super::super::integrations::authorize_launch(&ctx, &request, session).is_ok());
    fx.state
        .with_mut(|state| state.native_agents_mut().sealed = true);
    let result = super::super::integrations::authorize_launch(&ctx, &request, session);
    assert!(
        matches!(result, Err(CommandResult::Error { code: ErrorCode::Overflow, ref message }) if message.contains("no agent was started"))
    );
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_integration_host_and_viewer_cannot_launch() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let request = launch(&fx);
            let host = handle(&fx.context(fx.host), request.clone()).await;
            assert!(matches!(
                host,
                CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            ));
            let (tx, _rx) = mpsc::channel(32);
            fx.state.with_mut(|s| {
                s.attach_default_caps(fx.owner, "alpha", tx).unwrap();
                s.set_attached_viewer(fx.owner, true);
            });
            let viewer = handle(&fx.context(fx.owner), request).await;
            assert!(matches!(
                viewer,
                CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_integration_stale_key_never_adopts_an_existing_session_name() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let NativeRequest::LaunchIntegration {
                integration, cwd, ..
            } = launch(&fx)
            else {
                unreachable!()
            };
            let request = NativeRequest::LaunchIntegration {
                integration,
                cwd,
                session: phux_protocol::ids::SessionKey::new([0xa8; 16]).unwrap(),
            };
            let count = fx.state.with(|s| s.registry().resources().count());
            assert!(matches!(
                handle(&fx.context(fx.owner), request).await,
                CommandResult::Error { .. }
            ));
            assert_eq!(count, fx.state.with(|s| s.registry().resources().count()));
        })
        .await;
}
