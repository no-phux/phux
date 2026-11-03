/// Write an executable named `name` that emits one OSC 7501 report and holds.
/// Nothing else is painted, so the agent record it projects comes from the
/// report alone (docs/any-program-status.md).
fn write_reporter(dir: &std::path::Path, name: &str, report: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    let script = format!("#!/bin/sh\nprintf '\\033]7501;{report}\\033\\\\'\nsleep 120\n");
    std::fs::write(&path, script).expect("write reporter");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod reporter");
    path
}

/// #1869: the page tells tool authors to send `title`, because phux projects
/// `title` into the pane's agent name and `app` into its kind. Without a
/// `title` the name falls back to `app`, and a name equal to its kind is a
/// per-kind constant that `%name` refuses with `invalid_agent_name`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn program_status_title_is_the_agent_name_and_app_alone_is_not_addressable() {
    let home = tempfile::tempdir().expect("create temp dir");
    // `ZGVwbG95` is base64 for `deploy`.
    let titled = write_reporter(
        home.path(),
        "titled",
        "state=blocked:kind=permission:app=mytool:title=ZGVwbG95",
    );
    let untitled = write_reporter(
        home.path(),
        "untitled",
        "state=blocked:kind=permission:app=mytool",
    );
    let server = ServerGuard::start("agent");

    let titled_id = server.spawn_pane(&titled);
    let target = format!("@{titled_id}");
    let json = server.await_agent_state(&target, "blocked", DETECT_DEADLINE);
    let agent = &json["agents"][0];
    assert_eq!(agent["agent"]["label"], "deploy", "title is the name: {json}");
    assert_eq!(agent["agent"]["id"], "mytool", "app is the kind: {json}");
    assert_eq!(agent["address"], "%deploy", "{json}");
    assert_eq!(agent["address_refusal"], serde_json::Value::Null, "{json}");
    let by_name = server.agent_show("%deploy");
    assert_eq!(
        by_name["agents"][0]["terminal"], agent["terminal"],
        "%deploy must resolve to the reporting pane: {by_name}"
    );
    let (code, _, stderr) = server.run(&["tag", "ls", "%deploy"]);
    assert_eq!(code, 0, "`tag ls %deploy` must reach the pane: {stderr}");

    let untitled_id = server.spawn_pane(&untitled);
    let target = format!("@{untitled_id}");
    let json = server.await_agent_state(&target, "blocked", DETECT_DEADLINE);
    let agent = &json["agents"][0];
    assert_eq!(agent["agent"]["label"], "mytool", "no title: name falls back to app: {json}");
    assert_eq!(agent["agent"]["id"], "mytool", "{json}");
    assert_eq!(agent["address"], serde_json::Value::Null, "{json}");
    assert_eq!(
        agent["address_refusal"], "invalid_agent_name",
        "a name equal to its kind is a per-kind constant: {json}"
    );
    let (code, _, stderr) = server.run(&["tag", "ls", "%mytool"]);
    assert_eq!(code, 2, "`tag ls %mytool` must refuse the kind constant: {stderr}");
}
