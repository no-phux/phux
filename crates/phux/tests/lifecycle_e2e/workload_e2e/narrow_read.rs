/// Inventory of this host, with no OBSERVE or INPUT: the runbook's CI scope.
const INVENTORY_HOST_SCOPE: &str = "inventory@host";

/// The CI certificate lists a real session, but cannot read its real terminal
/// (including scrollback), type into it, or download files.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn inventory_host_key_lists_and_refuses_terminal_observe() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    init_authority(dir.path());
    let credential = HostReadKey::enroll_scoped(
        dir.path(),
        "inventory-client",
        None,
        &[INVENTORY_HOST_SCOPE],
        "automation",
        HOST_READ_EXPIRES_IN,
    );
    let verbs: Vec<&str> = INVENTORY_HOST_SCOPE.split([',', '@']).collect();
    assert!(!verbs.contains(&"observe"), "{INVENTORY_HOST_SCOPE}");
    assert!(!verbs.contains(&"input"), "{INVENTORY_HOST_SCOPE}");

    let _server = start_server(dir.path(), &["--quic", listeners::LOOPBACK_ANY_PORT], &[]);
    register_loopback_remote(dir.path());
    let socket = dir.path().join("s.sock");
    let socket_arg = socket.to_str().expect("utf-8 socket");
    let terminal = create_read_terminal(dir.path(), socket_arg, "observe-list");
    let identity = credential.identity();
    let listed = await_remote_listing(dir.path(), socket_arg, &identity);
    assert_remote_lists_session(&listed, "observe-list");
    assert_download_refused(dir.path(), &identity);

    let mut live =
        RawSession::open_cert(bound_quic_port(&socket), &credential.cert, &credential.key);
    live.await_hello_ok();
    assert_terminal_observe_refused(&mut live, &terminal);
    assert_terminal_input_refused(&mut live, &terminal);
}

/// The member certificate lists both sessions but reads only the one Patrick
/// explicitly opts in. The same certificate cannot read the unshared terminal
/// or type into either terminal, even though both are present in inventory.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn session_observe_key_reads_only_opted_in_session() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    init_authority(dir.path());
    let _server = start_server(dir.path(), &["--quic", listeners::LOOPBACK_ANY_PORT], &[]);
    register_loopback_remote(dir.path());
    let socket = dir.path().join("s.sock");
    let socket_arg = socket.to_str().expect("utf-8 socket");
    let shared = create_read_terminal(dir.path(), socket_arg, "shared");
    let unshared = create_read_terminal(dir.path(), socket_arg, "unshared");
    assert_ne!(shared, unshared);
    let observe = format!(
        "observe@session:{}",
        owner_session_key(dir.path(), socket_arg, "shared")
    );
    let credential = HostReadKey::enroll_scoped(
        dir.path(),
        "member-client",
        None,
        &[INVENTORY_HOST_SCOPE, &observe],
        "human",
        30 * 24 * 60 * 60,
    );
    let listed = await_remote_listing(dir.path(), socket_arg, &credential.identity());
    assert_remote_lists_session(&listed, "shared");
    assert_remote_lists_session(&listed, "unshared");

    let mut live =
        RawSession::open_cert(bound_quic_port(&socket), &credential.cert, &credential.key);
    live.await_hello_ok();
    assert_terminal_observed(&mut live, &shared);
    assert_terminal_observe_refused(&mut live, &unshared);
    assert_terminal_input_refused(&mut live, &shared);
    assert_terminal_input_refused(&mut live, &unshared);
}

/// Read the full organization key exactly as the runbook directs, on the owner
/// socket, not from a short human selector or a fabricated session identity.
fn owner_session_key(dir: &Path, socket: &str, name: &str) -> String {
    let out = phux(dir, &["--socket", socket, "ls", "--json"]);
    assert!(out.status.success(), "owner listing: {}", text(&out.stderr));
    let doc = json_doc(&out);
    doc["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["name"] == name)
        .expect("the created session")["key"]
        .as_str()
        .expect("full session key")
        .to_owned()
}

/// The registered remote answered with the named session, not merely the owner
/// socket's inventory or an unreachable host placeholder.
fn assert_remote_lists_session(listed: &serde_json::Value, name: &str) {
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
            .any(|session| session["name"] == name),
        "ls --all through the scoped key: {remote}"
    );
}

/// `GET_SCREEN` must deny a real terminal even when inventory lists it. Ask for
/// scrollback too, so denial cannot silently expose historical output.
fn assert_terminal_observe_refused(
    live: &mut RawSession,
    terminal: &phux_protocol::ids::ResourceId,
) {
    use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};

    live.send_frame(&FrameKind::Command {
        request_id: 9,
        command: Command::GetScreen {
            terminal_id: terminal.clone(),
            request_scrollback: Some(0),
            cells: false,
            format: 0,
        },
    });
    let screen = live.await_command_result(9);
    assert!(
        matches!(
            screen,
            CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                ref message,
            } if message == "permission denied"
        ),
        "terminal OBSERVE: {screen:?}"
    );
}
