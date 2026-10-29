//! Unit tests for finding and asking the desktop dev socket.

use std::os::unix::net::UnixListener;

use super::*;

#[test]
fn dev_socket_names() {
    assert!(is_dev_socket(Path::new("/r/desktop-42.dev.sock")));
    assert!(!is_dev_socket(Path::new("/r/default.sock")));
    assert!(!is_dev_socket(Path::new("/r/desktop-42.sock")));
}

#[test]
fn finds_only_a_live_desktop_and_reads_its_reply() {
    let dir = std::env::temp_dir().join(format!("phux-perf-desktop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    // A stale socket file nobody listens on is skipped.
    let dead = dir.join("desktop-1.dev.sock");
    drop(UnixListener::bind(&dead).unwrap_or_else(|e| panic!("{e}")));
    // A child another test is spawning holds the listener until it execs,
    // so wait for the socket to refuse, as a killed desktop's does.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&dead).is_ok() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let serving = dir.join("desktop-2.dev.sock");
    let listener = UnixListener::bind(&serving).unwrap_or_else(|e| panic!("{e}"));
    let server_socket = dir.join("server.sock");
    let server = std::thread::spawn(move || {
        // Liveness probe, identity handshake, then the request.
        for (n, stream) in listener.incoming().take(3).enumerate() {
            let stream = stream.unwrap_or_else(|e| panic!("{e}"));
            if n == 0 {
                continue;
            }
            let mut line = String::new();
            BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap_or_else(|e| panic!("{e}"));
            let mut writer = &stream;
            if n == 1 {
                writeln!(
                    writer,
                    "{}",
                    json!({ "ok": true, "server_socket": server_socket })
                )
                .unwrap_or_else(|e| panic!("{e}"));
                continue;
            }
            writeln!(writer, r#"{{"ok":false,"error":"no thanks"}}"#)
                .unwrap_or_else(|e| panic!("{e}"));
        }
    });
    let found = find(&dir.join("server.sock")).unwrap_or_else(|r| panic!("{}", r.err.message));
    assert_eq!(found, serving);
    assert!(
        !dir.join("desktop-1.dev.sock").exists(),
        "the dead desktop's socket is pruned"
    );
    let refused = ask(&found, &json!({ "op": "perf" }), Duration::from_secs(5))
        .err()
        .unwrap_or_else(|| panic!("an ok:false reply is a refusal"));
    assert_eq!(refused.err.code, "desktop_refused");
    assert_eq!(refused.err.message, "no thanks");
    server.join().unwrap_or_else(|_| panic!("server thread"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_selects_the_matching_server_not_the_newest_socket() {
    let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
    let mut workers = Vec::new();
    for (name, server) in [
        ("desktop-1.dev.sock", "a.sock"),
        ("desktop-2.dev.sock", "b.sock"),
    ] {
        let listener = UnixListener::bind(dir.path().join(name)).unwrap_or_else(|e| panic!("{e}"));
        let server = dir.path().join(server);
        workers.push(std::thread::spawn(move || {
            for (n, stream) in listener.incoming().take(2).enumerate() {
                let stream = stream.unwrap_or_else(|e| panic!("{e}"));
                if n == 0 {
                    continue;
                }
                let mut line = String::new();
                BufReader::new(&stream)
                    .read_line(&mut line)
                    .unwrap_or_else(|e| panic!("{e}"));
                writeln!(&stream, "{}", json!({"ok": true, "server_socket": server}))
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        }));
    }
    assert_eq!(
        find(&dir.path().join("a.sock")).unwrap_or_else(|r| panic!("{}", r.err.message)),
        dir.path().join("desktop-1.dev.sock")
    );
    for worker in workers {
        worker.join().unwrap_or_else(|_| panic!("worker"));
    }
}

#[test]
fn invalid_watch_does_not_connect_or_reset() {
    let opts = PerfOptions {
        json: true,
        desktop: true,
        watch: Some(f64::NAN),
        reset: true,
    };
    let error = report(&opts, Path::new("/nonexistent/dev.sock")).expect_err("invalid watch");
    assert_eq!(error.err.code, "invalid_argument");
}

#[test]
fn every_reset_sample_requests_snapshot_then_reset_and_keeps_interval_metrics() {
    let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("desktop.sock");
    let listener = UnixListener::bind(&path).unwrap_or_else(|e| panic!("{e}"));
    let worker = std::thread::spawn(move || {
        for (stream, count) in listener.incoming().take(2).zip([9, 4]) {
            let stream = stream.unwrap_or_else(|e| panic!("{e}"));
            let mut line = String::new();
            BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap_or_else(|e| panic!("{e}"));
            let request: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(request, json!({"op": "perf", "reset": true}));
            let mut report = phux_perf::snapshot("desktop", &[], Duration::ZERO);
            report.metrics.push(phux_perf::MetricSnapshot {
                name: "test.count".to_owned(),
                unit: phux_perf::Unit::Count,
                value: phux_perf::MetricValue::Counter(count),
            });
            writeln!(
                &stream,
                "{}",
                json!({"ok": true, "report": report, "frames": {}})
            )
            .unwrap_or_else(|e| panic!("{e}"));
        }
    });
    let (before, _) = read_report(&path, true).unwrap_or_else(|r| panic!("{}", r.err.message));
    let (now, _) = read_report(&path, true).unwrap_or_else(|r| panic!("{}", r.err.message));
    assert_eq!(before.metrics[0].value, phux_perf::MetricValue::Counter(9));
    assert_eq!(
        interval_report(&now, &before, true).metrics[0].value,
        phux_perf::MetricValue::Counter(4)
    );
    worker.join().unwrap_or_else(|_| panic!("worker"));
}

#[test]
fn oversized_desktop_reply_is_refused_without_unbounded_buffering() {
    let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("desktop.sock");
    let listener = UnixListener::bind(&path).unwrap_or_else(|e| panic!("{e}"));
    let worker = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap_or_else(|e| panic!("{e}"));
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = (&stream).write_all(&vec![
            b'x';
            usize::try_from(MAX_REPLY_BYTES + 1)
                .unwrap_or_default()
        ]);
    });
    let error =
        ask(&path, &json!({"op": "perf"}), Duration::from_secs(5)).expect_err("oversized reply");
    assert_eq!(error.err.code, "desktop_protocol");
    assert!(error.err.message.contains("8 MiB"));
    worker.join().unwrap_or_else(|_| panic!("worker"));
}
