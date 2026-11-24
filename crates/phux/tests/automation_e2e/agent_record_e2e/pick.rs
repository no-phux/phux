/// The pane's live title once it starts with `prefix`, polled through `snapshot`.
fn await_title(server: &ServerGuard, target: &str, prefix: &str) -> String {
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        let shot = server.run_with_env(&["snapshot", target, "--json"], &[]);
        let screen: serde_json::Value = serde_json::from_str(&shot).expect("snapshot JSON");
        if let Some(title) = screen["title"].as_str().filter(|t| t.starts_with(prefix)) {
            return title.to_owned();
        }
        assert!(Instant::now() < end, "title never began {prefix:?}: {screen}");
        std::thread::sleep(RECORD_POLL);
    }
}

/// Wait until `file` holds `lines` lines and return it.
fn await_lines(file: &std::path::Path, lines: usize) -> String {
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && text.lines().count() >= lines
        {
            return text;
        }
        assert!(Instant::now() < end, "{} never reached {lines} lines", file.display());
        std::thread::sleep(RECORD_POLL);
    }
}

/// phux-nigxp: `phux pick` raises its items as a `phux-ask`, an answer from
/// outside the pane (what a client's card sends) is printed with exit 0 and
/// the ask is retracted, and Escape cancels with exit 1.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn pick_prints_the_answered_item_and_escape_cancels() {
    let home = tempfile::tempdir().expect("create temp dir");
    let out = home.path().join("picked.txt");
    let script = home.path().join("picker");
    let phux = crate::runner::phux_bin();
    let out_path = out.display();
    common::write_executable(
        &script,
        format!(
            "#!/bin/sh\nprintf 'alpha\\nbeta\\n' | '{phux}' pick --prompt Which > '{out_path}'\n\
             echo \"exit=$?\" >> '{out_path}'\n\
             '{phux}' pick --prompt Again gamma >> '{out_path}'\n\
             echo \"exit=$?\" >> '{out_path}'\nsleep 120\n"
        ),
    );
    let server = ServerGuard::start("pick");
    let target = format!("@{}", server.spawn_pane(&script));

    let title = await_title(&server, &target, "phux-ask[pick-");
    let marker = phux_client_core::ask::parse_ask_title(&title).expect("a phux-ask title");
    assert_eq!(marker.question, "Which");
    assert_eq!(marker.suggestions, ["alpha", "beta"]);
    server.agent(&["answer", &target, "--id", &marker.id, "--choice", "2"]);
    assert_eq!(await_lines(&out, 2), "beta\nexit=0\n");

    // The second pick is a new ask; the first one's title is gone.
    let again = await_title(&server, &target, "phux-ask[pick-");
    assert!(again.contains(":Again?s=gamma"), "{again}");
    server.success(&["send-keys", &target, "Escape"]);
    assert_eq!(await_lines(&out, 3), "beta\nexit=0\nexit=1\n");
}
