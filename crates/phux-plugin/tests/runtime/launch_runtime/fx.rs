#[test]
fn native_fx_fallback_uses_official_acp_and_requires_fx_on_the_host() {
    let tmp = TempDir::new().expect("tempdir");
    let config = tmp.path().join("missing-config.toml");
    let ws = workspace(&tmp);
    let items = phux_plugin::list_launchable(&config, phux_plugin::LaunchMode::Native).expect("list");
    let item = items.iter().find(|item| item.integration_id == "fx").expect("built-in");
    assert_eq!(item.plugin_id, "phux.builtin");
    assert_eq!(item.native.as_ref().expect("native").provider, "fx");
    assert_eq!(item.agent_identity.as_ref().expect("identity").kind.as_deref(), Some("fx"));
    let resolved = resolve_launch(&config, "fx", &[], &ws, phux_plugin::LaunchMode::Native);
    if item.missing_executables.is_empty() {
        let resolved = resolved.expect("installed fx");
        assert_eq!(resolved.argv, ["fx", "acp"]);
        assert_eq!(resolved.cwd, ws);
    } else {
        assert!(matches!(resolved, Err(LaunchError::MissingExecutables { .. })));
    }
}

#[test]
fn explicit_fx_profile_wins_and_broken_profile_does_not_fall_back() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, root) = write_plugin(&tmp, true);
    let path = root.join("integrations/fx.toml");
    let ws = workspace(&tmp);
    std::fs::write(&path, "id = 'fx'\n[acp]\nprovider = 'fx-custom'\ncommand = ['sh', 'custom-acp']\nrequired_executables = ['sh']\n").expect("profile");
    let resolve = || resolve_launch(&config, "fx", &[], &ws, phux_plugin::LaunchMode::Native);
    let resolved = resolve().expect("custom fx");
    assert_ne!(resolved.plugin_id, "phux.builtin");
    assert_eq!(resolved.argv, ["sh", "custom-acp"]);
    std::fs::write(&path, "invalid [").expect("break profile");
    assert!(matches!(resolve(), Err(LaunchError::Template { .. })));
}
