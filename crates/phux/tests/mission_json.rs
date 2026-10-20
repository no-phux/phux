//! `phux mission --json` goldens, checked against the `phux.mission/v1` schema.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in an integration test"
)]

#[path = "common/runner.rs"]
mod runner;

#[path = "common/ambient.rs"]
mod common;

use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;

#[test]
fn bare_mission_prints_help() {
    let xdg = common::XdgDirs::fresh("mission");
    let help = mission(&xdg, &["mission"]);
    assert_eq!(help.status.code(), Some(0), "{}", stderr(&help));
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("Usage:"), "{text}");
    assert!(text.contains("phux.mission/v1"), "{text}");
    assert!(text.contains("add"), "{text}");
    assert!(text.contains("edit"), "{text}");
    assert!(
        text.contains("--socket"),
        "the keeper watches this server; {text}"
    );
    assert!(!text.to_ascii_lowercase().contains("delegate"), "{text}");

    let flagged = mission(&xdg, &["mission", "--help"]);
    assert_eq!(flagged.status.code(), Some(0), "{}", stderr(&flagged));
    let flagged_text = String::from_utf8_lossy(&flagged.stdout);
    assert!(flagged_text.contains("phux.mission/v1"), "{flagged_text}");
    assert!(flagged_text.contains("--socket"), "{flagged_text}");
    assert!(!flagged_text.to_ascii_lowercase().contains("delegate"));

    let add_help = mission(&xdg, &["mission", "add", "--help"]);
    assert_eq!(add_help.status.code(), Some(0), "{}", stderr(&add_help));
    let add_text = String::from_utf8_lossy(&add_help.stdout);
    assert!(add_text.contains("--socket"), "{add_text}");
}

#[test]
fn mission_socket_names_the_server_the_keeper_watches() {
    let xdg = common::XdgDirs::fresh("mission");
    let output = mission(
        &xdg,
        &[
            "mission",
            "list",
            "--socket",
            "/tmp/phux-mission-unused.sock",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No tasks"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let pid_path = xdg.state.join("phux-dev/work/keeper.pid");
    if let Ok(text) = std::fs::read_to_string(&pid_path)
        && let Ok(pid) = text.trim().parse::<u32>()
        && pid != 0
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
}

#[test]
fn human_list_and_show_use_words_padding_and_rev() {
    let xdg = common::XdgDirs::fresh("mission-human");
    let empty = mission(&xdg, &["mission", "list"]);
    assert_eq!(empty.status.code(), Some(0), "{}", stderr(&empty));
    assert_eq!(
        String::from_utf8_lossy(&empty.stdout),
        "No tasks. Add one with phux mission add \"<title>\".\n"
    );

    let add = mission(
        &xdg,
        &["mission", "add", "Fix the prompt", "--repo", "phux"],
    );
    assert_eq!(add.status.code(), Some(0), "{}", stderr(&add));
    assert_eq!(String::from_utf8_lossy(&add.stdout), "added phux/1\n");
    let other = mission(
        &xdg,
        &[
            "mission",
            "add",
            "Grant expired",
            "--repo",
            "github.com/no-phux/phux",
        ],
    );
    assert_eq!(other.status.code(), Some(0), "{}", stderr(&other));

    let list = mission(&xdg, &["mission", "list"]);
    let listed = String::from_utf8_lossy(&list.stdout);
    let lines: Vec<_> = listed.lines().collect();
    assert_eq!(lines.len(), 2, "{listed}");
    assert_eq!(lines[0].find("todo"), lines[1].find("todo"), "{listed}");
    assert!(listed.contains("github.com/no-phux/phux/1"), "{listed}");
    assert!(!listed.contains("needs_you"), "{listed}");

    let show = mission(&xdg, &["mission", "show", "phux/1"]);
    let shown = String::from_utf8_lossy(&show.stdout);
    assert!(shown.ends_with("rev 1\n"), "{shown}");
    assert!(!shown.contains("revision:"), "{shown}");
    assert!(shown.contains("state: todo\n"), "{shown}");
}

#[test]
fn human_structural_edits_print_the_new_revision() {
    let xdg = common::XdgDirs::fresh("mission-human-edit");
    let add = mission(
        &xdg,
        &["mission", "add", "Fix the prompt", "--repo", "phux"],
    );
    assert_eq!(add.status.code(), Some(0), "{}", stderr(&add));

    let missing = mission(&xdg, &["mission", "edit", "phux/1", "--title", "Renamed"]);
    assert_eq!(missing.status.code(), Some(2), "{}", stderr(&missing));
    assert!(
        stderr(&missing).contains("phux/1 is at rev 1; rerun with --expected-revision 1"),
        "{}",
        stderr(&missing)
    );

    let edit = mission(
        &xdg,
        &[
            "mission",
            "edit",
            "phux/1",
            "--title",
            "Renamed",
            "--expected-revision",
            "1",
        ],
    );
    assert_eq!(
        String::from_utf8_lossy(&edit.stdout),
        "updated phux/1 (rev 2)\n",
        "{}",
        stderr(&edit)
    );

    let stale = mission(
        &xdg,
        &[
            "mission",
            "edit",
            "phux/1",
            "--title",
            "Nope",
            "--expected-revision",
            "1",
        ],
    );
    assert_eq!(stale.status.code(), Some(1), "{}", stderr(&stale));
    assert!(
        stderr(&stale).contains("phux/1 is at rev 2; rerun with --expected-revision 2"),
        "{}",
        stderr(&stale)
    );

    let archive = mission(
        &xdg,
        &["mission", "archive", "phux/1", "--expected-revision", "2"],
    );
    assert_eq!(
        String::from_utf8_lossy(&archive.stdout),
        "archived phux/1 (rev 3)\n",
        "{}",
        stderr(&archive)
    );
    let restore = mission(
        &xdg,
        &["mission", "restore", "phux/1", "--expected-revision", "3"],
    );
    assert_eq!(
        String::from_utf8_lossy(&restore.stdout),
        "restored phux/1 (rev 4)\n",
        "{}",
        stderr(&restore)
    );

    let empty_edit = mission(
        &xdg,
        &["mission", "edit", "phux/1", "--expected-revision", "4"],
    );
    assert_eq!(empty_edit.status.code(), Some(2), "{}", stderr(&empty_edit));
    assert!(
        stderr(&empty_edit).contains("edit needs a title or a description"),
        "{}",
        stderr(&empty_edit)
    );
}

#[test]
fn default_repo_uses_the_origin_remote() {
    let xdg = common::XdgDirs::fresh("mission-origin");
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("checkout");
    std::fs::create_dir(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(
        &dir,
        &["remote", "add", "origin", "git@github.com:no-phux/phux.git"],
    );
    let add = mission_at(&xdg, &dir, &["mission", "add", "From origin", "--json"]);
    assert_success("origin", &add);
    let doc: Value = serde_json::from_slice(&add.stdout).expect("json");
    assert_eq!(doc["task"]["repo"], "github.com/no-phux/phux");
    assert_eq!(doc["task"]["alias"], "github.com/no-phux/phux/1");
}

#[test]
fn default_repo_uses_the_directory_name_when_there_is_no_remote() {
    let xdg = common::XdgDirs::fresh("mission-dir");
    let root = tempfile::tempdir().expect("tempdir");
    let plain = root.path().join("widget");
    std::fs::create_dir(&plain).expect("mkdir");
    let add = mission_at(&xdg, &plain, &["mission", "add", "Local", "--json"]);
    assert_success("dir", &add);
    let doc: Value = serde_json::from_slice(&add.stdout).expect("json");
    assert_eq!(doc["task"]["repo"], "widget");
    assert_eq!(doc["task"]["alias"], "widget/1");

    let bare = root.path().join("lonely");
    std::fs::create_dir(&bare).expect("mkdir");
    git(&bare, &["init", "-q", "-b", "main"]);
    let no_origin = mission_at(&xdg, &bare, &["mission", "add", "No remote", "--json"]);
    assert_success("no-origin", &no_origin);
    let no_origin_doc: Value = serde_json::from_slice(&no_origin.stdout).expect("json");
    assert_eq!(no_origin_doc["task"]["repo"], "lonely");
    assert_eq!(no_origin_doc["task"]["alias"], "lonely/1");
}

#[test]
fn a_store_held_outside_a_keeper_fails_busy_after_a_short_retry() {
    let xdg = common::XdgDirs::fresh("mission-lock");
    let path = phux_work::store_path(&xdg.state.join("phux-missiontest"));
    let mut store = phux_work::Store::open(&path).expect("open");
    store
        .add(phux_work::AddObjective {
            repo: "phux".to_owned(),
            title: "kept".to_owned(),
            description: String::new(),
            operation_id: phux_work::OperationId::generate().expect("id"),
            actor: "tester".to_owned(),
        })
        .expect("add");

    let started = std::time::Instant::now();
    let mut command = common::phux_cmd(runner::phux_bin());
    xdg.apply(&mut command);
    command.env("HOME", &xdg.state);
    command.env("PHUX_PROFILE", "missiontest");
    command.env("PHUX_WORK_IDLE_EXIT_SECS", KEEPER_IDLE_SECS);
    let output = command
        .args(["mission", "list", "--json"])
        .output()
        .expect("spawn");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_millis(1900),
        "retry ended after {elapsed:?}"
    );
    assert!(
        elapsed <= std::time::Duration::from_secs(6),
        "retry ended after {elapsed:?}"
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let doc: Value = serde_json::from_slice(&output.stderr).expect("busy json");
    assert_eq!(doc["error"]["code"], "mission_busy");
    assert_eq!(
        doc["error"]["message"],
        "another process holds the work store; try again"
    );
    assert_valid(&schema_validator(), &doc);
    let tasks = store
        .list(&phux_work::ListFilter {
            repo: None,
            archived: false,
        })
        .expect("list");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "kept");
}

#[test]
fn every_verb_json_matches_its_golden_and_the_schema() {
    let schema = schema_validator();
    let xdg = common::XdgDirs::fresh("mission");
    let add = mission(
        &xdg,
        &[
            "mission",
            "add",
            "Fix the prompt",
            "--repo",
            "phux",
            "--description",
            "details",
            "--operation-id",
            &"a".repeat(32),
            "--json",
        ],
    );
    assert_success("add", &add);
    check(&schema, "add", &add);

    let list = mission(&xdg, &["mission", "list", "--json"]);
    assert_success("list", &list);
    check(&schema, "list", &list);

    let show = mission(&xdg, &["mission", "show", "phux/1", "--json"]);
    assert_success("show", &show);
    check(&schema, "show", &show);

    let edit = mission(
        &xdg,
        &[
            "mission",
            "edit",
            "phux/1",
            "--description",
            "more detail",
            "--expected-revision",
            "1",
            "--operation-id",
            &"b".repeat(32),
            "--json",
        ],
    );
    assert_success("edit", &edit);
    check(&schema, "edit", &edit);

    let archive = mission(
        &xdg,
        &[
            "mission",
            "archive",
            "phux/1",
            "--expected-revision",
            "2",
            "--operation-id",
            &"c".repeat(32),
            "--json",
        ],
    );
    assert_success("archive", &archive);
    check(&schema, "archive", &archive);

    let restore = mission(
        &xdg,
        &[
            "mission",
            "restore",
            "phux/1",
            "--expected-revision",
            "3",
            "--operation-id",
            &"d".repeat(32),
            "--json",
        ],
    );
    assert_success("restore", &restore);
    check(&schema, "restore", &restore);
    errors_and_replay(&schema, &xdg);
}

fn errors_and_replay(schema: &jsonschema::Validator, xdg: &common::XdgDirs) {
    let missing = mission(xdg, &["mission", "show", "phux/99", "--json"]);
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
    assert!(missing.stdout.is_empty());
    let error = serde_json::from_slice::<Value>(&missing.stderr).expect("error json");
    assert_eq!(error["schema"], "phux.mission/v1");
    assert_eq!(error["error"]["code"], "mission_not_found");
    assert_valid(schema, &error);

    let missing_rev = mission(
        xdg,
        &["mission", "edit", "phux/1", "--title", "nope", "--json"],
    );
    assert_eq!(
        missing_rev.status.code(),
        Some(2),
        "{}",
        stderr(&missing_rev)
    );
    let missing_doc = serde_json::from_slice::<Value>(&missing_rev.stderr).expect("error json");
    assert_eq!(missing_doc["error"]["code"], "usage_error");
    assert_eq!(
        missing_doc["error"]["message"],
        "phux/1 is at rev 4; rerun with --expected-revision 4"
    );
    assert_valid(schema, &missing_doc);

    let conflict = mission(
        xdg,
        &[
            "mission",
            "edit",
            "phux/1",
            "--title",
            "nope",
            "--expected-revision",
            "1",
            "--operation-id",
            &"e".repeat(32),
            "--json",
        ],
    );
    assert_eq!(conflict.status.code(), Some(1), "{}", stderr(&conflict));
    let conflict_doc = serde_json::from_slice::<Value>(&conflict.stderr).expect("error json");
    assert_eq!(conflict_doc["error"]["code"], "mission_revision_conflict");
    assert_eq!(
        conflict_doc["error"]["message"],
        "phux/1 is at rev 4; rerun with --expected-revision 4"
    );
    assert_valid(schema, &conflict_doc);

    let replay = mission(
        xdg,
        &[
            "mission",
            "add",
            "Fix the prompt",
            "--repo",
            "phux",
            "--description",
            "details",
            "--operation-id",
            &"a".repeat(32),
            "--json",
        ],
    );
    assert_success("replay", &replay);
    let replay_doc = serde_json::from_slice::<Value>(&replay.stdout).expect("replay json");
    assert_eq!(replay_doc["idempotent_replay"], true);
    assert_eq!(replay_doc["task"]["alias"], "phux/1");
    assert_valid(schema, &replay_doc);
}

/// Keepers these tests start exit this soon after their last request.
const KEEPER_IDLE_SECS: &str = "3";

#[test]
fn concurrent_adds_go_through_one_keeper_and_none_is_lost() {
    let xdg = common::XdgDirs::fresh("mission-concurrent");
    let children: Vec<_> = (0..8)
        .map(|index| {
            let mut command = mission_command(&xdg);
            command
                .args([
                    "mission",
                    "add",
                    &format!("task-{index}"),
                    "--repo",
                    "phux",
                    "--json",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            command.spawn().expect("spawn phux")
        })
        .collect();
    let mut aliases = std::collections::BTreeSet::new();
    for child in children {
        let output = child.wait_with_output().expect("wait");
        assert_success("concurrent add", &output);
        let doc: Value = serde_json::from_slice(&output.stdout).expect("json");
        aliases.insert(doc["task"]["alias"].as_str().expect("alias").to_owned());
    }
    let expected: std::collections::BTreeSet<_> = (1..=8).map(|n| format!("phux/{n}")).collect();
    assert_eq!(aliases, expected);

    let list = mission(&xdg, &["mission", "list", "--json"]);
    assert_success("list", &list);
    let doc: Value = serde_json::from_slice(&list.stdout).expect("json");
    assert_eq!(doc["tasks"].as_array().expect("tasks").len(), 8);
}

#[test]
fn a_second_keeper_exits_at_once_while_one_serves() {
    let xdg = common::XdgDirs::fresh("mission-keeper");
    let add = mission(&xdg, &["mission", "add", "First", "--repo", "phux"]);
    assert_eq!(add.status.code(), Some(0), "{}", stderr(&add));

    let started = std::time::Instant::now();
    let second = mission(&xdg, &["mission", "keeper", "--idle-exit", "30"]);
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "a second keeper served instead of exiting"
    );
}

fn mission_command(xdg: &common::XdgDirs) -> std::process::Command {
    let mut command = common::phux_cmd(runner::phux_bin());
    xdg.apply(&mut command);
    command.env("HOME", &xdg.state);
    command.env("PHUX_WORK_IDLE_EXIT_SECS", KEEPER_IDLE_SECS);
    command
}

fn mission(xdg: &common::XdgDirs, args: &[&str]) -> Output {
    mission_command(xdg)
        .args(args)
        .output()
        .expect("spawn phux")
}

fn mission_at(xdg: &common::XdgDirs, dir: &std::path::Path, args: &[&str]) -> Output {
    let mut command = mission_command(xdg);
    command.current_dir(dir);
    command.args(args).output().expect("spawn phux")
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        status.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn assert_success(verb: &str, output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{verb} failed\nstdout {}\nstderr {}",
        String::from_utf8_lossy(&output.stdout),
        stderr(output)
    );
    assert!(output.stderr.is_empty(), "{verb} stderr {}", stderr(output));
}

fn check(schema: &jsonschema::Validator, verb: &str, output: &Output) {
    let value = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "{verb} stdout is not JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["schema"], "phux.mission/v1", "{verb}");
    assert_valid(schema, &value);
    let mut normalized = value;
    normalize(&mut normalized);
    let pretty = format!(
        "{}\n",
        serde_json::to_string_pretty(&normalized).expect("pretty")
    );
    let path = golden_dir().join(format!("{verb}.json"));
    if std::env::var_os("PHUX_UPDATE_MISSION_GOLDENS").is_some() {
        std::fs::create_dir_all(golden_dir()).expect("golden dir");
        std::fs::write(&path, &pretty).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("golden {} missing ({err}). Got:\n{pretty}", path.display()));
    assert_eq!(pretty, expected, "{verb} golden drifted");
}

fn assert_valid(schema: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|err| err.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{value} does not match the schema: {errors:?}"
    );
}

fn schema_validator() -> jsonschema::Validator {
    let path = manifest_dir().join("../phux-mission/schema/phux.mission.v1.schema.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let schema: Value = serde_json::from_str(&text).expect("schema json");
    jsonschema::validator_for(&schema).expect("compile schema")
}

fn golden_dir() -> PathBuf {
    manifest_dir().join("tests/mission_goldens")
}

fn normalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(id) = map.get("id").and_then(Value::as_str)
                && is_hex32(id)
            {
                map.insert("id".to_owned(), Value::String("<id>".to_owned()));
            }
            for key in ["created_at", "updated_at"] {
                if map.contains_key(key) {
                    map.insert(key.to_owned(), Value::String("<timestamp>".to_owned()));
                }
            }
            for child in map.values_mut() {
                normalize(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_hex32(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Read at run time: a compiled-in path would tie the test to one checkout
/// (`scripts/check-cache-portable.sh`).
fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
}
