use super::*;

/// A store that will not load still binds, admitting nobody.
#[test]
fn a_legacy_store_still_yields_a_bindable_listener_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("remote-tokens");
    std::fs::write(&path, format!("# old\n{}\n", "ab".repeat(32))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let store = remote_token_store(&path, "wss");
    assert!(
        store.is_empty(),
        "a store that will not load still binds, admitting nobody"
    );
}

/// A mode beside an embedder's engine (ADR-0072) sets the posture, and
/// the injected engine still decides every HELLO: `paired` loads no
/// workload authority of its own and `local` does not replace it.
#[test]
fn an_injected_engine_survives_either_mode() {
    for mode in [
        phux_config::PolicyMode::Paired,
        phux_config::PolicyMode::Local,
    ] {
        let injected: std::sync::Arc<dyn crate::policy::PolicyEngine> =
            std::sync::Arc::new(crate::policy::OwnerForAll);
        let cfg = ServerConfig {
            policy_engine: Some(injected.clone()),
            policy_mode: Some(mode),
            ..ServerConfig::with_default_socket()
        };
        let (posture, engine, fallback) =
            startup_policy(&cfg, false, false).expect("a mode with no listener starts");
        assert!(engine.is_none(), "{mode:?}: no posture engine replaces it");
        assert!(fallback.is_none());
        let state = SharedState::with_config(crate::state::StateConfig::from_runtime(
            &cfg,
            &cfg.socket_path,
            posture,
            engine,
        ));
        let live = state.with(|s| s.config().policy_engine.clone());
        assert!(
            std::ptr::addr_eq(
                std::sync::Arc::as_ptr(&live),
                std::sync::Arc::as_ptr(&injected)
            ),
            "{mode:?}: the injected engine authorizes"
        );
        assert_eq!(
            state.with(|s| s.config().policy_posture),
            match mode {
                phux_config::PolicyMode::Paired => crate::policy::PolicyPosture::Paired,
                phux_config::PolicyMode::Local => crate::policy::PolicyPosture::Local,
            }
        );
    }
}

static DETECT_CALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn detect_records_the_call() -> Vec<std::net::IpAddr> {
    DETECT_CALLED.store(true, std::sync::atomic::Ordering::SeqCst);
    vec![std::net::IpAddr::from([100, 79, 155, 27])]
}

/// A closed gate must skip the `tailscale` shell-out, not just discard
/// its answer.
#[tokio::test]
async fn closed_gate_never_calls_detect() {
    let token = CancellationToken::new();
    token.cancel();
    let ports = AutoOverlayPorts {
        ws: true,
        quic: true,
    };
    let state = SharedState::new();
    serve_auto_overlay_listeners(
        false,
        ports,
        detect_records_the_call,
        state.clone(),
        token,
        None,
    )
    .await
    .expect("parks until shutdown");
    assert!(
        !DETECT_CALLED.load(std::sync::atomic::Ordering::SeqCst),
        "detect() must not run when the gate is closed"
    );
    assert!(!state.with(crate::state::ServerState::has_remote_listener_report));
}

/// A server up before its overlay reports the free overlay port as waiting
/// for its address (#2430), and keeps detecting until shutdown.
#[tokio::test(start_paused = true)]
async fn a_missing_overlay_address_is_reported_as_waiting_for_it() {
    let state = SharedState::new();
    let token = CancellationToken::new();
    let ports = AutoOverlayPorts {
        ws: true,
        quic: false,
    };
    let serve =
        serve_auto_overlay_listeners(true, ports, Vec::new, state.clone(), token.clone(), None);
    let read = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let report = state.with(|s| s.remote_listeners().clone());
        token.cancel();
        report
    };
    let (served, report) = tokio::join!(serve, read);
    served.expect("a shutdown ends the search cleanly");
    assert_eq!(
        report.listeners,
        vec![RemoteListenerSlot::waiting_for_address(
            RemoteListenerTransport::Wss
        )],
        "only the port no explicit address claimed waits"
    );
}

/// Only the default profile auto-binds (ports are host-global), and the
/// disable switch wins over everything.
#[test]
fn auto_overlay_gate_matrix() {
    for (disabled, default_profile, open) in [
        (false, true, true),
        (false, false, false),
        (true, true, false),
        (true, false, false),
    ] {
        assert_eq!(
            auto_overlay_gate_open(disabled, default_profile),
            open,
            "disabled={disabled} default_profile={default_profile}"
        );
    }
}

#[test]
fn exiting_server_unlinks_the_socket_it_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("phux.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let bound = socket_identity(&path);
    drop(listener);

    unlink_socket_if_ours(&path, bound);
    assert!(!path.exists());
}

/// A server whose socket path was taken over must not delete the
/// winner's socket, or the winner becomes unreachable by path.
#[test]
fn exiting_server_leaves_a_socket_that_now_belongs_to_another_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("phux.sock");

    let first = std::os::unix::net::UnixListener::bind(&path).expect("bind A");
    let bound_by_a = socket_identity(&path);
    drop(first);

    // Rename rather than unlink so A's inode stays allocated: ext4 and
    // tmpfs would otherwise hand the same inode number straight to B.
    let parked = dir.path().join("phux.sock.parked");
    std::fs::rename(&path, &parked).expect("park A's inode");

    let _second = std::os::unix::net::UnixListener::bind(&path).expect("bind B");
    let bound_by_b = socket_identity(&path);
    assert_ne!(bound_by_a, bound_by_b, "the test needs distinct inodes");

    unlink_socket_if_ours(&path, bound_by_a);
    assert_eq!(
        socket_identity(&path),
        bound_by_b,
        "the winner's socket survives"
    );
}

#[test]
fn a_server_that_never_established_identity_unlinks_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("phux.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");

    unlink_socket_if_ours(&path, None);

    assert!(path.exists(), "unprovable ownership must not delete");
}

#[test]
fn socket_path_length_limit() {
    let at_limit = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH_LEN - 1)));
    validate_socket_path_len(&at_limit).unwrap();

    let len = MAX_SOCKET_PATH_LEN + 1;
    let path = PathBuf::from(format!("/{}", "a".repeat(len - 1)));
    let err = validate_socket_path_len(&path).unwrap_err();
    assert!(matches!(err, ServerError::SocketPathTooLong { .. }));
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("{len} bytes")),
        "offending length missing from: {msg}"
    );
    assert!(
        msg.contains(&format!("{MAX_SOCKET_PATH_LEN} bytes")),
        "platform limit missing from: {msg}"
    );
    assert!(
        msg.contains("/tmp"),
        "shorter-path remedy missing from: {msg}"
    );
}
