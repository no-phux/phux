/// Write an executable `claude` that paints nothing and holds, so every
/// lifecycle edge must come from the session stream.
fn write_quiet_claude(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("claude");
    std::fs::write(&path, "#!/bin/sh\nprintf '\\033[2J\\033[H'\nsleep 120\n")
        .expect("write quiet claude");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod quiet claude");
    path
}

/// As [`run_hook`], with Claude's JSON payload on the hook's stdin — the
/// shape Claude Code actually hands a hook.
fn run_hook_with_payload(
    shim: &std::path::Path,
    event: &str,
    terminal_id: u32,
    socket: &std::path::Path,
    payload: &serde_json::Value,
) {
    use std::io::Write as _;

    let mut child = common::phux_cmd(shim)
        .args(["--phux-hook", event])
        .env("PHUX_TERMINAL_ID", terminal_id.to_string())
        .env("PHUX_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("run the installed shim's {event} hook: {err}"));
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write the hook payload");
    let out = child.wait_with_output().expect("wait for the hook");
    assert!(
        out.status.success(),
        "the {event} hook exited {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

impl ServerGuard {
    /// Whether this server advertises `resource_kinds` — the same probe the
    /// generated wrapper runs, through the same verb.
    fn serves_resource_kinds(&self) -> bool {
        let status = self.run_with_env(&["status", "--json"], &[]);
        let json: serde_json::Value = serde_json::from_str(&status).expect("status JSON");
        json["features"]
            .as_array()
            .is_some_and(|features| features.iter().any(|f| f == "resource_kinds"))
    }

    /// `phux agent log TARGET --json`, decoded into its records. Accepts a
    /// JSON array, an object carrying a `records` array, or one record per
    /// line, so the assertion does not hinge on the verb's framing.
    fn agent_log(&self, target: &str) -> Vec<serde_json::Value> {
        let text = self.agent(&["log", target, "--json"]);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(records) = value.as_array() {
                return records.clone();
            }
            if let Some(records) = value["records"].as_array() {
                return records.clone();
            }
        }
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|err| panic!("agent log record ({err}): {line}"))
            })
            .collect()
    }

    /// Poll the log until a record of `kind` appears and return the whole
    /// log. Panics with the last reading on timeout.
    fn await_record(&self, target: &str, kind: &str, deadline: Duration) -> Vec<serde_json::Value> {
        let end = Instant::now() + deadline;
        loop {
            let records = self.agent_log(target);
            if records.iter().any(|record| record["type"] == kind) {
                return records;
            }
            assert!(
                Instant::now() < end,
                "{target} never logged a `{kind}` record within {deadline:?}; last: {records:?}"
            );
            std::thread::sleep(RECORD_POLL);
        }
    }

    /// Poll the log until a `phux.transcript/v1` entry with `role` appears
    /// and return the whole log.
    fn await_transcript(
        &self,
        target: &str,
        role: &str,
        deadline: Duration,
    ) -> Vec<serde_json::Value> {
        let end = Instant::now() + deadline;
        loop {
            let records = self.agent_log(target);
            if records.iter().any(|record| is_transcript(record, role)) {
                return records;
            }
            assert!(
                Instant::now() < end,
                "{target} never logged a `{role}` transcript entry within {deadline:?}; last: {records:?}"
            );
            std::thread::sleep(RECORD_POLL);
        }
    }
}

fn is_transcript(record: &serde_json::Value, role: &str) -> bool {
    record["type"] == "provider_raw"
        && record["data"]["schema"] == "phux.transcript/v1"
        && record["data"]["provider"] == "claude"
        && record["data"]["entry"]["role"] == role
}

/// The last `phux.transcript/v1` entry with `role` in `records`.
fn transcript_entry(records: &[serde_json::Value], role: &str) -> serde_json::Value {
    records
        .iter()
        .rev()
        .find(|record| is_transcript(record, role))
        .map_or_else(
            || panic!("a `{role}` transcript entry: {records:?}"),
            |record| record["data"]["entry"].clone(),
        )
}

/// The generated wrapper, fed Claude's real hook payloads, drives an agent
/// session end to end: `SessionStart` opens it with Claude's `session_id`;
/// prompt, tool, ask, and stop hooks log records and the pane derives
/// `working`, `blocked`, then `done`; `SessionEnd` closes it. No prompt text,
/// tool input, or tool output may appear in a typed record; the conversation
/// rides `provider_raw` records in the `phux.transcript/v1` convention
/// (ADR-0156), and the transcript path never appears at all. States are read from a
/// `phux watch` because the detector's next screen tick supersedes a stream
/// edge within ~300 ms.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
#[allow(clippy::too_many_lines, reason = "one linear hook-by-hook scenario")]
fn the_generated_claude_shim_feeds_the_agent_session_stream() {
    const SESSION_ID: &str = "e2e-claude-session-0001";

    let home = tempfile::tempdir().expect("create temp HOME");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin dir");
    let fake_claude = write_quiet_claude(&bin);
    let data = home.path().join("data");

    let install = common::phux_cmd(crate::runner::phux_bin())
        .args(["agent", "install-claude", "--shell", "bash", "--real"])
        .arg(&fake_claude)
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", &data)
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent install-claude");
    assert!(
        install.status.success(),
        "install-claude exited {:?}; stderr={}",
        install.status.code(),
        String::from_utf8_lossy(&install.stderr)
    );
    let shim = data.join("phux").join("shims").join("claude");

    let server = start_with_env(&[("PHUX_AGENT_STARTUP_GRACE_MS", TEST_STARTUP_GRACE_MS)]);
    // The wrapper takes its stream path only when the server serves
    // `AgentSession`; without it the test would prove nothing.
    assert!(
        server.serves_resource_kinds(),
        "this server must advertise `resource_kinds`"
    );
    let terminal_id = server.spawn_pane(&fake_claude);
    let target = format!("@{terminal_id}");
    // Live for the whole scenario: every state edge below is a published
    // record that the detector's next tick supersedes.
    let watch = common::WatchChild::start(
        std::path::Path::new(crate::runner::phux_bin()),
        &server.socket,
        &target,
        &[],
        RECORD_POLL,
        DETECT_DEADLINE,
    );
    watch.await_line("an initial agent_state line for the pane", |line| {
        line["event"] == "agent_state" && line["terminal"] == target.as_str()
    });
    let payload = |event: &str, extra: serde_json::Value| {
        let mut base = serde_json::json!({
            "session_id": SESSION_ID,
            "hook_event_name": event,
            "transcript_path": "/nonexistent/TRANSCRIPT-MARKER.jsonl",
            "cwd": "/tmp",
        });
        base.as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        base
    };
    let assert_private = |records: &[serde_json::Value]| {
        let all = serde_json::to_string(records).expect("serialize log");
        assert!(
            !all.contains("TRANSCRIPT-MARKER"),
            "the transcript path must never reach the stream: {all}"
        );
        let typed: Vec<_> = records
            .iter()
            .filter(|record| record["type"] != "provider_raw")
            .collect();
        let text = serde_json::to_string(&typed).expect("serialize typed records");
        for marker in [
            "PROMPT-MARKER",
            "INPUT-MARKER",
            "OUTPUT-MARKER",
            "TRANSCRIPT-MARKER",
        ] {
            assert!(
                !text.contains(marker),
                "the stream must never carry `{marker}`: {text}"
            );
        }
    };

    // --- 1. SessionStart opens the session under the pane -----------------
    run_hook_with_payload(
        &shim,
        "start",
        terminal_id,
        &server.socket,
        &payload("SessionStart", serde_json::json!({ "source": "startup" })),
    );
    let records = server.await_record(&target, "session_start", DETECT_DEADLINE);
    assert_eq!(records[0]["type"], "session_start", "{records:?}");
    let shown = server.agent_show(&target);
    // The key is `agent_session`, not `session`: `session` in this document
    // was already the phux session name, and still is.
    let session = &shown["agents"][0]["agent_session"];
    assert_eq!(session["provider"], "claude", "{shown}");
    assert_eq!(session["native_id"], SESSION_ID, "{shown}");
    assert_eq!(shown["agents"][0]["session"], SESSION, "{shown}");

    // --- 2. UserPromptSubmit: a count, never the text; working ------------
    run_hook_with_payload(
        &shim,
        "working",
        terminal_id,
        &server.socket,
        &payload(
            "UserPromptSubmit",
            serde_json::json!({ "prompt": "hello PROMPT-MARKER world" }),
        ),
    );
    let records = server.await_record(&target, "prompt", DETECT_DEADLINE);
    let prompt = records
        .iter()
        .find(|r| r["type"] == "prompt")
        .expect("prompt record");
    assert_eq!(prompt["data"]["chars"], 25, "{prompt}");
    assert_private(&records);
    let user = transcript_entry(
        &server.await_transcript(&target, "user", DETECT_DEADLINE),
        "user",
    );
    assert_eq!(user["text"], "hello PROMPT-MARKER world", "{user}");
    assert_eq!(user["final"], true, "{user}");
    watch.await_agent_state("working");

    // --- 3. Tool records name the tool and nothing else -------------------
    run_hook_with_payload(
        &shim,
        "tool-start",
        terminal_id,
        &server.socket,
        &payload(
            "PreToolUse",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "echo INPUT-MARKER" },
                "tool_use_id": "toolu_e2e"
            }),
        ),
    );
    run_hook_with_payload(
        &shim,
        "tool-end",
        terminal_id,
        &server.socket,
        &payload(
            "PostToolUse",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "echo INPUT-MARKER" },
                "tool_response": "OUTPUT-MARKER",
                "tool_use_id": "toolu_e2e"
            }),
        ),
    );
    let records = server.await_record(&target, "tool_end", DETECT_DEADLINE);
    for kind in ["tool_start", "tool_end"] {
        let record = records
            .iter()
            .find(|r| r["type"] == kind)
            .unwrap_or_else(|| panic!("{kind} record: {records:?}"));
        assert_eq!(record["data"]["tool_name"], "Bash", "{record}");
        assert!(record["data"].get("tool_input").is_none(), "{record}");
    }
    assert_private(&records);
    let tool = transcript_entry(
        &server.await_transcript(&target, "tool", DETECT_DEADLINE),
        "tool",
    );
    // Tool output is not on the screen; only PHUX_AGENT_TRANSCRIPT=full carries it.
    assert_eq!(tool["id"], "toolu_e2e", "{tool}");
    assert_eq!(
        tool["tool"],
        serde_json::json!({
            "name": "Bash",
            "call_id": "toolu_e2e",
            "summary": "echo INPUT-MARKER",
            "status": "ok",
            "output": ""
        }),
        "{tool}"
    );

    // --- 4. PermissionRequest: ask, and blocked ---------------------------
    run_hook_with_payload(
        &shim,
        "blocked",
        terminal_id,
        &server.socket,
        &payload(
            "PermissionRequest",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "rm INPUT-MARKER" },
                "message": "Claude wants to run: rm INPUT-MARKER"
            }),
        ),
    );
    let records = server.await_record(&target, "ask", DETECT_DEADLINE);
    assert_private(&records);
    watch.await_agent_state("blocked");

    // --- 5. Stop: done ----------------------------------------------------
    run_hook_with_payload(
        &shim,
        "done",
        terminal_id,
        &server.socket,
        &payload(
            "Stop",
            serde_json::json!({ "last_assistant_message": "OUTPUT-MARKER" }),
        ),
    );
    let records = server.await_record(&target, "stop", DETECT_DEADLINE);
    assert_private(&records);
    let reply = transcript_entry(&records, "assistant");
    assert_eq!(reply["text"], "OUTPUT-MARKER", "{reply}");
    let seq_of = |pred: &dyn Fn(&serde_json::Value) -> bool| {
        records.iter().find(|r| pred(r)).map(|r| r["seq"].as_u64())
    };
    assert!(
        seq_of(&|r| r["data"]["entry"]["role"] == "assistant") < seq_of(&|r| r["type"] == "stop"),
        "the reply lands before the turn's stop: {records:?}"
    );
    watch.await_agent_state("done");
    // The whole ordered ladder, on one stream: the hooks drove the pane
    // through working, blocked and done in that order, from a screen that
    // never painted a character.
    let timeline = watch.seen();
    let at = |state: &str| {
        common::first_index(&timeline, |line| {
            line["event"] == "agent_state" && line["state"] == state
        })
        .unwrap_or_else(|| panic!("a {state} edge: {timeline:?}"))
    };
    assert!(
        at("working") < at("blocked") && at("blocked") < at("done"),
        "the hook order must reach the consumer in order: {timeline:?}"
    );

    // --- 6. SessionEnd: session_end, and the session is closed ------------
    run_hook_with_payload(
        &shim,
        "clear",
        terminal_id,
        &server.socket,
        &payload(
            "SessionEnd",
            serde_json::json!({ "reason": "prompt_input_exit" }),
        ),
    );
    // The stream wrapper ends the session without `agent clear`: the session
    // leaves the report and inventory, and the record goes back to the detector.
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        let shown = server.agent_show(&target);
        let listed = server.run_with_env(&["ls", "--json"], &[]);
        let inventory: serde_json::Value =
            serde_json::from_str(&listed).unwrap_or_else(|err| panic!("ls JSON ({err}): {listed}"));
        let gone_from_report = shown["agents"][0]["agent_session"].is_null();
        let gone_from_inventory = inventory["resources"].as_array().is_some_and(|resources| {
            resources
                .iter()
                .all(|entry| entry["kind"] != "agent_session")
        });
        if gone_from_report && gone_from_inventory {
            break;
        }
        assert!(
            Instant::now() < end,
            "SessionEnd must close the session; last report: {shown}; inventory: {inventory}"
        );
        std::thread::sleep(RECORD_POLL);
    }
    // And the closed session's stream is no longer readable through the pane.
    let out = common::phux_cmd(crate::runner::phux_bin())
        .arg("agent")
        .args(["log", &target, "--json", "--socket"])
        .arg(&server.socket)
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent log");
    assert!(
        !out.status.success(),
        "`agent log` on a pane whose session ended must refuse: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
