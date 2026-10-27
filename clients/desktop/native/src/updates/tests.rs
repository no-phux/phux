use super::*;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
mod windows;

const STABLE: &str = include_str!("../../../../../scripts/fixtures/release-manifest/stable.json");
const NEXT: &str = include_str!("../../../../../scripts/fixtures/release-manifest/next.json");

pub(super) fn scratch(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "phux-update-{name}-{}-{nonce} $(touch pwned)",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn phux_moves_with_desktop_by_default_and_latest_is_stable() {
    let prefs = Preferences::from_value(&json!({"desktop":"latest"}));
    assert!(prefs.automatic);
    assert!(prefs.manage_runtime);
    assert_eq!(prefs.desktop, Channel::Stable);
}

#[test]
fn golden_manifests_name_the_desktop_release() {
    let for_host = |body: &str| {
        let mut value: Value = serde_json::from_str(body).unwrap();
        value["products"]["desktop"]["artifacts"][TARGET] =
            value["products"]["desktop"]["artifacts"]["aarch64-apple-darwin"].clone();
        value.to_string()
    };
    let stable = manifest_release(&for_host(STABLE), Channel::Stable).unwrap();
    assert_eq!(stable.version, "1.2.3");
    assert_eq!(stable.revision, "b".repeat(40));
    assert_eq!(stable.sha256, "d".repeat(64));
    assert_eq!(stable.protocol, "0.9");
    let next = manifest_release(&for_host(NEXT), Channel::Next).unwrap();
    assert_eq!(next.channel, Channel::Next);
    assert!(manifest_release(STABLE, Channel::Next).is_err());
}

#[test]
fn a_channel_without_desktop_reads_as_unpublished() {
    let mut document: Value = serde_json::from_str(NEXT).unwrap();
    document["products"]
        .as_object_mut()
        .unwrap()
        .remove("desktop");
    let error = manifest_release(&document.to_string(), Channel::Next).unwrap_err();
    assert_eq!(
        error.to_string(),
        "No Desktop Next build is published yet. Check again later."
    );
}

#[test]
fn channel_switches_allow_return_from_next() {
    let stable = Release {
        version: "1.2.3".into(),
        revision: "a".repeat(40),
        ..Release::default()
    };
    let next = Release {
        revision: "b".repeat(40),
        channel: Channel::Next,
        ..stable.clone()
    };
    assert!(available(&next, &stable));
    assert!(available(&stable, &next));
    assert!(!available(&next, &next));
    assert!(!available(&stable, &stable));
    let older = Release {
        version: "1.2.2".into(),
        ..stable.clone()
    };
    assert!(!available(&stable, &older));
    assert!(available(&older, &stable));
}

#[test]
fn portable_bundle_version_and_identity_are_checked() {
    let dir = scratch("linux-release");
    let bundle = dir.join("phux-desktop");
    let metadata = bundle.join("share/phux-desktop/release.json");
    std::fs::create_dir_all(metadata.parent().unwrap()).unwrap();
    let mut value = json!({"identifier":"dev.phux.desktop", "version":"1.2.3",
            "sha":"b".repeat(40), "channel":"next", "target":"x86_64-unknown-linux-gnu"});
    std::fs::write(&metadata, value.to_string()).unwrap();
    let installed = linux_release(&bundle).unwrap();
    assert_eq!(installed.version, "1.2.3");
    assert_eq!(installed.channel, Channel::Next);
    let executable = bundle.join("bin/phux-desktop");
    assert_eq!(linux_bundle(&executable).unwrap(), bundle);
    assert!(linux_bundle(Path::new("target/debug/phux-desktop")).is_none());
    value["identifier"] = json!("dev.example.other");
    std::fs::write(&metadata, value.to_string()).unwrap();
    assert!(linux_bundle(&executable).is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn windows_bundle_requires_its_layout_identity_and_msvc_target() {
    let dir = scratch("windows-release");
    let bundle = dir.join("phux-desktop");
    std::fs::create_dir_all(&bundle).unwrap();
    let metadata = bundle.join("share/phux-desktop/release.json");
    std::fs::create_dir_all(metadata.parent().unwrap()).unwrap();
    let executable = bundle.join("phux-desktop.exe");
    assert!(windows_bundle(&executable).is_none());
    assert!(windows_bundle(Path::new("target/debug/phux-desktop.exe")).is_none());
    let value = json!({"identifier":"dev.phux.desktop", "version":"1.2.3",
        "sha":"b".repeat(40), "channel":"next", "target":WINDOWS_TARGET});
    std::fs::write(&metadata, value.to_string()).unwrap();
    assert_eq!(windows_bundle(&executable), Some(bundle.clone()));
    let current = dir.join("Phux/current");
    std::fs::create_dir_all(current.parent().unwrap()).unwrap();
    std::fs::rename(&bundle, &current).unwrap();
    assert_eq!(
        windows_bundle(&current.join("phux-desktop.exe")),
        Some(current.clone())
    );
    std::fs::rename(&current, &bundle).unwrap();
    assert_eq!(windows_release(&bundle).unwrap().channel, Channel::Next);
    for (field, invalid) in [
        ("identifier", "dev.example.other"),
        ("version", "1.2.3-unverified"),
        ("sha", "not-a-revision"),
        ("channel", "unknown"),
        ("target", "x86_64-unknown-linux-gnu"),
    ] {
        let mut invalid_value = value.clone();
        invalid_value[field] = json!(invalid);
        std::fs::write(&metadata, invalid_value.to_string()).unwrap();
        assert!(windows_bundle(&executable).is_none(), "accepted {field}");
    }
    let mut missing_target = value;
    missing_target.as_object_mut().unwrap().remove("target");
    std::fs::write(&metadata, missing_target.to_string()).unwrap();
    assert!(windows_bundle(&executable).is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn windows_manual_update_names_the_signed_installer_and_quotes_the_destination() {
    let remedy = windows_install_remedy(Channel::Next, None);
    assert!(remedy.contains("not supported on Windows"));
    assert!(remedy.contains("PowerShell"));
    assert!(remedy.contains("minisign"));
    assert!(remedy.contains("verifies release signatures"));
    assert!(remedy.contains("main/scripts/install-desktop-windows.ps1"));
    assert!(remedy.ends_with("-Channel next"));
    let remedy = windows_install_remedy(
        Channel::Stable,
        Some(Path::new("C:/custom apps/person's desktop/phux-desktop")),
    );
    assert!(remedy.ends_with("-InstallDir 'C:/custom apps/person''s desktop/phux-desktop'"));
    let remedy = windows_install_remedy(
        Channel::Stable,
        Some(Path::new("C:/custom apps/person's desktop/current")),
    );
    assert!(remedy.ends_with("-InstallDir 'C:/custom apps/person''s desktop'"));
}

#[cfg(windows)]
#[test]
fn windows_never_runs_the_macos_swap_or_staging_installer() {
    assert_eq!(TARGET, WINDOWS_TARGET);
    assert_eq!(CURL, "curl.exe");
    let path = Path::new("C:/does-not-exist/phux-desktop");
    let error = stage(path, &Release::default(), std::sync::mpsc::channel().0).unwrap_err();
    assert!(error.to_string().contains("install-desktop-windows.ps1"));
    let error = apply_on_exit(path, path, true).unwrap_err();
    assert!(error.to_string().contains("not supported on Windows"));
    clear_stale_stage(path);
}

#[test]
fn linux_manual_update_requires_release_signatures() {
    let remedy = linux_install_remedy(Channel::Next, None);
    assert!(remedy.contains("PHUX_REQUIRE_SIGNATURE=1"));
    assert!(remedy.contains("--desktop --channel next"));
    assert!(remedy.contains("minisign"));
    let bundle = Path::new("/custom apps/person's desktop/phux-desktop");
    let remedy = linux_install_remedy(Channel::Stable, Some(bundle));
    let expected = crate::attach::quote_paths(&[bundle.parent().unwrap().to_path_buf()]);
    assert!(remedy.ends_with(&format!("--applications-dir {expected}")));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_never_runs_the_macos_swap_or_staging_installer() {
    let path = Path::new("/does-not-exist/phux-desktop");
    assert!(
        stage(path, &Release::default(), std::sync::mpsc::channel().0)
            .unwrap_err()
            .to_string()
            .contains("PHUX_REQUIRE_SIGNATURE=1")
    );
    assert!(
        apply_on_exit(path, path, true)
            .unwrap_err()
            .to_string()
            .contains("not supported on Linux")
    );
    clear_stale_stage(path);
}

#[test]
fn runtime_handoff_refusal_is_not_a_restart() {
    let refused = json!({"schema_version":2,"server_handoff":{"result":"refused","detail":"busy"}});
    let result = runtime_result(&refused).unwrap();
    assert!(!result.reconnect);
    assert!(result.retry_handoff);
    assert!(result.message.contains("busy"));
    assert!(runtime_result(&json!({"schema_version":3})).is_err());
    let accepted = json!({"schema_version":1,"server_handoff":{"result":"upgrading"}});
    assert!(runtime_result(&accepted).unwrap().reconnect);
}

#[test]
fn runtime_protocol_is_optional() {
    let base = json!({"schema_version":2,"current_version":"1.0.0","target_version":"v1.1.0",
            "update_available":true,"install":{"mutable":true}});
    let old = runtime_release(&base).unwrap();
    assert_eq!(old.protocol_current, None);
    let mut new = base;
    new["protocol"] = json!({"current":"0.9","target":"0.10"});
    let release = runtime_release(&new).unwrap();
    assert_eq!(release.protocol_current.as_deref(), Some("0.9"));
    assert_eq!(release.protocol_target.as_deref(), Some("0.10"));
}

#[test]
fn windows_runtime_checks_are_read_only_and_quote_the_manual_wsl_update() {
    let mut wsl = Command::new("wsl.exe");
    wsl.args([
        "--distribution",
        "Team's Ubuntu λ $(unsafe)",
        "--exec",
        "/home/person's bin/phux",
    ]);
    let command = runtime::manual_runtime_update(
        wsl,
        Path::new("/run/person's coordinator.sock"),
        Channel::Next,
    );
    let release = runtime_release(&json!({
        "schema_version":2, "current_version":"1.0.0", "target_version":"2.0.0",
        "update_available":true, "install":{"mutable":true},
    }))
    .unwrap();
    let readonly = runtime::windows_runtime_policy(release, &command);
    assert!(readonly.available);
    assert!(
        !readonly.mutable,
        "a Linux updater must never be started by Desktop on Windows"
    );
    let remedy = readonly.native_command.unwrap();
    assert!(remedy.starts_with("& 'wsl.exe' '--distribution' 'Team''s Ubuntu λ $(unsafe)' '--exec' 'sh' '-c' 'IFS=;set -f;eval $(printf %s "));
    assert!(
        !remedy.contains('"'),
        "PowerShell 5.1 must not lose nested shell quotes"
    );
    let encoded = remedy
        .split("printf %s ")
        .nth(1)
        .unwrap()
        .split('|')
        .next()
        .unwrap();
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert_eq!(
        String::from_utf8(decoded).unwrap(),
        "exec '/home/person'\\''s bin/phux' '--socket' '/run/person'\\''s coordinator.sock' 'update' '--channel' 'next'"
    );
}

#[cfg(windows)]
#[test]
fn windows_runtime_install_and_retry_refuse_before_starting_wsl() {
    let phux = Path::new("/nonexistent/person's bin/phux");
    let socket = Path::new("/nonexistent/person's coordinator.sock");
    let install = runtime_install(phux, socket, Channel::Next)
        .err()
        .unwrap()
        .to_string();
    let retry = runtime_retry(phux, socket).err().unwrap().to_string();
    assert!(install.starts_with("Automatic runtime updates are disabled on Windows"));
    assert!(retry.starts_with("Automatic server handoff is disabled on Windows"));
    for remedy in [&install, &retry] {
        assert!(remedy.contains("'wsl.exe'"));
        assert!(remedy.contains("'--exec' 'sh' '-c' 'IFS=;set -f;eval $(printf %s "));
        if let Some(distro) = std::env::var_os("PHUX_WSL_DISTRO").filter(|name| !name.is_empty()) {
            assert!(remedy.contains(&format!(
                "'--distribution' '{}'",
                distro.to_string_lossy().replace('\'', "''")
            )));
        }
    }
}

#[cfg(unix)]
#[test]
fn manual_wsl_wrapper_preserves_nested_quotes_and_argument_boundaries() {
    let mut command = Command::new("wsl.exe");
    command.args([
        "--exec",
        "sh",
        "-c",
        "exec \"$@\"",
        "phux-desktop",
        "/usr/bin/printf",
        "<%s>",
        "a 'quote' $(never-executed) λ",
        "two words",
    ]);
    let remedy = runtime::powershell_command(&command);
    assert!(!remedy.contains('"'));
    let script = remedy
        .split("'sh' '-c' '")
        .nth(1)
        .unwrap()
        .strip_suffix('\'')
        .unwrap();
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "<a 'quote' $(never-executed) λ><two words>"
    );
}

#[test]
fn structured_failure_has_readable_remedy() {
    assert_eq!(
        failure_message(
            br#"{"error":{"message":"Cannot update"},"remedy":"Use brew upgrade phux"}"#
        ),
        "Cannot update Use brew upgrade phux"
    );
    assert_eq!(
        failure_message(b"network unavailable\n"),
        "network unavailable"
    );
}

#[cfg(unix)]
#[test]
fn a_timed_out_command_has_its_process_group_killed() {
    let start = std::time::Instant::now();
    let result = run_bounded(
        Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]),
        None,
        Duration::from_millis(30),
        "timed out",
    );
    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[cfg(unix)]
#[test]
fn bounded_runs_feed_stdin() {
    let text = run_bounded(
        Command::new("/bin/sh").arg("-s"),
        Some(b"echo fed\n"),
        Duration::from_secs(10),
        "timed out",
    )
    .unwrap();
    assert_eq!(text, "fed\n");
}

#[cfg(windows)]
#[test]
fn a_timed_out_windows_command_has_only_its_recorded_tree_cancelled() {
    let mut unrelated = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let dir = scratch("windows-timeout");
    let marker = dir.join("survived");
    let start = std::time::Instant::now();
    let result = run_bounded(
        Command::new("powershell.exe").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 2; [IO.File]::WriteAllText($env:PHUX_UPDATE_TEST_MARKER, 'survived')",
        ]).env("PHUX_UPDATE_TEST_MARKER", &marker),
        None,
        Duration::from_millis(100),
        "timed out",
    );
    let elapsed = start.elapsed();
    let unrelated_survived = unrelated.try_wait().unwrap().is_none();
    let _ = unrelated.kill();
    let _ = unrelated.wait();
    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(elapsed < Duration::from_secs(5));
    assert!(
        unrelated_survived,
        "cancellation killed an unrelated process"
    );
    std::thread::sleep(Duration::from_secs(3));
    assert!(!marker.exists(), "timed-out child was left running");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(windows)]
#[test]
fn bounded_windows_runs_feed_stdin() {
    let text = run_bounded(
        Command::new("powershell.exe").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::Write([Console]::In.ReadToEnd())",
        ]),
        Some(b"fed"),
        Duration::from_secs(10),
        "timed out",
    )
    .unwrap();
    assert_eq!(text, "fed");
}

#[cfg(unix)]
#[test]
fn runtime_update_resolves_an_empty_socket_before_installing_or_retrying() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("runtime-first-launch");
    let phux = dir.join("phux");
    let record = dir.join("args");
    let socket = dir.join("resolved.sock");
    std::fs::write(&phux, format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\ncase \"$*\" in\n 'status --json') printf '%s\\n' '{{\"socket\":\"{}\"}}' ;;\n *'update --channel'*) printf '%s\\n' '{{\"schema_version\":2,\"server_handoff\":{{\"result\":\"upgrading\"}}}}' ;;\nesac\n",
        record.display(), socket.display()
    )).unwrap();
    std::fs::set_permissions(&phux, std::fs::Permissions::from_mode(0o700)).unwrap();
    let installed = runtime_install(&phux, Path::new(""), Channel::Stable).unwrap();
    assert_eq!(installed.socket, socket);
    assert!(installed.reconnect);
    let retried = runtime_retry(&phux, Path::new("")).unwrap();
    assert_eq!(retried.socket, socket);
    let calls = std::fs::read_to_string(record).unwrap();
    assert!(
        calls.starts_with("server\n--ensure\nstatus\n--json\n"),
        "{calls}"
    );
    assert!(!calls.contains("--socket\n\n"), "{calls}");
    assert!(
        calls.contains(&format!("--socket\n{}\nupdate", socket.display())),
        "{calls}"
    );
    assert!(
        calls.contains(&format!("--socket\n{}\nupgrade", socket.display())),
        "{calls}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn runtime_commands_use_explicit_socket_and_never_close_agents() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("runtime");
    let phux = dir.join("phux");
    let record = dir.join("args");
    let response = dir.join("response.json");
    let document = json!({"schema_version":2,"current_version":"1.0.0","target_version":"next","update_available":true,"install":{"mutable":true},"server_handoff":{"result":"upgrading"}});
    std::fs::write(&response, document.to_string()).unwrap();
    std::fs::write(
        &phux,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat '{}'\n",
            record.display(),
            response.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&phux, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        runtime_check(&phux, &dir.join("isolated.sock"), Channel::Next)
            .unwrap()
            .available
    );
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        "update\n--check\n--channel\nnext\n--json\n"
    );
    assert!(
        runtime_install(&phux, &dir.join("isolated.sock"), Channel::Stable)
            .unwrap()
            .reconnect
    );
    let args = std::fs::read_to_string(&record).unwrap();
    assert!(args.starts_with(&format!(
        "--socket\n{}\nupdate\n",
        dir.join("isolated.sock").display()
    )));
    assert!(!args.contains("close-agent-sessions"));
    assert!(
        runtime_retry(&phux, &dir.join("isolated.sock"))
            .unwrap()
            .reconnect
    );
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        format!(
            "--socket\n{}\nupgrade\n",
            dir.join("isolated.sock").display()
        )
    );
    std::fs::write(
        &response,
        json!({"schema_version":2,"server_handoff":{"result":"no_server"}}).to_string(),
    )
    .unwrap();
    assert!(
        runtime_install(&phux, &dir.join("isolated.sock"), Channel::Stable)
            .unwrap()
            .reconnect
    );
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        format!(
            "--socket\n{}\nserver\n--ensure\n",
            dir.join("isolated.sock").display()
        )
    );
    std::fs::remove_dir_all(dir).unwrap();
}
