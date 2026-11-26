//! Restore admits only a current trusted host template or an exact
//! local-owner review of the raw recipe ([organization]).
//!
//! [organization]: ../../../../../docs/organization.md#restore-and-executable-authority

use super::*;

/// `phux restore NAME` while no current trusted template admits its pane:
/// exit 2, the host's refusal, and the exact review command, with nothing
/// started. Returns the pane's key as the review command names it.
fn restore_refuses_untrusted(socket: &str, name: &str, xdg: Option<&Path>) -> String {
    let (code, stdout, stderr) = run_in(&["restore", name, "--socket", socket], xdg);
    assert_eq!(
        code, 2,
        "an untrusted recipe must be refused: {stdout} {stderr}"
    );
    assert!(
        stdout.contains(&format!("relaunched 0 of 1 pane in {name}"))
            && stdout.contains("no current trusted host template admits this recipe"),
        "restore must report the host's refusal: {stdout}"
    );
    assert!(
        live_terminals(socket, name).is_empty(),
        "a refused restore must start nothing"
    );
    let review = stdout
        .lines()
        .find_map(|line| {
            line.split_once("review and approve it: phux relaunch ")?
                .1
                .split_once(' ')
        })
        .unwrap_or_else(|| panic!("restore must name the exact review command: {stdout}"));
    assert_eq!(review.1, "--approve-raw", "{stdout}");
    review.0.to_owned()
}

/// Enable the fixture's integrations on the destination host itself: the
/// host's current template, not the archive or a client's config, admits
/// a resume.
fn enable_on_host(host: &common::ServerGuard, xdg: &Path) {
    let config = host.xdg.config.join("phux");
    std::fs::create_dir_all(&config).expect("host config directory");
    std::fs::copy(xdg.join("phux/config.toml"), config.join("config.toml"))
        .expect("enable the integration on the host");
}

#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn import_then_restore_starts_archived_command_process() {
    let dest =
        common::raw_review::hermetic_server(common::ServerGuard::builder("wa").session("seed"))
            .start();
    let archive_dir = tempfile::tempdir().expect("archive tempdir");
    let archive_path = archive_dir.path().join("workspace-command.json");
    let cwd = archive_dir.path().to_string_lossy().into_owned();
    let ran = archive_dir.path().join("ran");
    let marker = format!(
        "PHUX_RESTORED_PROCESS_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    );
    let command = [
        "sh".to_owned(),
        "-lc".to_owned(),
        format!("printf '%s\\nPWD=%s\\n' {marker} \"$PWD\"; : > ran; sleep 30"),
    ];
    let pane = serde_json::json!({
        "active": true, "cwd": cwd, "command": command, "cols": 80, "rows": 24
    });
    let window = serde_json::json!({
        "name": "main", "active": true, "layout": { "kind": "pane", "pane": 0 }, "panes": [pane]
    });
    let session = serde_json::json!({
        "name": "restored-proc", "active": true, "cwd": cwd, "command": command, "windows": [window]
    });
    let archive = serde_json::json!({ "schema_version": 1, "sessions": [session] });
    std::fs::write(&archive_path, archive.to_string()).expect("write archive");
    let socket = dest.socket_text();
    import(&archive_path, &socket, "restored-proc", None);
    let key = restore_refuses_untrusted(&socket, "restored-proc", None);

    // No terminal, or anything but a typed yes, approves nothing.
    let (code, _, stderr) = run(&["relaunch", "--socket", &socket, &key, "--approve-raw"]);
    assert_eq!(code, 2, "a review without a terminal must refuse: {stderr}");
    assert!(stderr.contains("there is no --yes"), "{stderr}");
    let declined = common::raw_review::review(&dest.socket, &key, "n");
    assert_eq!(declined.code, 2, "{}", declined.transcript);
    assert!(
        declined.transcript.contains(&marker),
        "the review must show the exact archived command: {}",
        declined.transcript
    );
    assert!(live_terminals(&socket, "restored-proc").is_empty());
    assert!(!ran.exists(), "nothing may run before an explicit approval");

    common::raw_review::approve(&dest.socket, &key);

    // Ambient budget: the marker is unique, so only load can expire it.
    let (code, stderr) = wait_until(&socket, "restored-proc", &marker, AGENT_PAINT_BUDGET_SECS);
    assert_eq!(code, 0, "restored command marker did not appear: {stderr}");
    let snapshot = ok(
        &["snapshot", "--json", "--socket", &socket, "restored-proc"],
        None,
    );
    assert!(
        snapshot.contains(&marker),
        "snapshot should show restored command output"
    );
    assert!(
        snapshot.contains(&cwd),
        "snapshot should show restored command cwd"
    );
    assert!(ran.exists(), "the approved recipe ran in its archived cwd");
}

const RESTORE_AGENT: &str = r#"schema_version = 1
id = "restore-agent"
display_name = "Restore Agent"
kind = "terminal-agent"
first_party = true

[session_identity]
mode = "native-or-phux"
native_env = "PHUX_FAKE_SESSION_ID"
restore = "external-cli"
resume_args = ["--resume", "${PHUX_AGENT_SESSION_ID}"]
fresh_args = ["--new", "${PHUX_AGENT_SESSION_ID}"]

[launch]
command = ["${PHUX_PLUGIN_ROOT}/scripts/fake-agent.sh"]
working_directory = "plugin-root"
"#;

/// Claims the `fakeprov` provider that a shell-started agent's
/// `AgentSession` names, so save can bridge it (ADR-0151).
const SHELL_RESUME_AGENT: &str = r#"schema_version = 1
id = "shell-resume-agent"
display_name = "Shell-started resume agent"
kind = "terminal-agent"
first_party = true

[session_identity]
mode = "native-or-phux"
native_env = "PHUX_FAKE_SESSION_ID"
restore = "external-cli"
resume_args = ["--resume", "${PHUX_AGENT_SESSION_ID}"]

[agent_identity]
name = "fakeprov"
kind = "fakeprov"

[launch]
command = ["${PHUX_PLUGIN_ROOT}/scripts/fake-agent.sh"]
working_directory = "workspace"
"#;

fn write_fake_agent_plugin(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let plugin = root.join("plugin");
    let xdg = root.join("xdg");
    for dir in [
        plugin.join("integrations"),
        plugin.join("scripts"),
        xdg.join("phux"),
    ] {
        std::fs::create_dir_all(dir).expect("create fixture directory");
    }
    let script = plugin.join("scripts/fake-agent.sh");
    let manifest = plugin.join("phux-plugin.toml");
    for (path, contents) in [
        (
            manifest.clone(),
            "id = \"com.phux.test.restore\"\nname = \"Restore test agent\"\n\
             version = \"1.0.0\"\nmin_phux_version = \"0.0.2\"\n\
             platforms = [\"linux\", \"macos\"]\n",
        ),
        (
            plugin.join("integrations/restore-agent.toml"),
            RESTORE_AGENT,
        ),
        (
            plugin.join("integrations/shell-resume-agent.toml"),
            SHELL_RESUME_AGENT,
        ),
        (
            script.clone(),
            "#!/bin/sh\nprintf 'FAKE_AGENT_ARGS=%s\\n' \"$*\"\n\
             printf 'FAKE_AGENT_ENV=%s\\n' \"$PHUX_FAKE_SESSION_ID\"\n\
             printf 'FAKE_AGENT_CWD=%s\\n' \"$PWD\"\nexec sleep 60\n",
        ),
    ] {
        std::fs::write(path, contents).expect("write plugin fixture");
    }
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))
        .expect("make fake agent executable");
    std::fs::write(
        xdg.join("phux/config.toml"),
        format!(
            "[[plugins]]\nmanifest = {:?}\nenabled = true\n",
            manifest.to_string_lossy()
        ),
    )
    .expect("write config");
    xdg
}

/// An imported agent pane resumes only through the destination host's current
/// integration template: refused while the host lacks it, resumed once enabled.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn native_agent_session_is_replayed_after_pane_restart_and_rejects_stale_ownership() {
    let root = tempfile::tempdir().expect("restore test tempdir");
    let config = write_fake_agent_plugin(root.path());
    let xdg = Some(config.as_path());
    let source_archive = root.path().join("source.json");

    let source = start("agent-restart");
    let source_socket = source.socket_text();
    let launch = ok(
        &[
            "launch",
            "restore-agent",
            "--socket",
            &source_socket,
            "--json",
        ],
        xdg,
    );
    let launch: Value = serde_json::from_str(&launch).expect("launch JSON");
    assert_eq!(launch["integration"], "restore-agent");
    let launched_id = launch["terminal_id"].as_u64().expect("terminal id");
    wait_painted_then(
        &source_socket,
        &format!("@{launched_id}"),
        "FAKE_AGENT_ARGS=--new",
    );
    // Remove the pre-agent seed pane before save.
    kill_and_wait_absent(&source_socket, "@1");
    let mut saved = export(&source_socket, &source_archive, xdg);
    assert_eq!(saved["schema_version"], 3);
    let native_id =
        archived(&saved, "agent-restart")["windows"][0]["panes"][0]["agent_session"]["native_id"]
            .as_str()
            .expect("saved agent session native id")
            .to_owned();
    assert!(
        uuid::Uuid::parse_str(&native_id).is_ok(),
        "fresh identity is a UUID: {native_id}"
    );
    let pane = &mut archived_mut(&mut saved, "agent-restart")["windows"][0]["panes"][0];
    pane["cwd"] = Value::from("/archived/cwd/must-not-win");
    std::fs::write(&source_archive, saved.to_string()).expect("write cwd-edited archive");
    drop(source);

    let dest = start("replay-seed");
    let dest_socket = dest.socket_text();
    // The client's config names the integration; the host's does not yet.
    import(&source_archive, &dest_socket, "agent-restart", xdg);
    restore_refuses_untrusted(&dest_socket, "agent-restart", xdg);
    enable_on_host(&dest, &config);
    ok(&["restore", "agent-restart", "--socket", &dest_socket], xdg);
    let resume_marker = format!("FAKE_AGENT_ARGS=--resume {native_id}");
    wait_painted_then(&dest_socket, "agent-restart", &resume_marker);
    let plugin_cwd = root
        .path()
        .join("plugin")
        .canonicalize()
        .expect("canonical plugin root");
    let cwd_marker = format!("FAKE_AGENT_CWD={}", plugin_cwd.display());
    let (code, stderr) = wait_until(
        &dest_socket,
        "agent-restart",
        &cwd_marker,
        AGENT_LINE_BUDGET_SECS,
    );
    assert_eq!(
        code, 0,
        "restore must use current integration working directory: {stderr}"
    );
    let replayed = export(&dest_socket, &root.path().join("replay.json"), xdg);
    let replayed = &archived(&replayed, "agent-restart")["windows"][0]["panes"][0]["agent_session"];
    assert_eq!(replayed["native_id"].as_str(), Some(native_id.as_str()));

    let agent =
        &mut archived_mut(&mut saved, "agent-restart")["windows"][0]["panes"][0]["agent_session"];
    agent["plugin_id"] = Value::from("com.phux.wrong-owner");
    let stale_archive = root.path().join("stale.json");
    std::fs::write(&stale_archive, saved.to_string()).expect("write stale archive");
    // Import is inert and resolves no template. The host enables the real
    // integration, whose owner differs from the archived one: restore must not
    // run the archived intent through it.
    let stale_dest = start("stale-seed");
    enable_on_host(&stale_dest, &config);
    let stale_socket = stale_dest.socket_text();
    import(&stale_archive, &stale_socket, "agent-restart", xdg);
    restore_refuses_untrusted(&stale_socket, "agent-restart", xdg);
}

/// ADR-0151: an agent started inside an existing shell has no launch record,
/// only a live `AgentSession` with a native id. Save bridges it into the
/// archived resume record through the integration claiming its provider, and
/// restore resumes it through that integration's current argv, once the
/// destination host itself enables the integration.
#[test]
#[ignore = "spawns real phux servers; run explicitly when validating workspace archives."]
fn shell_started_agent_session_is_archived_and_resumed() {
    let root = tempfile::tempdir().expect("restore test tempdir");
    let config = write_fake_agent_plugin(root.path());
    let xdg = Some(config.as_path());
    let archive = root.path().join("shell-agent.json");

    let source = start("shell-agent");
    let source_socket = source.socket_text();
    ok(
        &[
            "agent",
            "session",
            "open",
            "@1",
            "--provider",
            "fakeprov",
            "--native-id",
            "conv-7",
            "--socket",
            &source_socket,
        ],
        None,
    );
    let saved = export(&source_socket, &archive, xdg);
    assert_eq!(
        archived(&saved, "shell-agent")["windows"][0]["panes"][0]["agent_session"],
        serde_json::json!({
            "plugin_id": "com.phux.test.restore",
            "integration_id": "shell-resume-agent",
            "native_id": "conv-7",
        }),
        "the live session is archived as the inert resume record"
    );
    assert!(
        !saved.to_string().contains("fake-agent.sh"),
        "the archive never carries executable argv: {saved}"
    );
    drop(source);

    let dest = start("shell-agent-seed");
    let dest_socket = dest.socket_text();
    let path_arg = archive.to_str().expect("utf-8 archive");
    ok(
        &["archive", "import", path_arg, "--socket", &dest_socket],
        xdg,
    );
    restore_refuses_untrusted(&dest_socket, "shell-agent", xdg);
    enable_on_host(&dest, &config);
    ok(&["restore", "shell-agent", "--socket", &dest_socket], xdg);
    wait_painted_then(
        &dest_socket,
        "shell-agent",
        "FAKE_AGENT_ARGS=--resume conv-7",
    );
}
