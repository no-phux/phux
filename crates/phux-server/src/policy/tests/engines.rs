// -----------------------------------------------------------------------------
// Engines and posture.
// -----------------------------------------------------------------------------

/// A peer on `transport` running as the serving user.
fn peer(transport: TransportType) -> PeerIdentity {
    PeerIdentity {
        uid: nix::unistd::geteuid().as_raw(),
        pid: None,
        exe_path: None,
        mcp_host_key: None,
        transport,
        source_addr: None,
    }
}

fn credential(id: &str, scopes: &[&str]) -> AuthenticatedCredential {
    AuthenticatedCredential {
        id: id.to_owned(),
        principal: id.to_owned(),
        scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        issued_at: Utc::now(),
        expires_at: None,
        generation: 1,
        registry_instance: None,
    }
}

/// A registry file holding one credential with `scopes`.
fn enrolled(scopes: &[&str]) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workload-keys");
    let scopes = scopes.iter().map(|scope| (*scope).to_owned()).collect();
    let id = WorkloadRegistry::register(&path, b"policy-test-key", scopes, None)
        .unwrap()
        .id;
    (dir, path, id)
}

fn paired_policy(path: &std::path::Path) -> ScopedPolicy {
    ScopedPolicy::paired(Arc::new(
        ReloadingWorkloadRegistry::load(path.to_owned()).unwrap(),
    ))
}

#[test]
fn local_policy_admits_only_the_owner_socket() {
    let policy = ScopedPolicy::local();
    assert!(
        policy
            .decide(&peer(TransportType::UnixSocket), None)
            .unwrap()
            .is_owner()
    );
    for transport in [
        TransportType::Quic,
        TransportType::WebSocket,
        TransportType::WebTransport,
        TransportType::SshTunnel,
        TransportType::Localhost,
    ] {
        assert!(
            policy.decide(&peer(transport), None).is_err(),
            "{transport:?}"
        );
    }
}

#[test]
fn paired_policy_mints_the_registry_ceiling_not_the_cached_scopes() {
    let (_dir, path, id) = enrolled(&["observe@host:devbox"]);
    let policy = paired_policy(&path);
    // The owner socket keeps the owner's grant in paired mode.
    assert!(
        policy
            .decide(&peer(TransportType::UnixSocket), None)
            .unwrap()
            .is_owner()
    );
    let grant = policy
        .decide(
            &peer(TransportType::Quic),
            Some(&credential(&id, &["*@global"])),
        )
        .unwrap();
    let Authority::Scoped { granted, .. } = &grant.authority else {
        panic!("a workload connection got the owner's grant");
    };
    assert_eq!(
        granted,
        &TerminalScopeSet::parse_all(&["observe@host:devbox"]).unwrap()
    );
    assert_eq!(grant.credential_id.as_deref(), Some(id.as_str()));
    assert_eq!(grant.registry_generation, 1);
    assert!(grant.registry_instance.is_some());
}

#[test]
fn paired_policy_refuses_bearer_unknown_and_revoked_credentials() {
    let (_dir, path, id) = enrolled(&["*@global"]);
    let policy = paired_policy(&path);
    let quic = peer(TransportType::Quic);
    assert!(policy.decide(&quic, None).is_err(), "no credential");
    assert!(
        policy
            .decide(&quic, Some(&credential("bearer-1", &["*@global"])))
            .is_err(),
        "a bearer credential is admission, never authority"
    );
    let unknown = format!("sha256:{}", "0".repeat(64));
    assert!(
        policy
            .decide(&quic, Some(&credential(&unknown, &[])))
            .is_err()
    );
    assert!(policy.decide(&quic, Some(&credential(&id, &[]))).is_ok());
    WorkloadRegistry::revoke(&path, &id).unwrap();
    assert!(
        policy.decide(&quic, Some(&credential(&id, &[]))).is_err(),
        "a revoked credential mints nothing at the next HELLO"
    );
}

#[test]
fn paired_mode_without_registry_denies_everything() {
    let dir = tempfile::tempdir().unwrap();
    let policy = paired_policy(&dir.path().join("workload-keys"));
    let id = format!("sha256:{}", "a".repeat(64));
    for transport in [
        TransportType::Quic,
        TransportType::WebSocket,
        TransportType::WebTransport,
    ] {
        assert!(
            policy
                .decide(&peer(transport), Some(&credential(&id, &["*@global"])))
                .is_err(),
            "{transport:?}"
        );
    }
}

#[test]
fn posture_follows_the_mode_and_the_listeners() {
    use phux_config::PolicyMode::{Local, Paired};
    // (mode, remote listener, workload location named, embedder engine)
    let cases = [
        // No mode and nothing past the owner's socket is `local`.
        (None, false, false, false, Ok(PolicyPosture::Local)),
        // No mode beside a remote door is refused, never an owner grant for
        // whoever reaches it (ADR-0179).
        (
            None,
            true,
            false,
            false,
            Err(PostureError::UnsetWithRemoteListener),
        ),
        (Some(Paired), true, false, false, Ok(PolicyPosture::Paired)),
        (Some(Paired), false, true, false, Ok(PolicyPosture::Paired)),
        (Some(Paired), true, false, true, Ok(PolicyPosture::Paired)),
        (Some(Local), false, false, false, Ok(PolicyPosture::Local)),
        (Some(Local), false, true, false, Ok(PolicyPosture::Local)),
        (
            Some(Local),
            true,
            false,
            false,
            Err(PostureError::LocalWithRemoteListener),
        ),
        (
            Some(Local),
            true,
            false,
            true,
            Err(PostureError::LocalWithRemoteListener),
        ),
        // A named CA or registry location with no mode is refused, with or
        // without a listener (workload-auth §8).
        (
            None,
            false,
            true,
            false,
            Err(PostureError::UnsetWithWorkloadAuthority),
        ),
        (
            None,
            true,
            true,
            false,
            Err(PostureError::UnsetWithWorkloadAuthority),
        ),
        // An embedder's own engine with no mode owns the decision.
        (None, true, false, true, Ok(PolicyPosture::Embedded)),
        (None, false, true, true, Ok(PolicyPosture::Embedded)),
        // A mode wins over an embedder's engine: the posture follows the
        // mode, and the runtime keeps the injected engine authorizing.
        (Some(Paired), false, false, true, Ok(PolicyPosture::Paired)),
        (Some(Local), false, false, true, Ok(PolicyPosture::Local)),
    ];
    for (mode, remote, located, embedded, want) in cases {
        let inputs = PostureInputs {
            mode,
            retired_workload_mtls_env: false,
            remote_listener: remote,
            workload_authority_configured: located,
            embedder_engine: embedded,
        };
        assert_eq!(
            PolicyPosture::resolve(inputs),
            want,
            "{mode:?} remote={remote} located={located} embedded={embedded}"
        );
    }
    for message in [
        PostureError::UnsetWithWorkloadAuthority.to_string(),
        PostureError::UnsetWithRemoteListener.to_string(),
        PostureError::RetiredWorkloadMtlsEnv.to_string(),
    ] {
        assert!(
            message.contains("mode = \"paired\""),
            "the refusal names the remedy: {message}"
        );
    }
    assert!(PolicyPosture::Paired.requires_workload_mtls());
    assert!(!PolicyPosture::Local.requires_workload_mtls());
    assert!(!PolicyPosture::Embedded.requires_workload_mtls());
    assert!(!PolicyPosture::Embedded.is_local());
}

/// A resumed image never exits on its posture (ADR-0032): the retired
/// variable is ignored when the rest resolves, and anything else still
/// refused runs `local` with the remote entry points unbound.
#[test]
fn a_resume_falls_back_instead_of_refusing() {
    use phux_config::PolicyMode::{Local, Paired};
    // (mode, retired env, remote listener, workload location) -> (posture, dropped)
    let cases = [
        (None, false, true, false, PolicyPosture::Local, true),
        (None, true, true, false, PolicyPosture::Local, true),
        (
            Some(Paired),
            true,
            true,
            false,
            PolicyPosture::Paired,
            false,
        ),
        (Some(Local), true, false, false, PolicyPosture::Local, false),
        (Some(Local), false, true, false, PolicyPosture::Local, true),
        (None, false, false, true, PolicyPosture::Local, false),
        (None, true, false, false, PolicyPosture::Local, false),
    ];
    for (mode, retired, remote, located, posture, dropped) in cases {
        let inputs = PostureInputs {
            mode,
            retired_workload_mtls_env: retired,
            remote_listener: remote,
            workload_authority_configured: located,
            embedder_engine: false,
        };
        let refusal = PolicyPosture::resolve(inputs).expect_err("a cold start refuses");
        let fallback = PolicyPosture::resume_fallback(inputs, refusal);
        assert_eq!(
            (fallback.posture, fallback.remote_dropped, fallback.refusal),
            (posture, dropped, refusal),
            "{mode:?} retired={retired} remote={remote} located={located}"
        );
    }
}

#[test]
fn retired_workload_mtls_variable_refuses_every_mode() {
    use phux_config::PolicyMode::{Local, Paired};
    for mode in [None, Some(Local), Some(Paired)] {
        for embedder_engine in [false, true] {
            let inputs = PostureInputs {
                mode,
                retired_workload_mtls_env: true,
                embedder_engine,
                ..PostureInputs::default()
            };
            assert_eq!(
                PolicyPosture::resolve(inputs),
                Err(PostureError::RetiredWorkloadMtlsEnv),
                "{mode:?} embedded={embedder_engine}"
            );
        }
    }
}

#[test]
fn denial_errors_are_limited_to_one_per_interval() {
    let mut grant = scoped(&["observe@global"]);
    let start = Instant::now();
    assert!(grant.admit_denial_error(start));
    assert!(!grant.admit_denial_error(start + Duration::from_millis(999)));
    assert!(grant.admit_denial_error(start + Duration::from_secs(1)));
}
