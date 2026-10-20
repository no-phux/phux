/// The host-scoped read ceiling a CI runner or another member holds.
///
/// `phux ls` is `GET_STATE { SERVER }`, which the kind catalog admits only
/// with `inventory` and then returns what that verb covers. `observe` is
/// what lets the same key read a terminal. `input` is absent on purpose.
const HOST_READ_SCOPE: &str = "inventory,observe@host";

/// Seven days, the suggested CI-runner lifetime.
const HOST_READ_EXPIRES_IN: i64 = 7 * 24 * 60 * 60;

/// `workload add-key --scope inventory,observe@host` on a paired loopback
/// listener: the certificate lists and observes a real terminal but cannot
/// type into it. Replacement closes its live connection and refuses its next
/// dial; the equally restricted successor works until explicitly revoked.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn host_scoped_read_key_lists_refuses_input_and_revoke_closes_it() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    init_authority(dir.path());
    let original = HostReadKey::enroll(dir.path(), "original", None);
    assert!(
        !HOST_READ_SCOPE
            .split([',', '@'])
            .any(|verb| verb == "input"),
        "{HOST_READ_SCOPE}"
    );

    let _server = start_server(dir.path(), &["--quic", listeners::LOOPBACK_ANY_PORT], &[]);
    register_loopback_remote(dir.path());
    let socket = dir.path().join("s.sock");
    let socket_arg = socket.to_str().expect("utf-8 socket");
    let terminal = create_read_terminal(dir.path(), socket_arg);

    let identity = original.identity();
    let listed = await_remote_listing(dir.path(), socket_arg, &identity);
    let remote = listed["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .find(|host| host["name"] == REMOTE)
        .expect("the registered host");
    assert_eq!(remote["reachable"], true, "{remote}");
    assert!(remote["error"].is_null(), "{remote}");
    assert!(
        remote["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|session| session["name"].as_str() == Some("observe-list")),
        "ls --all through the read key: {remote}"
    );

    assert_download_refused(dir.path(), &identity);

    let port = bound_quic_port(&socket);
    let mut live = RawSession::open_cert(port, &original.cert, &original.key);
    live.await_hello_ok();
    assert_terminal_observed(&mut live, &terminal);
    assert_terminal_input_refused(&mut live, &terminal);
    assert_read_key_replacement(dir.path(), &original, &mut live, &terminal);
}

/// A live terminal whose output is readable without INPUT or BIND.
fn create_read_terminal(dir: &Path, socket: &str) -> phux_protocol::ids::ResourceId {
    let created = phux(
        dir,
        &[
            "--socket",
            socket,
            "new",
            "--json",
            "-s",
            "observe-list",
            "--",
            "/bin/sh",
            "-c",
            "printf 'HOST_READ_READY\\n'; exec cat",
        ],
    );
    assert!(
        created.status.success(),
        "new terminal: {}",
        text(&created.stderr)
    );

    let id = json_doc(&created)["terminal_id"]
        .as_u64()
        .expect("terminal_id");
    phux_protocol::ids::ResourceId::local(u32::try_from(id).expect("local terminal id"))
}

/// File downloads require INPUT even though they do not type into a terminal.
fn assert_download_refused(dir: &Path, identity: &[(&str, &Path)]) {
    let dest = dir.join("not-written");
    let download = phux_with(
        dir,
        &[
            "download",
            "--remote",
            REMOTE,
            "--json",
            "--timeout",
            "15",
            "/etc/hosts",
            dest.to_str().expect("utf-8 dest"),
        ],
        b"",
        identity,
    );
    assert!(
        !download.status.success(),
        "INPUT was admitted: {}",
        text(&download.stdout)
    );
    let download_text = format!("{}{}", text(&download.stdout), text(&download.stderr));
    assert!(
        download_text.contains("permission denied"),
        "GET_FILE (INPUT) refusal: {download_text}"
    );
    assert!(!dest.exists(), "a refused download left a file");
}

/// The same real terminal that admitted OBSERVE must refuse INPUT.
fn assert_terminal_input_refused(live: &mut RawSession, terminal: &phux_protocol::ids::ResourceId) {
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};

    live.send_frame(&FrameKind::Command {
        request_id: 7,
        command: Command::RouteInput {
            terminal_id: terminal.clone(),
            event: InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"not-typed".to_vec(),
            }),
        },
    });
    let input = live.await_command_result(7);
    assert!(
        matches!(
            input,
            CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                ref message,
            } if message == "permission denied"
        ),
        "terminal INPUT: {input:?}"
    );
}

/// The restricted certificate really reads the terminal's output, not just inventory.
fn assert_terminal_observed(live: &mut RawSession, terminal: &phux_protocol::ids::ResourceId) {
    use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind};

    let start = Instant::now();
    loop {
        live.send_frame(&FrameKind::Command {
            request_id: 8,
            command: Command::GetScreen {
                terminal_id: terminal.clone(),
                request_scrollback: None,
                cells: false,
                format: 0,
            },
        });
        let result = live.await_command_result(8);
        assert!(
            matches!(&result, CommandResult::OkWith(CommandValue::Json(_))),
            "terminal OBSERVE: {result:?}"
        );
        if let CommandResult::OkWith(CommandValue::Json(json)) = result {
            let screen: phux_core::screen::ScreenState =
                serde_json::from_str(&json).expect("screen");
            if screen
                .lines
                .iter()
                .any(|line| line.contains("HOST_READ_READY"))
            {
                return;
            }
        }
        assert!(
            start.elapsed() < READY_DEADLINE,
            "terminal output never appeared"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `--replace` revokes the old live connection and its next dial, not its successor.
fn assert_read_key_replacement(
    dir: &Path,
    original: &HostReadKey,
    predecessor: &mut RawSession,
    terminal: &phux_protocol::ids::ResourceId,
) {
    let replacement = HostReadKey::enroll(dir, "replacement", Some(&original.id));
    assert_ne!(replacement.id, original.id);
    assert_revoked(predecessor);

    let port = bound_quic_port(&dir.join("s.sock"));
    let mut refused = RawSession::open_cert(port, &original.cert, &original.key);
    let frames = refused.frames_until_close();
    assert!(
        frames.is_empty(),
        "replaced key received frames: {frames:?}"
    );
    let reason = refused.close_reason();
    assert!(reason.contains("unauthorized"), "replaced key: {reason}");

    let mut current = RawSession::open_cert(port, &replacement.cert, &replacement.key);
    current.await_hello_ok();
    assert_terminal_observed(&mut current, terminal);
    assert_terminal_input_refused(&mut current, terminal);
    let revoked = phux(dir, &["workload", "revoke", &replacement.id]);
    assert!(revoked.status.success(), "{}", text(&revoked.stderr));
    assert_revoked(&mut current);
}

/// One sandbox-only identity, shared by the initial and replacement enrollments.
struct HostReadKey {
    cert: PathBuf,
    key: PathBuf,
    id: String,
}

impl HostReadKey {
    fn enroll(dir: &Path, name: &str, replaces: Option<&str>) -> Self {
        let client = client_key();
        let cert = dir.join(format!("{name}.pem"));
        let key = dir.join(format!("{name}.key"));
        std::fs::write(&key, &client.key_pem).expect("write client key");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let enrolled = enroll_host_read_key(dir, &client, &cert, replaces);
        let id = enrolled["credential_id"]
            .as_str()
            .expect("credential_id")
            .to_owned();
        assert_eq!(enrolled["principal"], "automation");
        assert_eq!(enrolled["scopes"], serde_json::json!([HOST_READ_SCOPE]));
        assert_eq!(enrolled["replaced"], replaces.is_some());
        let expires_at = enrolled["expires_at"].as_i64().expect("expires_at");
        let skew = (expires_at - (chrono::Utc::now().timestamp() + HOST_READ_EXPIRES_IN)).abs();
        assert!(
            skew < 120,
            "expires_at {expires_at} is not ~{HOST_READ_EXPIRES_IN}s out"
        );
        Self { cert, key, id }
    }

    fn identity(&self) -> [(&'static str, &Path); 2] {
        [
            ("PHUX_WORKLOAD_CERT", self.cert.as_path()),
            ("PHUX_WORKLOAD_KEY", self.key.as_path()),
        ]
    }
}

/// Enroll `client` at [`HOST_READ_SCOPE`] for [`HOST_READ_EXPIRES_IN`] seconds
/// as an automation, the flags an operator passes for a CI runner.
fn enroll_host_read_key(
    dir: &Path,
    client: &ClientKey,
    cert_out: &Path,
    replaces: Option<&str>,
) -> serde_json::Value {
    let expires = HOST_READ_EXPIRES_IN.to_string();
    let mut args = vec![
        "workload",
        "add-key",
        "--json",
        "--cert-out",
        cert_out.to_str().expect("utf-8 path"),
        "--scope",
        HOST_READ_SCOPE,
        "--expires-in",
        expires.as_str(),
        "--principal",
        "automation",
    ];
    if let Some(id) = replaces {
        args.extend(["--replace", id]);
    }
    let out = phux_with(dir, &args, client.csr_pem.as_bytes(), &[]);
    assert!(out.status.success(), "add-key: {}", text(&out.stderr));
    json_doc(&out)
}

/// `phux ls --all --json` once the registered host answers with its sessions.
fn await_remote_listing(dir: &Path, socket: &str, identity: &[(&str, &Path)]) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let out = phux_with(
            dir,
            &["--socket", socket, "ls", "--all", "--json"],
            b"",
            identity,
        );
        if out.status.success() {
            let doc = json_doc(&out);
            let ready = doc["hosts"].as_array().is_some_and(|hosts| {
                hosts
                    .iter()
                    .any(|host| host["name"] == REMOTE && host["reachable"] == true)
            });
            if ready {
                return doc;
            }
        }
        assert!(
            start.elapsed() < READY_DEADLINE,
            "`phux ls --all` never listed {REMOTE}: {}",
            text(&out.stderr) + &text(&out.stdout)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
