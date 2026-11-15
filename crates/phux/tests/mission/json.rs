//! `phux mission --json` goldens, checked against the `phux.mission/v1` schema.

use crate::common;
use crate::golden::{assert_valid, check, schema_validator};
use crate::runner;
use crate::support::git;
use std::process::Output;

use serde_json::Value;

/// Keepers these tests start exit this soon after their last request.
const KEEPER_IDLE_SECS: &str = "3";

#[test]
fn bare_mission_prints_help() {
    let xdg = common::XdgDirs::fresh("mission");
    for (args, needles) in [
        (
            &["mission"][..],
            &["Usage:", "phux.mission/v1", "add", "edit", "--socket"][..],
        ),
        (&["mission", "--help"], &["phux.mission/v1", "--socket"]),
        (&["mission", "add", "--help"], &["--socket"]),
    ] {
        let (text, _) = expect_exit(&xdg, args, 0);
        for needle in needles {
            assert!(text.contains(needle), "{args:?} lacks {needle}: {text}");
        }
        if args.len() < 3 {
            assert!(!text.to_ascii_lowercase().contains("delegate"), "{text}");
        }
    }
}

#[test]
fn mission_socket_names_the_server_the_keeper_watches() {
    let xdg = common::XdgDirs::fresh("mission");
    let args = [
        "mission",
        "list",
        "--socket",
        "/tmp/phux-mission-unused.sock",
    ];
    let (listed, _) = expect_exit(&xdg, &args, 0);
    assert!(listed.contains("No tasks"), "{listed}");
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
    let (empty, _) = expect_exit(&xdg, &["mission", "list"], 0);
    assert_eq!(
        empty,
        "No tasks. Add one with phux mission add \"<title>\".\n"
    );
    let (added, _) = expect_exit(
        &xdg,
        &["mission", "add", "Fix the prompt", "--repo", "phux"],
        0,
    );
    assert_eq!(added, "added phux/1\n");
    let other = [
        "mission",
        "add",
        "Grant expired",
        "--repo",
        "github.com/no-phux/phux",
    ];
    expect_exit(&xdg, &other, 0);

    let (listed, _) = expect_exit(&xdg, &["mission", "list"], 0);
    let lines: Vec<_> = listed.lines().collect();
    assert_eq!(lines.len(), 2, "{listed}");
    assert_eq!(lines[0].find("todo"), lines[1].find("todo"), "{listed}");
    assert!(listed.contains("github.com/no-phux/phux/1"), "{listed}");
    assert!(!listed.contains("needs_you"), "{listed}");

    let (shown, _) = expect_exit(&xdg, &["mission", "show", "phux/1"], 0);
    assert!(shown.ends_with("rev 1\n"), "{shown}");
    assert!(!shown.contains("revision:"), "{shown}");
    assert!(shown.contains("state: todo\n"), "{shown}");
}

#[test]
fn human_structural_edits_print_the_new_revision() {
    let xdg = common::XdgDirs::fresh("mission-human-edit");
    expect_exit(
        &xdg,
        &["mission", "add", "Fix the prompt", "--repo", "phux"],
        0,
    );
    let edit = |rev: Option<&'static str>, title: &'static str| {
        let mut args = vec!["mission", "edit", "phux/1"];
        if !title.is_empty() {
            args.extend(["--title", title]);
        }
        if let Some(rev) = rev {
            args.extend(["--expected-revision", rev]);
        }
        args
    };
    let at_rev = |rev: u8| format!("phux/1 is at rev {rev}; rerun with --expected-revision {rev}");
    // (args, exit code, stdout on success or a stderr phrase on failure)
    let steps: [(Vec<&str>, i32, String); 6] = [
        (edit(None, "Renamed"), 2, at_rev(1)),
        (
            edit(Some("1"), "Renamed"),
            0,
            "updated phux/1 (rev 2)\n".to_owned(),
        ),
        (edit(Some("1"), "Nope"), 1, at_rev(2)),
        (
            vec!["mission", "archive", "phux/1", "--expected-revision", "2"],
            0,
            "archived phux/1 (rev 3)\n".to_owned(),
        ),
        (
            vec!["mission", "restore", "phux/1", "--expected-revision", "3"],
            0,
            "restored phux/1 (rev 4)\n".to_owned(),
        ),
        (
            edit(Some("4"), ""),
            2,
            "edit needs a title or a description".to_owned(),
        ),
    ];
    for (args, code, expected) in steps {
        let (stdout, stderr) = expect_exit(&xdg, &args, code);
        if code == 0 {
            assert_eq!(stdout, expected, "{args:?}: {stderr}");
        } else {
            assert!(stderr.contains(&expected), "{args:?}: {stderr}");
        }
    }
}

#[test]
fn default_repo_follows_the_origin_remote_or_the_directory_name() {
    let xdg = common::XdgDirs::fresh("mission-origin");
    let root = tempfile::tempdir().expect("tempdir");
    for (name, git_init, origin, repo) in [
        (
            "checkout",
            true,
            Some("git@github.com:no-phux/phux.git"),
            "github.com/no-phux/phux",
        ),
        ("widget", false, None, "widget"),
        ("lonely", true, None, "lonely"),
    ] {
        let dir = root.path().join(name);
        std::fs::create_dir(&dir).expect("mkdir");
        if git_init {
            git(&dir, &["init", "-q", "-b", "main"]);
        }
        if let Some(origin) = origin {
            git(&dir, &["remote", "add", "origin", origin]);
        }
        let add = mission_command(&xdg)
            .current_dir(&dir)
            .args(["mission", "add", name, "--json"])
            .output()
            .expect("spawn phux");
        assert_success(name, &add);
        let doc: Value = serde_json::from_slice(&add.stdout).expect("json");
        assert_eq!(doc["task"]["repo"], repo);
        assert_eq!(doc["task"]["alias"], format!("{repo}/1"));
    }
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
    let output = mission_command(&xdg)
        .env("PHUX_PROFILE", "missiontest")
        .args(["mission", "list", "--json"])
        .output()
        .expect("spawn");
    let elapsed = started.elapsed();
    assert!(
        (std::time::Duration::from_millis(1900)..=std::time::Duration::from_secs(6))
            .contains(&elapsed),
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
    let op = |letter: &str| letter.repeat(32);
    let (a, e) = (op("a"), op("e"));
    let add = mission(&xdg, &add_args(&a));
    assert_success("add", &add);
    check(&schema, "add", &add);
    for (verb, args) in [
        ("list", &["mission", "list", "--json"][..]),
        ("show", &["mission", "show", "phux/1", "--json"]),
    ] {
        let output = mission(&xdg, args);
        assert_success(verb, &output);
        check(&schema, verb, &output);
    }
    for (verb, rev, letter) in [
        ("edit", "1", "b"),
        ("archive", "2", "c"),
        ("restore", "3", "d"),
    ] {
        let op = op(letter);
        let mut args = vec!["mission", verb, "phux/1"];
        if verb == "edit" {
            args.extend(["--description", "more detail"]);
        }
        args.extend(["--expected-revision", rev, "--operation-id", &op, "--json"]);
        let output = mission(&xdg, &args);
        assert_success(verb, &output);
        check(&schema, verb, &output);
    }

    let error_doc = |args: &[&str], code: i32, error_code: &str| {
        let output = mission(&xdg, args);
        assert_eq!(output.status.code(), Some(code), "{}", stderr(&output));
        assert!(output.stdout.is_empty());
        let error = serde_json::from_slice::<Value>(&output.stderr).expect("error json");
        assert_eq!(error["schema"], "phux.mission/v1");
        assert_eq!(error["error"]["code"], error_code);
        assert_valid(&schema, &error);
        error
    };
    error_doc(
        &["mission", "show", "phux/99", "--json"],
        1,
        "mission_not_found",
    );
    let at_rev_4 = "phux/1 is at rev 4; rerun with --expected-revision 4";
    let missing_rev = ["mission", "edit", "phux/1", "--title", "nope", "--json"];
    let missing = error_doc(&missing_rev, 2, "usage_error");
    assert_eq!(missing["error"]["message"], at_rev_4);
    let stale = [
        "mission",
        "edit",
        "phux/1",
        "--title",
        "nope",
        "--expected-revision",
        "1",
        "--operation-id",
        &e,
        "--json",
    ];
    let conflict = error_doc(&stale, 1, "mission_revision_conflict");
    assert_eq!(conflict["error"]["message"], at_rev_4);

    let replay = mission(&xdg, &add_args(&a));
    assert_success("replay", &replay);
    let replay_doc = serde_json::from_slice::<Value>(&replay.stdout).expect("replay json");
    assert_eq!(replay_doc["idempotent_replay"], true);
    assert_eq!(replay_doc["task"]["alias"], "phux/1");
    assert_valid(&schema, &replay_doc);
}

#[test]
fn concurrent_adds_go_through_one_keeper_and_none_is_lost() {
    let xdg = common::XdgDirs::fresh("mission-concurrent");
    let children: Vec<_> = (0..8)
        .map(|index| {
            mission_command(&xdg)
                .args([
                    "mission",
                    "add",
                    &format!("task-{index}"),
                    "--repo",
                    "phux",
                    "--json",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn phux")
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
    expect_exit(&xdg, &["mission", "add", "First", "--repo", "phux"], 0);
    let started = std::time::Instant::now();
    expect_exit(&xdg, &["mission", "keeper", "--idle-exit", "30"], 0);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "a second keeper served instead of exiting"
    );
}

fn add_args(op: &str) -> Vec<&str> {
    vec![
        "mission",
        "add",
        "Fix the prompt",
        "--repo",
        "phux",
        "--description",
        "details",
        "--operation-id",
        op,
        "--json",
    ]
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

/// Run `args`, assert the exit code, and return `(stdout, stderr)`.
fn expect_exit(xdg: &common::XdgDirs, args: &[&str], code: i32) -> (String, String) {
    let output = mission(xdg, args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(
        output.status.code(),
        Some(code),
        "{args:?}: {stdout} {}",
        stderr(&output)
    );
    (stdout, stderr(&output))
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
