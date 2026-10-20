use super::run::{
    broken_config_message, build_server_config, log_startup, open_server_log, select_connectors,
};
use super::spawn::{SpawnLock, parse_auto_spawn_idle};
use super::*;

#[test]
fn ensure_document_is_an_availability_result_not_a_handshake_claim() {
    let socket = Path::new("/tmp/phux-contract.sock");
    let doc = ensure_document(socket, EnsureDisposition::SupervisedStarted);
    assert_eq!(doc["schema_version"], u64::from(ENSURE_SCHEMA_VERSION));
    assert_eq!(doc["running"], true);
    assert_eq!(doc["socket"], socket.display().to_string());
    assert_eq!(doc["disposition"], "supervised_started");
    assert_eq!(doc["cli_version"], env!("CARGO_PKG_VERSION"));
    assert!(doc["server_log"].is_string());
    assert!(doc.get("protocol").is_none());
    assert!(doc.get("server_version").is_none());
}

#[test]
fn spawn_lock_refuses_symlinks_and_fifos() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target");
    std::fs::write(&target, b"do not lock").expect("target");
    let symlink = dir.path().join("symlink.lock");
    std::os::unix::fs::symlink(&target, &symlink).expect("symlink");
    assert!(SpawnLock::acquire(&symlink).0.is_none());

    let fifo = dir.path().join("fifo.lock");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo must create the hostile fixture");
    assert!(SpawnLock::acquire(&fifo).0.is_none());
}

fn connector(relay: &str, token: &str) -> phux_config::ConnectorConfigEntry {
    phux_config::ConnectorConfigEntry {
        relay: relay.to_owned(),
        token_file: Some(PathBuf::from(token)),
        cert_fingerprint: Some("AB".to_owned()),
    }
}

/// The idle backstop accepts exactly what `--exit-after-idle` accepts.
#[test]
fn the_auto_spawn_idle_backstop_accepts_what_the_flag_accepts() {
    assert_eq!(parse_auto_spawn_idle("600"), Some(600));
    assert_eq!(parse_auto_spawn_idle("1"), Some(1));
    assert_eq!(parse_auto_spawn_idle("86400"), Some(86_400));

    assert_eq!(parse_auto_spawn_idle("0"), None);
    assert_eq!(parse_auto_spawn_idle("86401"), None);

    assert_eq!(parse_auto_spawn_idle(""), None);
    assert_eq!(parse_auto_spawn_idle("600s"), None);
    assert_eq!(parse_auto_spawn_idle("1.5"), None);
    assert_eq!(parse_auto_spawn_idle("-1"), None);
    assert_eq!(parse_auto_spawn_idle("forever"), None);
}

/// `metadata-value-bytes` is clamped up to the agent-session record floor and
/// passes through above it.
#[test]
fn metadata_value_bytes_is_clamped_to_the_agent_session_record_floor() {
    let floor = u32::try_from(phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES).unwrap();
    for (configured, expected) in [
        (0, floor),
        (
            phux_config::DEFAULT_METADATA_VALUE_BYTES,
            phux_config::DEFAULT_METADATA_VALUE_BYTES,
        ),
    ] {
        let cfg = build_server_config(
            None,
            Path::new("/tmp/phux-test.sock"),
            phux_config::DefaultsCfg::default(),
            phux_config::VoiceCfg::default(),
            &phux_config::LimitsCfg {
                metadata_value_bytes: configured,
            },
            phux_server::hooks::HookCatalog::default(),
            None,
            None,
        );
        assert_eq!(cfg.metadata_value_bytes, expected);
    }
}

#[test]
fn broken_config_message_names_path_error_and_remedy() {
    let msg = broken_config_message(
        Path::new("/home/u/.config/phux/config.toml"),
        &"config.toml: 3:14: expected `=` after key",
    );
    assert!(msg.contains("/home/u/.config/phux/config.toml"), "{msg}");
    assert!(
        msg.contains("config.toml: 3:14: expected `=` after key"),
        "{msg}"
    );
    assert!(msg.contains("run `phux config check`"), "{msg}");
}

/// The startup line carries pid, version, and socket and passes the default
/// filter into a file sink.
#[test]
#[allow(clippy::expect_used, reason = "test")]
fn startup_line_lands_in_file_at_default_filter() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("server.log");
    let file = std::fs::File::create(&path).expect("create sink");
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false);
    let subscriber = tracing_subscriber::registry()
            // Mirrors telemetry::DEFAULT_FILTER.
            .with(tracing_subscriber::EnvFilter::new("phux=info,warn"))
            .with(layer);
    tracing::subscriber::with_default(subscriber, || {
        log_startup(Path::new("/run/user/1000/phux/phux.sock"));
    });

    let contents = std::fs::read_to_string(&path).expect("read back log");
    assert!(
        contents.contains("phux server started"),
        "startup line filtered out at the default filter: {contents}"
    );
    assert!(
        contents.contains(&format!("pid={}", std::process::id())),
        "pid missing: {contents}"
    );
    assert!(
        contents.contains(&format!("version={}", env!("CARGO_PKG_VERSION"))),
        "version missing: {contents}"
    );
    assert!(
        contents.contains("socket=/run/user/1000/phux/phux.sock"),
        "socket missing: {contents}"
    );
}

/// The server log opens under a `0o700` parent at `0o600` (ADR-0028).
#[cfg(unix)]
#[test]
#[allow(clippy::expect_used, reason = "test")]
fn open_server_log_creates_0700_parent_and_0600_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("phux").join("server.log");
    let _file = open_server_log(&path).expect("open server log");

    let parent_mode = std::fs::metadata(path.parent().expect("parent"))
        .expect("parent metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(parent_mode, 0o700, "parent mode was {parent_mode:o}");
    let file_mode = std::fs::metadata(&path)
        .expect("file metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600, "log mode was {file_mode:o}");
}

#[test]
fn connectors_all_run_by_default_and_connect_selects_one() {
    let configured = vec![
        connector("one.example:4433", "/one"),
        connector("two.example:4433", "/two"),
    ];
    assert_eq!(select_connectors(configured.clone(), None), configured);
    assert_eq!(
        select_connectors(configured, Some("two.example:4433")),
        vec![connector("two.example:4433", "/two")]
    );

    // An unconfigured relay is allowed ad hoc, without credentials.
    let selected = select_connectors(Vec::new(), Some("127.0.0.1:4433"));
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].relay, "127.0.0.1:4433");
    assert!(selected[0].token_file.is_none());
    assert!(selected[0].cert_fingerprint.is_none());
}
