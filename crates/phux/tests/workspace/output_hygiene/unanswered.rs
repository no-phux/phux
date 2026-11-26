/// A socket that accepts every connection and never writes a byte: the
/// field repro for a CLI that waited forever on `HELLO` (#2786).
fn silent_socket(dir: &std::path::Path) -> String {
    let path = dir.join("silent.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind silent socket");
    std::thread::spawn(move || {
        // Hold every accepted stream open, unread, until the test exits.
        let held: Vec<_> = listener.incoming().map_while(Result::ok).collect();
        drop(held);
    });
    path.to_str().expect("utf-8 temp path").to_owned()
}

/// A server that accepts and never answers fails inside `PHUX_TIMEOUT`, with
/// exit 4 and a message naming the socket and the duration, in both output
/// modes; the JSON document carries `code: timeout`.
#[test]
fn a_server_that_never_answers_times_out_with_exit_four() {
    let tmp = TempDir::new().expect("tempdir");
    let sock = &silent_socket(tmp.path());
    for args in [
        &["ls", "--socket", sock][..],
        &["ls", "--json", "--socket", sock],
        &["snapshot", "--json", "work", "--socket", sock],
        &["forward", "-L", "0:localhost:80", "--socket", sock],
    ] {
        let started = std::time::Instant::now();
        let (code, stdout, stderr) =
            Out::of(phux().env("PHUX_TIMEOUT", "1").args(args)).into_parts();
        let elapsed = started.elapsed();
        assert_eq!(code, 4, "{args:?}: stderr={stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(8),
            "{args:?} took {elapsed:?}"
        );
        assert!(
            stderr.contains(sock.as_str()) && stderr.contains("timed out after 1s"),
            "{args:?}: the message names the socket and the deadline: {stderr}"
        );
        if args.contains(&"--json") {
            let doc: serde_json::Value =
                serde_json::from_str(stderr.trim()).expect("one JSON line");
            assert_eq!(doc["error"]["code"], "timeout", "{doc}");
            assert_eq!(doc["exit_code"], 4, "{doc}");
        }
    }
}

/// A deadline that is not a number is refused, not silently replaced.
#[test]
fn an_invalid_timeout_is_a_usage_error() {
    let (code, stdout, stderr) =
        Out::of(phux().env("PHUX_TIMEOUT", "abc").args(["ls", "--json"])).into_parts();
    assert_eq!(code, 2, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    let doc: serde_json::Value = serde_json::from_str(stderr.trim()).expect("one JSON line");
    assert_eq!(doc["error"]["code"], "usage_error", "{doc}");
    assert!(stderr.contains("PHUX_TIMEOUT"), "{stderr}");
}
