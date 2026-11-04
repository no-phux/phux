#![allow(clippy::unwrap_used, reason = "tests")]

use super::super::handoff::*;
use super::super::validate::*;
use super::super::*;
use super::support::*;
use phux_config::instance::BuildKind;

/// Every opt-in flag the server applied is re-emitted on the resume argv
/// (dropping them silently was the original upgrade bug); a default
/// server re-execs with the bare argv; a sub-second idle lifetime rounds
/// up rather than becoming `--exit-after-idle 0`.
#[test]
fn resume_args_reconstruct_the_served_surface() {
    const BASE: [&str; 5] = ["server", "--resume", "7", "--socket", "/run/phux/phux.sock"];
    assert_eq!(args_as_strings(RuntimeFlags::default()), BASE);

    let every = RuntimeFlags {
        ws_addr: Some("127.0.0.1:8787".parse().unwrap()),
        quic_addr: Some("0.0.0.0:4433".parse().unwrap()),
        wt_addr: Some("0.0.0.0:4434".parse().unwrap()),
        connect: Some("relay.example:4433".to_owned()),
        hub: true,
        exit_after_idle: Some(Duration::from_secs(90)),
        upgrade_source_exe: None,
        autosave: Some(PathBuf::from("/home/u/.local/state/phux/workspace.json")),
    };
    let mut expected: Vec<&str> = BASE.to_vec();
    expected.extend([
        "--listen",
        "127.0.0.1:8787",
        "--quic",
        "0.0.0.0:4433",
        "--webtransport",
        "0.0.0.0:4434",
        "--connect",
        "relay.example:4433",
        "--hub",
        "--autosave",
        "/home/u/.local/state/phux/workspace.json",
        "--exit-after-idle",
        "90",
    ]);
    assert_eq!(args_as_strings(every), expected);

    let sub_second = RuntimeFlags {
        exit_after_idle: Some(Duration::from_millis(300)),
        ..RuntimeFlags::default()
    };
    assert_eq!(args_as_strings(sub_second)[5..], ["--exit-after-idle", "1"]);
}

/// A server the doorbell promoted into a hub re-execs with `--hub`, even
/// after the registry is emptied again (an empty `--hub` registry starts
/// cleanly); one that never grew links keeps its builder flags.
#[tokio::test(flavor = "current_thread")]
async fn a_promoted_hub_resumes_with_hub() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let cancel = tokio_util::sync::CancellationToken::new();
            state.with_mut(|s| {
                s.set_upgrade_context(
                    3,
                    PathBuf::from("/run/phux/phux.sock"),
                    RuntimeFlags::default(),
                );
                s.arm_hub_reload(None, cancel.clone());
            });
            let served = |state: &SharedState| {
                args_as_strings(capture_upgrade_context(state).unwrap().flags)
            };
            assert!(!served(&state).contains(&"--hub".to_owned()));
            assert!(state.with_mut(|s| s.hub_ensure_links(std::ffi::OsString::from("ssh"))));
            assert!(served(&state).contains(&"--hub".to_owned()));

            let empty = crate::hub::HubTable::from_registry(&[]).unwrap();
            state.with_mut(|s| s.hub_replace_table(empty, &state));
            assert!(served(&state).contains(&"--hub".to_owned()));
            assert!(crate::hub::resolve_hub_table(true, &[]).is_ok());
            cancel.cancel();
        })
        .await;
}

/// A promoted hub whose on-disk registry no longer validates refuses the
/// upgrade before exec, instead of resuming into a `--hub` startup that
/// exits and takes the panes with it.
#[tokio::test(flavor = "current_thread")]
async fn an_invalid_registry_blocks_a_promoted_hub_upgrade() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let registry =
                std::sync::Arc::new(std::sync::Mutex::new(vec![satellite("box", "ssh://box")]));
            let read = std::sync::Arc::clone(&registry);
            let source = crate::hub::SatelliteSource::new(move || Ok(read.lock().unwrap().clone()));
            let state = SharedState::new();
            let cancel = tokio_util::sync::CancellationToken::new();
            let listener = tempfile::tempfile().unwrap();
            let dir = tempfile::tempdir().unwrap();
            state.with_mut(|s| {
                s.set_upgrade_context(
                    listener.as_raw_fd(),
                    dir.path().join("phux.sock"),
                    RuntimeFlags {
                        upgrade_source_exe: Some(dir.path().join("missing-replacement")),
                        ..RuntimeFlags::default()
                    },
                );
                s.arm_hub_reload(Some(source), cancel.clone());
            });
            crate::hub::reload_satellites(&state);
            assert!(state.with(crate::state::ServerState::hub_has_links));

            registry.lock().unwrap().extend([
                satellite("gopher", "gopher://box"),
                satellite("box", "ssh://b"),
            ]);
            let Err(UpgradeError::HubRegistry(reason)) = prepare_upgrade(&state).await else {
                panic!("an invalid registry must refuse the upgrade");
            };
            assert!(!reason.is_empty());

            // A valid registry gets past the check to the next stage.
            registry.lock().unwrap().truncate(1);
            assert!(matches!(
                prepare_upgrade(&state).await,
                Err(UpgradeError::Io(_))
            ));
            cancel.cancel();
        })
        .await;
}

#[test]
fn descriptor_guard_restores_flags_after_partial_prepare_failure() {
    let file = tempfile::tempfile().unwrap();
    let fd = file.as_raw_fd();
    let original = fd_flags(fd).union(rustix::io::FdFlags::CLOEXEC);
    set_fd_flags(fd, original);

    let mut guard = FdFlagsGuard::new();
    guard.clear_cloexec(fd).unwrap();
    assert!(!fd_flags(fd).contains(rustix::io::FdFlags::CLOEXEC));
    // A dropped tempfile's number is reused by whichever parallel test
    // opens next, so probe a descriptor no test can hold instead.
    assert!(guard.clear_cloexec(RawFd::MAX).is_err());
    drop(guard);

    assert_eq!(fd_flags(fd), original);
}

#[tokio::test]
async fn exec_failure_restores_original_descriptor_flags() {
    let listener = tempfile::tempfile().unwrap();
    let listener_fd = listener.as_raw_fd();
    let original = fd_flags(listener_fd).union(rustix::io::FdFlags::CLOEXEC);
    set_fd_flags(listener_fd, original);

    let blob_file = tempfile::tempfile().unwrap();
    let blob_fd = blob_file.as_raw_fd();
    let mut guard = FdFlagsGuard::new();
    guard.clear_cloexec(blob_fd).unwrap();
    guard.clear_cloexec(listener_fd).unwrap();
    let plan = UpgradePlan {
        executable: PinnedExecutable {
            path: PathBuf::from("/definitely/missing/phux"),
            source_path: PathBuf::from("/definitely/missing/phux"),
            dir: tempfile::tempdir().unwrap(),
        },
        blob_fd,
        socket_path: PathBuf::from("/tmp/phux.sock"),
        flags: RuntimeFlags::default(),
        fd_flags: guard,
        _blob_file: blob_file,
        _listener_fd: tempfile::tempfile().unwrap().into(),
        _delegated_listeners: Vec::new(),
        _handoffs: HashMap::new(),
        streams: crate::runtime::apps::agent_session::upgrade::AgentSessionUpgrade::holding(
            crate::runtime::apps::agent_session::upgrade::AgentSessionSeal {
                bootstraps: Vec::new(),
            },
        ),
    };

    assert_eq!(plan.exec().await.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(fd_flags(listener_fd), original);
}

/// Re-exec lands on the installed (allowlistable) path while it still
/// matches the validated snapshot, and aborts once the install is
/// swapped; validation always sees the snapshot.
#[test]
fn upgrade_execs_the_installed_path_only_while_it_matches_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("phux");
    install("/usr/bin/true", &path);
    let pinned = PinnedExecutable::open(&path).unwrap();
    assert_ne!(pinned.path, pinned.source_path);
    assert_eq!(pinned.exec_path().unwrap(), pinned.source_path.as_path());

    let replacement = dir.path().join("replacement");
    install("/usr/bin/false", &replacement);
    std::fs::rename(&replacement, &path).unwrap();
    validate_binary(&pinned.path).expect("the pinned image remains the validated one");
    assert!(validate_binary(&path).is_err());
    assert!(
        pinned.exec_path().is_err(),
        "a swapped install path must abort rather than re-exec the tempfile"
    );
}

/// A resumed image whose predecessor handed down `<path> (deleted)`
/// still pins the installed `<path>`.
#[test]
fn an_inherited_deleted_source_pins_the_installed_path() {
    let dir = tempfile::tempdir().unwrap();
    let installed = write_stub_phux(dir.path(), "#!/bin/sh\nexit 0\n");
    let inherited = dir.path().join("phux (deleted)");
    let pinned = pin_validated_executable(Some(&inherited)).unwrap();
    assert_eq!(pinned.source_path, installed);
}

/// `config check` runs before the irreversible exec: a replacement that
/// cannot load this host's config aborts with the loader's diagnostic.
#[test]
fn validate_binary_requires_version_and_config_check() {
    let dir = tempfile::tempdir().unwrap();
    let good = write_stub_phux(
        dir.path(),
        "#!/bin/sh\ncase \"$1\" in\n--version|config) exit 0 ;;\n*) exit 1 ;;\nesac\n",
    );
    validate_binary(&good).expect("a coherent replacement image must pass");

    let bad = write_stub_phux(
        dir.path(),
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --version) echo phux 0; exit 0 ;;\n\
         config) echo 'extends layer missing' >&2; exit 1 ;;\n\
         *) exit 0 ;;\n\
         esac\n",
    );
    let err = validate_binary(&bad).expect_err("a broken config must abort the upgrade");
    let message = err.to_string();
    assert!(
        message.contains("config check"),
        "validation error must name the probe: {message}"
    );
    assert!(
        message.contains("extends layer missing"),
        "validation error must carry the loader diagnostic: {message}"
    );
}

#[test]
fn a_production_server_never_hot_swaps_into_a_dev_build() {
    use BuildKind::{Dev, Local, Release};
    for current in [Release, Local] {
        let refusal = upgrade_kind_refusal(current, Some(Dev))
            .expect("a dev build must never replace a production server");
        assert!(refusal.contains("a dev build"), "{refusal}");
    }
    // A stamped release only ever becomes another stamped release: a
    // copied-out local `--release` build or a pre-probe binary is refused.
    assert!(upgrade_kind_refusal(Release, Some(Local)).is_some());
    assert!(upgrade_kind_refusal(Release, None).is_some());
    assert!(upgrade_kind_refusal(Release, Some(Release)).is_none());
    // An unstamped server can move onto a release; dev servers are free.
    assert!(upgrade_kind_refusal(Local, Some(Release)).is_none());
    assert!(upgrade_kind_refusal(Local, None).is_none());
    assert!(upgrade_kind_refusal(Dev, Some(Release)).is_none());
    assert!(upgrade_kind_refusal(Dev, Some(Dev)).is_none());
}

#[test]
fn validate_binary_accepts_when_version_and_config_check_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_stub_phux(
        dir.path(),
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --version|config) exit 0 ;;\n\
         *) exit 1 ;;\n\
         esac\n",
    );
    validate_binary(&path).expect("a coherent replacement image must pass");
}

/// The candidate's verdict on the staged blob decides the upgrade, and the
/// file comes back rewound for the real resume either way. A candidate that
/// predates the check answers `--version`, which must not block an upgrade.
#[test]
fn the_candidate_vets_the_blob_before_the_irreversible_exec() {
    use std::io::{Read as _, Seek as _, Write as _};

    let blob = |bytes: &[u8]| {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        file
    };
    let dir = tempfile::tempdir().unwrap();

    let refuses = write_stub_phux(
        dir.path(),
        "#!/bin/sh\ncat >/dev/null\necho 'unsupported upgrade blob version 2' >&2\nexit 1\n",
    );
    let error = preflight_blob(&refuses, &mut blob(b"{}")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported upgrade blob version 2"),
        "{error}"
    );

    let older = write_stub_phux(dir.path(), "#!/bin/sh\necho 'phux 0.115.0'\n");
    preflight_blob(&older, &mut blob(b"{}")).expect("an older candidate is not refused");

    let reads = write_stub_phux(
        dir.path(),
        "#!/bin/sh\ncat >/dev/null\necho upgrade-blob-readable\n",
    );
    let mut staged = blob(b"staged");
    preflight_blob(&reads, &mut staged).expect("readable");
    let mut left = String::new();
    staged.read_to_string(&mut left).unwrap();
    assert_eq!(left, "staged", "the resume must read from the start");
}

/// A tree pane whose engine exited but is not reaped yet aborts as
/// retryable, never staging a blob that would resurrect it as a blank
/// pane in the new image.
#[tokio::test]
async fn pane_handoff_with_a_closed_mailbox_aborts_for_retry() {
    let (dead, receiver) = mpsc::channel(1);
    drop(receiver);
    let mut state = crate::state::ServerState::new();
    let (_, dead_pane) = state.seed_session("s");

    let err = collect_pane_handoffs(vec![(dead_pane, dead)], Duration::from_secs(1))
        .await
        .expect_err("an unreaped dead pane must not be staged");

    assert!(matches!(err, UpgradeError::PaneExiting { pane } if pane == dead_pane));
    assert!(
        err.to_string().contains("retry the upgrade"),
        "the user must be told to retry: {err}"
    );
}

/// A live actor that accepted the request but never answered still
/// aborts: its PTY may be alive and must not be stranded.
#[tokio::test]
async fn pane_handoff_aborts_when_a_live_actor_drops_its_reply() {
    let (upgrade, mut receiver) = mpsc::channel::<UpgradeHandleRequest>(1);
    tokio::spawn(async move {
        drop(receiver.recv().await);
    });

    let result = request_pane_handoff(phux_core::ids::ResourceId::default(), &upgrade).await;

    assert!(matches!(
        result,
        Err(UpgradeError::PaneHandoff {
            reason: "actor dropped its reply",
            ..
        })
    ));
}
