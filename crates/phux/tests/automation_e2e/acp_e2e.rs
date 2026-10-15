//! Real CLI, daemon, durable guardian and official Rust ACP connection.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
#[path = "../common/mod.rs"]
mod common;
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn document(server: &common::ServerGuard, args: &[&str]) -> Value {
    serde_json::from_str(&server.success(args)).unwrap()
}
fn await_value(mut read: impl FnMut() -> Value, want: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let value = read();
        if want(&value) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "expected ACP state; last: {value}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn log(server: &common::ServerGuard, agent: &str) -> Value {
    let (code, stdout, stderr) = server.run(&["agent", "log", agent, "--json"]);
    assert_eq!(
        code,
        0,
        "{stderr}; resources={:?}; guardian={:?}",
        server.run(&["ls", "--json"]),
        server.run(&["snapshot", "@2"])
    );
    serde_json::from_str(&stdout).unwrap()
}
fn latest_view(log: &Value) -> Value {
    log["records"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find_map(|row| {
            let update = &row["data"]["update"];
            (update["type"] == "view").then(|| update["view"].clone())
        })
        .unwrap_or(Value::Null)
}
fn command(
    server: &common::ServerGuard,
    agent: &str,
    activation: &str,
    id: u64,
    intent: &Value,
) -> Value {
    let (status, stdout, stderr) = server.run(&[
        "agent",
        "native-command",
        agent,
        "--activation",
        activation,
        "--operation-id",
        &id.to_string(),
        "--intent",
        &intent.to_string(),
        "--json",
    ]);
    assert!([0, 2].contains(&status), "native command: {stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("native receipt missing: {stdout} {stderr}"))
}
fn profile(server: &common::ServerGuard, root: &Path, ledger: &Path) {
    profile_mode(server, root, ledger, true);
}
fn profile_mode(server: &common::ServerGuard, root: &Path, ledger: &Path, load_supported: bool) {
    let plugin = root.join("plugin");
    std::fs::create_dir_all(plugin.join("integrations")).unwrap();
    std::fs::write(
        plugin.join("phux-plugin.toml"),
        "id = 'test.acp'\nname = 'ACP proof'\nversion = '1.0.0'\nmin_phux_version = '0.0.2'\n",
    )
    .unwrap();
    let peer = crate::runner::manifest_dir().join("tests/fixtures/acp_peer.py");
    std::fs::write(plugin.join("integrations/peer.toml"), format!("id = 'peer'\n[acp]\nprovider = 'peer'\ncommand = ['python3', {:?}, {:?}, {:?}]\nrequired_executables = ['python3']\n", peer.display().to_string(), ledger.display().to_string(), if load_supported { "load" } else { "no-load" })).unwrap();
    let config = server.xdg.config.join("phux");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[[plugins]]\nmanifest = {:?}\nenabled = true\n",
            plugin.join("phux-plugin.toml").display().to_string()
        ),
    )
    .unwrap();
}

#[test]
#[ignore = "real isolated daemon and ACP peer; run via just e2e"]
fn cli_acp_permissions_stream_reconnect_and_same_worker_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let ledger = root.path().join("peer.jsonl");
    let server = common::ServerGuard::start("acp");
    profile(&server, root.path(), &ledger);
    let listed = document(&server, &["launch", "--acp", "--list", "--json"]);
    assert!(listed.to_string().contains("peer"));
    let spawned = document(
        &server,
        &["launch", "--acp", "peer", "--session", "work", "--json"],
    );
    let agent = spawned["agent_session_id"].as_str().unwrap();
    let activation = spawned["activation"].as_str().unwrap();
    let view = await_value(
        || {
            let events = log(&server, agent);
            let view = latest_view(&events);
            assert!(
                view["provider_session_id"].is_null() || view["connected"] == true,
                "ACP closed during startup: {events}; guardian={:?}",
                server.run(&["snapshot", "@2"])
            );
            view
        },
        |view| view["connected"] == true,
    );
    assert_eq!(view["provider_session_id"], "peer-stable-session");
    assert_eq!(view["capabilities"]["follow_up"], false);
    permission_rounds(&server, agent, activation);
    // A new CLI connection observes the same activation; upgrade must retain the
    // very same external worker, not reconstruct an ACP client or replay prompts.
    let before = std::fs::read_to_string(&ledger).unwrap();
    assert_eq!(
        before
            .lines()
            .filter(|line| line.contains("started"))
            .count(),
        1
    );
    let turn = upgrade_and_cancel(&server, agent, activation);
    let after = std::fs::read_to_string(&ledger).unwrap();
    assert_eq!(
        after
            .lines()
            .filter(|line| line.contains("started"))
            .count(),
        1
    );
    assert!(!after.contains("\"private_binding\": true"));
    assert_eq!(
        after
            .lines()
            .filter(|line| line.contains("\"method\": \"initialize\""))
            .count(),
        1
    );
    assert_eq!(
        after
            .lines()
            .filter(|line| line.contains("\"method\": \"session/prompt\""))
            .count(),
        4
    );
    let duplicate = command(
        &server,
        agent,
        activation,
        41,
        &json!({"type":"interrupt","turn_id":turn}),
    );
    assert_eq!(duplicate["result"]["disposition"], "queued");
    let stale = command(
        &server,
        agent,
        "stale-activation",
        50,
        &json!({"type":"prompt","text":"never send"}),
    );
    assert_eq!(stale["result"]["disposition"], "refused");
    assert!(
        after.contains("-32601"),
        "unsupported callbacks must fail explicitly: {after}"
    );
    server.success(&["kill", agent, "--yes"]);
}

#[test]
#[ignore = "real isolated daemon and ACP peer; run via just e2e"]
fn cli_acp_transport_loss_is_unknown_and_never_replays_prompt() {
    let root = tempfile::tempdir().unwrap();
    let ledger = root.path().join("peer.jsonl");
    let server = common::ServerGuard::start("acploss");
    profile(&server, root.path(), &ledger);
    let spawned = document(
        &server,
        &["launch", "--acp", "peer", "--session", "work", "--json"],
    );
    let agent = spawned["agent_session_id"].as_str().unwrap();
    let activation = spawned["activation"].as_str().unwrap();
    await_value(
        || latest_view(&log(&server, agent)),
        |view| view["connected"] == true,
    );
    let intent = json!({"type":"prompt","text":"disconnect"});
    command(&server, agent, activation, 90, &intent);
    await_value(
        || latest_view(&log(&server, agent)),
        |view| view["connected"] == false,
    );
    let receipt = command(&server, agent, activation, 90, &intent);
    assert_eq!(receipt["result"]["disposition"], "unknown");
    let calls = std::fs::read_to_string(&ledger).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains("\"method\": \"session/prompt\""))
            .count(),
        1
    );
}

fn upgrade_and_cancel(server: &common::ServerGuard, agent: &str, activation: &str) -> String {
    server.success(&["upgrade"]);
    let view = await_value(
        || latest_view(&log(server, agent)),
        |view| view["connected"] == true,
    );
    assert_eq!(view["activation"], activation);
    command(
        server,
        agent,
        activation,
        40,
        &json!({"type":"prompt","text":"hold"}),
    );
    let view = await_value(
        || latest_view(&log(server, agent)),
        |view| view["questions"].as_array().is_some_and(|q| !q.is_empty()),
    );
    let turn = view["turn_id"].as_str().unwrap();
    let stopped = command(
        server,
        agent,
        activation,
        41,
        &json!({"type":"interrupt","turn_id":turn}),
    );
    assert_eq!(stopped["result"]["disposition"], "queued");
    await_value(
        || latest_view(&log(server, agent)),
        |view| view["turn_id"].is_null() && view["questions"].as_array().is_some_and(Vec::is_empty),
    );
    turn.to_owned()
}

fn permission_rounds(server: &common::ServerGuard, agent: &str, activation: &str) {
    for (id, response, expected) in [
        (10, json!({"option_id":"allow-once"}), "allow-once"),
        (20, json!({"option_id":"deny-once"}), "deny-once"),
        (30, json!({"cancelled":true}), "cancelled"),
    ] {
        let receipt = command(
            server,
            agent,
            activation,
            id,
            &json!({"type":"prompt","text":"permission"}),
        );
        assert!(matches!(
            receipt["result"]["disposition"].as_str(),
            Some("queued" | "completed")
        ));
        let question_view = await_value(
            || latest_view(&log(server, agent)),
            |view| view["questions"].as_array().is_some_and(|q| !q.is_empty()),
        );
        let question_id = question_view["questions"][0]["id"].as_str().unwrap();
        let intent = json!({"type":"answer","question_id":question_id,"response":response});
        let answers = std::thread::scope(|scope| {
            let first = scope.spawn(|| command(server, agent, activation, id + 1, &intent));
            let second = scope.spawn(|| command(server, agent, activation, id + 2, &intent));
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(
            answers
                .iter()
                .filter(|answer| answer["result"]["disposition"] == "queued")
                .count(),
            1
        );
        assert_eq!(
            answers
                .iter()
                .filter(|answer| answer["result"]["disposition"] == "refused")
                .count(),
            1
        );
        let rows = await_value(
            || log(server, agent),
            |log| {
                log.to_string()
                    .contains(&format!("peer result: {expected}"))
            },
        );
        assert!(rows.to_string().contains("inert tool"));
        await_value(
            || latest_view(&log(server, agent)),
            |view| view["turn_id"].is_null(),
        );
    }
}

fn restored_agent(server: &common::ServerGuard) -> String {
    let listed = await_value(
        || document(server, &["ls", "--json"]),
        |doc| {
            doc["resources"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["kind"] == "agent_session"))
        },
    );
    listed["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "agent_session")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
#[ignore = "real isolated daemon and ACP peer; run via just e2e"]
fn cli_acp_archive_load_is_capability_gated_and_history_is_finalized() {
    for supports_load in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let ledger = root.path().join("peer.jsonl");
        let archive = root.path().join("archive.json");
        let source = common::ServerGuard::start("acpload");
        profile_mode(&source, root.path(), &ledger, supports_load);
        let spawned = document(
            &source,
            &["launch", "--acp", "peer", "--session", "work", "--json"],
        );
        let source_agent = spawned["agent_session_id"].as_str().unwrap();
        await_value(
            || latest_view(&log(&source, source_agent)),
            |view| view["connected"] == true,
        );
        source.success(&["archive", "export", "--output", archive.to_str().unwrap()]);
        let saved = std::fs::read_to_string(&archive).unwrap();
        assert!(saved.contains("peer-stable-session"));
        assert!(!saved.contains("PHUX_NATIVE_BINDING"));
        drop(source);
        let dest = common::ServerGuard::builder("acpload")
            .session("destination")
            .start();
        dest.success(&["archive", "import", archive.to_str().unwrap()]);
        dest.success(&["restore", "work"]);
        let agent = restored_agent(&dest);
        if supports_load {
            let view = await_value(
                || latest_view(&log(&dest, &agent)),
                |view| view["connected"] == true,
            );
            assert_eq!(view["provider_session_id"], "peer-stable-session");
            let activation = view["activation"].as_str().unwrap();
            command(
                &dest,
                &agent,
                activation,
                70,
                &json!({"type":"prompt","text":"resumed"}),
            );
            let view = await_value(
                || latest_view(&log(&dest, &agent)),
                |view| view["questions"].as_array().is_some_and(|q| !q.is_empty()),
            );
            let question = view["questions"][0]["id"].as_str().unwrap();
            command(
                &dest,
                &agent,
                activation,
                71,
                &json!({"type":"answer","question_id":question,"response":{"option_id":"deny-once"}}),
            );
            let events = await_value(
                || log(&dest, &agent),
                |log| log.to_string().contains("peer result: deny-once"),
            );
            assert!(events.to_string().contains("historical assistant"));
            assert!(!events.to_string().contains("historical assistantstreamed"));
        } else {
            await_value(
                || log(&dest, &agent),
                |log| log.to_string().contains("does not advertise session/load"),
            );
        }
        let calls = std::fs::read_to_string(&ledger).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("\"method\": \"session/new\""))
                .count(),
            1,
            "never fresh-start an unsupported resume"
        );
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("\"method\": \"session/load\""))
                .count(),
            usize::from(supports_load)
        );
    }
}
