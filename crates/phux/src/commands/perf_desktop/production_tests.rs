use std::os::unix::net::UnixListener;

use super::*;

const PRODUCTION: &str = "/run/user/4294967294/phux/phux.sock";

#[test]
fn dev_socket_address_itself_is_guarded_before_connecting() {
    let error = ask(
        Path::new(PRODUCTION),
        &json!({"op": "identity"}),
        Duration::from_secs(1),
    )
    .expect_err("a production socket is not a dev endpoint");
    assert_eq!(error.err.code, "production_socket");
}

#[test]
fn selected_production_server_refuses_before_override_discovery() {
    let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("desktop.sock");
    let listener = UnixListener::bind(&path).unwrap_or_else(|e| panic!("{e}"));
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        phux_config::instance::build_kind(),
        phux_config::instance::BuildKind::Dev
    );
    let error =
        find_with_override(Path::new(PRODUCTION), Some(path)).expect_err("production is refused");
    assert_eq!(error.err.code, "production_socket");
    assert_eq!(
        listener
            .accept()
            .expect_err("no discovery connection")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
}

fn identity_server(
    identity: Value,
) -> (
    tempfile::TempDir,
    PathBuf,
    std::thread::JoinHandle<UnixListener>,
) {
    let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("desktop.sock");
    let listener = UnixListener::bind(&path).unwrap_or_else(|e| panic!("{e}"));
    let worker = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap_or_else(|e| panic!("{e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap_or_else(|e| panic!("{e}"));
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("{e}"));
        let request: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(request, json!({"op": "identity"}));
        writeln!(
            &stream,
            "{}",
            json!({"ok": true, "server_socket": identity})
        )
        .unwrap_or_else(|e| panic!("{e}"));
        listener
    });
    (dir, path, worker)
}

#[test]
fn override_actual_production_identity_refuses_before_any_operation() {
    let (dir, path, worker) = identity_server(json!(PRODUCTION));
    let error = find_with_override(&dir.path().join("dev-server.sock"), Some(path))
        .expect_err("actual production identity is refused");
    assert_eq!(error.err.code, "production_socket");
    let listener = worker.join().unwrap_or_else(|_| panic!("identity worker"));
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        listener
            .accept()
            .expect_err("no operation connection after refused identity")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn override_missing_identity_fails_closed() {
    let (dir, path, worker) = identity_server(Value::Null);
    let error = find_with_override(&dir.path().join("dev-server.sock"), Some(path))
        .expect_err("identity is required");
    assert_eq!(error.err.code, "desktop_protocol");
    worker.join().unwrap_or_else(|_| panic!("identity worker"));
}

#[test]
fn override_identity_must_match_the_selected_server() {
    let (dir, path, worker) = identity_server(json!("/nonexistent/other-dev-server.sock"));
    let error = find_with_override(&dir.path().join("dev-server.sock"), Some(path))
        .expect_err("identity must match");
    assert_eq!(error.err.code, "desktop_server_mismatch");
    worker.join().unwrap_or_else(|_| panic!("identity worker"));
}
