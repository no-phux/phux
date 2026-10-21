use super::*;

#[cfg(target_os = "macos")]
mod macos;

const STABLE: &str = include_str!("../../../../../scripts/fixtures/release-manifest/stable.json");
const NEXT: &str = include_str!("../../../../../scripts/fixtures/release-manifest/next.json");

fn scratch(name: &str) -> PathBuf {
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
        stage(path, &Release::default())
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

#[cfg(unix)]
#[test]
fn runtime_commands_use_explicit_socket_and_never_close_agents() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("runtime");
    let phux = dir.join("phux");
    let record = dir.join("args");
    let document = json!({"schema_version":2,"current_version":"1.0.0","target_version":"next","update_available":true,"install":{"mutable":true},"server_handoff":{"result":"upgrading"}});
    std::fs::write(
        &phux,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat <<'JSON'\n{document}\nJSON\n",
            record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&phux, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(runtime_check(&phux, Channel::Next).unwrap().available);
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
    std::fs::remove_dir_all(dir).unwrap();
}
