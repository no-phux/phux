//! The dev socket's requests, bounds, trust checks and lifecycle.

use super::*;

#[test]
fn unknown_and_malformed_requests_fail_politely() {
    let reply = handle(&json!({ "op": "explode" }));
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap_or_default().contains("help"));
    assert_eq!(handle(&json!({ "op": "help" }))["ok"], true);
    assert_eq!(handle(&json!({ "op": "trace", "seconds": 0 }))["ok"], false);
}

#[test]
fn trace_duration_caps_huge_values_before_conversion() {
    for seconds in [1e300, f64::MAX, 61.0] {
        assert_eq!(trace_duration(seconds), Some(MAX_TRACE));
    }
    assert_eq!(trace_duration(0.25), Some(Duration::from_millis(250)));
    for seconds in [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MIN_POSITIVE,
    ] {
        assert_eq!(trace_duration(seconds), None);
    }
}

#[test]
fn socket_refuses_untrusted_parents_and_checks_kernel_peer() {
    let dir = std::env::temp_dir().join(format!("phux-devsock-trust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(serve(&dir.join("t.sock")).is_err());
    assert!(!dir.join("t.sock").exists());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|e| panic!("{e}"));
    let link = dir.with_extension("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap_or_else(|e| panic!("{e}"));
    assert!(serve(&link.join("t.sock")).is_err());
    let (a, b) = UnixStream::pair().unwrap_or_else(|e| panic!("{e}"));
    assert!(owner_peer(&a));
    assert!(owner_peer(&b));
    let _ = std::fs::remove_file(link);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn socket_requests_are_bounded_and_abandoned_reads_expire() {
    let (mut client, stream) = UnixStream::pair().unwrap_or_else(|e| panic!("{e}"));
    let worker = std::thread::spawn(move || serve_connection(stream));
    client
        .write_all(&vec![b'x'; MAX_REQUEST_BYTES + 1])
        .unwrap_or_else(|e| panic!("{e}"));
    let mut reply = String::new();
    BufReader::new(client)
        .read_line(&mut reply)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(reply.contains("exceeds 65536 bytes"), "{reply}");
    worker.join().unwrap_or_else(|_| panic!("worker"));
    let (_client, stream) = UnixStream::pair().unwrap_or_else(|e| panic!("{e}"));
    let error = read_request(&mut BufReader::new(stream), Duration::from_millis(20))
        .expect_err("an abandoned request expires");
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    let mut permits = Vec::new();
    while let Some(permit) = ConnectionPermit::take() {
        permits.push(permit);
    }
    assert!(permits.len() <= MAX_CONNECTIONS);
    assert!(ConnectionPermit::take().is_none());
    drop(permits);
    assert!(ConnectionPermit::take().is_some());
}

#[test]
fn perf_reset_returns_the_snapshot_before_clearing_metrics() {
    crate::diag::DRAIN_WAKES.add(100_000);
    let reply = handle(&json!({"op": "perf", "reset": true}));
    let metrics = reply["report"]["metrics"]
        .as_array()
        .unwrap_or_else(|| panic!("metrics"));
    let wakes = metrics
        .iter()
        .find(|row| row["name"] == "desktop.drain_wakes")
        .unwrap_or_else(|| panic!("wakes"));
    assert!(wakes["value"].as_u64().unwrap_or_default() >= 100_000);
    assert!(crate::diag::DRAIN_WAKES.get() < 100_000);
}

#[test]
fn perf_and_frames_answer_over_a_real_socket() {
    let dir = std::env::temp_dir().join(format!("phux-devsock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|e| panic!("{e}"));
    let path = serve(&dir.join("t.sock")).unwrap_or_else(|e| panic!("{e}"));
    let mode = std::fs::metadata(&path)
        .unwrap_or_else(|e| panic!("{e}"))
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    drop(crate::devtools::frames::FrameTimer::start());
    let stream = UnixStream::connect(&path).unwrap_or_else(|e| panic!("{e}"));
    let mut writer = stream.try_clone().unwrap_or_else(|e| panic!("{e}"));
    let mut lines = BufReader::new(stream).lines();
    let mut ask = |request: Value| -> Value {
        writeln!(writer, "{request}").unwrap_or_else(|e| panic!("{e}"));
        let line = lines
            .next()
            .unwrap_or_else(|| panic!("no reply"))
            .unwrap_or_else(|e| panic!("{e}"));
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}"))
    };
    let perf = ask(json!({ "op": "perf" }));
    assert_eq!(perf["ok"], true);
    assert!(
        perf["report"]["metrics"]
            .as_array()
            .is_some_and(|m| !m.is_empty())
    );
    assert!(perf["frames"]["fps"].is_number());
    let frames = ask(json!({ "op": "frames", "n": 5 }));
    assert!(frames["frames"].as_array().is_some_and(|f| !f.is_empty()));
    assert!(serve(&path).is_err(), "a live socket is never replaced");
    let file = dir.join("not-a-socket");
    std::fs::write(&file, "keep").unwrap_or_else(|e| panic!("{e}"));
    assert!(serve(&file).is_err());
    assert!(file.exists(), "a non-socket at the path is left alone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stopped_socket_goes_away_and_can_serve_again() {
    let dir = std::env::temp_dir().join(format!("phux-devsock-stop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|e| panic!("{e}"));
    let path = serve(&dir.join("t.sock")).unwrap_or_else(|e| panic!("{e}"));
    stop(&path);
    assert!(!path.exists(), "stopping removes the socket");
    assert!(UnixStream::connect(&path).is_err());
    assert!(listeners().iter().all(|(bound, _)| bound != &path));
    stop(&path);
    let again = serve(&path).unwrap_or_else(|e| panic!("{e}"));
    let mut stream = UnixStream::connect(&again).unwrap_or_else(|e| panic!("{e}"));
    writeln!(stream, "{}", json!({ "op": "help" })).unwrap_or_else(|e| panic!("{e}"));
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(reply.contains("\"ok\":true"), "{reply}");
    stop(&again);
    let _ = std::fs::remove_dir_all(&dir);
}
