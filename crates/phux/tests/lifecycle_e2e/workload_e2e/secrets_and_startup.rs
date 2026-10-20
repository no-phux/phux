/// Run `phux ARGS` on a PTY with the hermetic environment until one of
/// `needles` appears (or 30 s pass), then kill it; what it printed.
fn run_in_pty(dir: &Path, args: &[&str], needles: &[&str]) -> String {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read as _;
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(crate::runner::phux_bin());
    cmd.args(args);
    cmd.env_clear();
    for var in ["PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(var) {
            cmd.env(var, value);
        }
    }
    for (name, value) in hermetic_env(dir) {
        cmd.env(name, value);
    }
    cmd.env("HOME", dir);
    cmd.env("TERM", "xterm-256color");
    cmd.env("PHUX_SOCKET", dir.join("client.sock"));
    cmd.env("PHUX_NO_AUTO_LISTEN", "1");
    cmd.env("PHUX_NO_UPDATE_CHECK", "1");
    cmd.cwd(dir);
    let mut child = pty.slave.spawn_command(cmd).expect("spawn phux");
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().expect("clone reader");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + READY_DEADLINE;
    let mut seen = String::new();
    while !needles.iter().any(|needle| seen.contains(needle)) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(chunk) => seen.push_str(&String::from_utf8_lossy(&chunk)),
            Err(_) => break,
        }
    }
    // Let the line that matched finish.
    std::thread::sleep(Duration::from_millis(200));
    while let Ok(chunk) = rx.try_recv() {
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    let _ = child.kill();
    let _ = child.wait();
    seen
}

/// `workload-auth.md` §8/§9 secret sweep across an enrolled lifecycle,
/// revocation included: the private key never reaches argv, environment,
/// stdout, stderr, or the server's trace log.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn no_key_nonce_or_signature_bytes_in_argv_env_stdout_stderr_or_trace() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    init_authority(dir.path());
    let client = client_key();
    let needles = key_needles(&client.key_pem);
    let cert = dir.path().join("client.pem");
    let key = dir.path().join("client.key");
    std::fs::write(&key, &client.key_pem).expect("write client key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let id = enroll(dir.path(), &client, &cert, &["*@global"]);

    let log = dir.path().join("server.log");
    let stderr = std::fs::File::create(dir.path().join("server.stderr")).expect("stderr file");
    let trace =
        "phux=trace,phux_server=trace,phux_dial=trace,phux_protocol=trace,phux_client=trace,info";
    let server = server_command(
        dir.path(),
        &[
            "--quic",
            listeners::LOOPBACK_ANY_PORT,
            "--exit-after-idle",
            "120",
        ],
        &[
            ("RUST_LOG", trace),
            ("PHUX_LOG", log.to_str().expect("utf-8 path")),
        ],
    )
    .stdout(Stdio::null())
    .stderr(stderr)
    .spawn()
    .expect("spawn phux server");
    let server = Server(server);
    register_loopback_remote(dir.path());

    let identity = [
        ("PHUX_WORKLOAD_CERT", cert.as_path()),
        ("PHUX_WORKLOAD_KEY", key.as_path()),
    ];
    let whoami = ["whoami", "--remote", REMOTE, "--json"];
    let mut outputs = vec![await_success(dir.path(), &whoami, &identity)];
    outputs.push(phux(dir.path(), &["workload", "revoke", &id]));
    outputs.push(phux_with(dir.path(), &whoami, b"", &identity));
    outputs.push(phux(dir.path(), &["workload", "list", "--json"]));
    drop(server);

    for out in &outputs {
        assert_no_key_bytes(out, &needles);
    }
    // The key is named by path on every invocation, never carried.
    let passed: Vec<String> = whoami
        .iter()
        .map(|arg| (*arg).to_owned())
        .chain(identity.iter().map(|(_, path)| path.display().to_string()))
        .collect();
    for needle in &needles {
        assert!(
            !passed.iter().any(|value| value.contains(needle.as_str())),
            "key bytes reached argv or the environment"
        );
    }
    let mut traced = std::fs::read_to_string(dir.path().join("server.stderr")).unwrap_or_default();
    traced.push_str(&std::fs::read_to_string(&log).unwrap_or_default());
    assert!(
        traced.contains("TRACE") || traced.contains("DEBUG"),
        "the server's trace log was captured"
    );
    for needle in &needles {
        assert!(
            !traced.contains(needle.as_str()),
            "key bytes reached the server's trace log"
        );
    }
}

/// Workload mode starts beside a WebTransport listener (ADR-0154 item 6):
/// the listener admits only an end-to-end session presenting an enrolled
/// certificate, which a browser cannot open yet.
#[test]
#[ignore = "runs a real server start; runs in the e2e lane"]
fn workload_mode_starts_with_webtransport_and_reports_it_bound() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    init_authority(dir.path());
    let _server = start_server(
        dir.path(),
        &["--webtransport", listeners::LOOPBACK_ANY_PORT],
        &[],
    );
    let bound = listeners::bound_listener_addr(
        &dir.path().join("s.sock"),
        listeners::RemoteListenerTransport::Wt,
    );
    assert_ne!(bound.port(), 0, "the WebTransport listener is bound");
}

/// A named workload CA or registry location with no `[policy] mode` is a
/// startup error, not a silent owner grant (workload-auth §8): the operator
/// pointed at workload authority without choosing to enforce it.
#[test]
#[ignore = "runs a real server start; runs in the e2e lane"]
fn an_unset_mode_beside_a_named_workload_registry_refuses_to_start() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    let registry = dir.path().join("elsewhere-workload-keys");
    let registry = registry.to_str().expect("utf-8 tempdir");
    let out = server_command(
        dir.path(),
        &["--exit-after-idle", "5"],
        &[("PHUX_WORKLOAD_KEYS", registry)],
    )
    .output()
    .expect("run phux server");
    let stderr = text(&out.stderr);
    assert!(
        !out.status.success(),
        "an unset mode beside a named registry must refuse: {stderr}"
    );
    assert!(
        stderr.contains("[policy] mode") && stderr.contains("PHUX_WORKLOAD_KEYS"),
        "the refusal names the setting and the remedy: {stderr}"
    );
}

/// No `[policy] mode` beside a remote listener is a startup error naming the
/// remedy (ADR-0179): the transitional owner grant for remote consumers is
/// gone, so nothing would authorize what the listener admits.
#[test]
#[ignore = "runs a real server start; runs in the e2e lane"]
fn an_unset_mode_beside_a_remote_listener_refuses_to_start() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    let out = server_command(
        dir.path(),
        &[
            "--quic",
            listeners::LOOPBACK_ANY_PORT,
            "--exit-after-idle",
            "5",
        ],
        &[],
    )
    .output()
    .expect("run phux server");
    let stderr = text(&out.stderr);
    assert!(
        !out.status.success(),
        "an unset mode beside a listener must refuse: {stderr}"
    );
    assert!(
        stderr.contains("[policy] mode") && stderr.contains("paired"),
        "the refusal names the setting and the remedy: {stderr}"
    );
}

/// The retired `PHUX_WORKLOAD_MTLS` refuses to start, naming its
/// replacement, rather than quietly choosing a mode (ADR-0179).
#[test]
#[ignore = "runs a real server start; runs in the e2e lane"]
fn the_retired_workload_mtls_variable_refuses_to_start() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    let out = server_command(
        dir.path(),
        &["--exit-after-idle", "5"],
        &[("PHUX_WORKLOAD_MTLS", "1")],
    )
    .output()
    .expect("run phux server");
    let stderr = text(&out.stderr);
    assert!(
        !out.status.success(),
        "the retired variable must refuse: {stderr}"
    );
    assert!(
        stderr.contains("PHUX_WORKLOAD_MTLS is retired") && stderr.contains("[policy] mode"),
        "the refusal names the replacement: {stderr}"
    );
}

/// Poll `child` until it exits or `deadline` passes, then collect it.
fn wait_exit(mut child: Child, deadline: Duration) -> Output {
    let start = Instant::now();
    while child.try_wait().expect("poll child").is_none() {
        if start.elapsed() > deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("collect child")
}
