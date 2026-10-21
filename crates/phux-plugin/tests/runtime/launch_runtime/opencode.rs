#[test]
fn native_opencode_fallback_needs_no_plugin_and_uses_official_acp() {
    let tmp = TempDir::new().expect("tempdir");
    let config = tmp.path().join("missing-config.toml");
    let ws = workspace(&tmp);
    let items =
        phux_plugin::list_launchable(&config, phux_plugin::LaunchMode::Native).expect("list");
    let item = items
        .iter()
        .find(|item| item.integration_id == "opencode")
        .expect("built-in");
    assert_eq!(item.plugin_id, "phux.builtin");
    assert_eq!(item.native.as_ref().expect("native").provider, "opencode");
    let resolved = resolve_launch(
        &config,
        "opencode",
        &[],
        &ws,
        phux_plugin::LaunchMode::Native,
    );
    if item.missing_executables.is_empty() {
        let resolved = resolved.expect("installed OpenCode");
        assert!(Path::new(&resolved.argv[0]).is_absolute());
        assert_eq!(resolved.argv[1], "acp");
        assert_eq!(resolved.cwd, ws);
    } else {
        assert!(matches!(
            resolved,
            Err(LaunchError::MissingExecutables { .. })
        ));
    }
}

#[test]
fn explicit_opencode_native_profile_overrides_fallback_and_invalid_never_falls_back() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, root) = write_plugin(&tmp, true);
    let path = root.join("integrations/opencode.toml");
    let ws = workspace(&tmp);
    std::fs::write(
        &path,
        r#"
id = "opencode"
[acp]
provider = "opencode-custom"
command = ["sh", "custom-acp"]
required_executables = ["sh"]
"#,
    )
    .expect("custom profile");
    let resolve = || {
        resolve_launch(
            &config,
            "opencode",
            &[],
            &ws,
            phux_plugin::LaunchMode::Native,
        )
    };
    let resolved = resolve().expect("configured native");
    assert_ne!(resolved.plugin_id, "phux.builtin");
    assert_eq!(resolved.argv, ["sh", "custom-acp"]);
    std::fs::copy(&path, root.join("integrations/duplicate.toml")).expect("duplicate");
    assert!(matches!(
        resolve(),
        Err(LaunchError::DuplicateIntegrationId { .. })
    ));
    std::fs::write(&path, "invalid [").expect("break profile");
    assert!(matches!(resolve(), Err(LaunchError::Template { .. })));
}
