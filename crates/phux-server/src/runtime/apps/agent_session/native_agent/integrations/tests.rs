#![allow(clippy::unwrap_used, reason = "isolated profile fixtures")]
use super::*;

fn profile(root: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(root.join("integrations")).unwrap();
    std::fs::write(
        root.join("phux-plugin.toml"),
        r#"
id = "test.native"
name = "Native test"
version = "0.1.0"
min_phux_version = "0.0.2"
"#,
    )
    .unwrap();
    let config = root.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[[plugins]]\nmanifest = {:?}\n",
            root.join("phux-plugin.toml").to_str().unwrap()
        ),
    )
    .unwrap();
    config
}

#[test]
fn catalog_reports_explicit_profile_failures_without_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let config = profile(dir.path());
    std::fs::write(dir.path().join("integrations/opencode.toml"), "invalid [").unwrap();
    let catalog = catalog_at(&config).unwrap();
    assert_eq!(catalog.integrations.len(), 2);
    let item = catalog
        .integrations
        .iter()
        .find(|item| item.integration == "opencode")
        .unwrap();
    assert!(!item.native_available);
    assert!(
        item.native_detail
            .as_ref()
            .unwrap()
            .contains("opencode.toml")
    );
}

#[test]
fn catalog_exposes_fx_even_when_its_override_is_broken() {
    let dir = tempfile::tempdir().unwrap();
    let config = profile(dir.path());
    let catalog = catalog_at(&config).unwrap();
    let fx = catalog
        .integrations
        .iter()
        .find(|item| item.integration == "fx")
        .unwrap();
    assert_eq!(fx.display_name.as_deref(), Some("fx"));
    assert_eq!(
        fx.native_available,
        phux_plugin::executable_path("fx").is_some()
    );
    assert!(fx.auth_command.is_none());

    std::fs::write(dir.path().join("integrations/fx.toml"), "invalid [").unwrap();
    let catalog = catalog_at(&config).unwrap();
    let fx = catalog
        .integrations
        .iter()
        .find(|item| item.integration == "fx")
        .unwrap();
    assert!(!fx.native_available);
    assert!(fx.native_detail.as_ref().unwrap().contains("fx.toml"));
}

#[test]
fn resolution_uses_host_configuration_and_validates_both_directories() {
    let dir = tempfile::tempdir().unwrap();
    let config = profile(dir.path());
    std::fs::write(
        dir.path().join("integrations/opencode.toml"),
        r#"
id = "opencode"
[acp]
provider = "opencode"
command = ["sh", "configured-acp"]
required_executables = ["sh"]
working_directory = "workspace"
"#,
    )
    .unwrap();
    let cwd = dir.path().to_str().unwrap();
    let spec = resolve(&config, "opencode", cwd).unwrap();
    assert_eq!(spec.command, ["sh", "configured-acp"]);
    assert_eq!(spec.cwd.as_deref(), Some(cwd));
    assert_eq!(spec.backend, "acp");
    assert!(spec.env.is_empty());
    for invalid in [
        "",
        "relative",
        "/dev/null",
        "/definitely-missing-phux-native-directory",
    ] {
        assert!(resolve(&config, "opencode", invalid).is_err());
    }
    assert!(valid_directory(&dir.path().join("not-a-directory")).is_err());
    assert!(resolve(&config, "unknown", cwd).is_err());
}

#[test]
fn catalog_refuses_more_than_sixty_four_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let config = profile(dir.path());
    for index in 0..64 {
        let id = format!("profile-{index}");
        std::fs::write(
            dir.path().join("integrations").join(format!("{id}.toml")),
            format!(
                r#"
id = "{id}"
[acp]
provider = "opencode"
command = ["sh"]
"#
            ),
        )
        .unwrap();
    }
    assert!(
        matches!(catalog_at(&config), Err(CommandResult::Error { message, .. }) if message.contains("64 profiles"))
    );
}

#[test]
fn catalog_auth_tracks_the_absolute_opencode_override_but_never_wrappers_or_custom_args() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let config = profile(dir.path());
    let executable = dir.path().join("opencode");
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("integrations/opencode.toml");
    let write = |executable: &Path, args: &str| {
        std::fs::write(
            &path,
            format!(
                "id = 'opencode'\n[acp]\nprovider = 'opencode'\ncommand = [{:?}, {args}]\n",
                executable.to_str().unwrap()
            ),
        )
        .unwrap();
    };
    let auth = || {
        catalog_at(&config)
            .unwrap()
            .integrations
            .into_iter()
            .find(|item| item.integration == "opencode")
            .unwrap()
            .auth_command
    };
    write(&executable, "'acp'");
    assert_eq!(
        auth(),
        Some(vec![
            executable.to_str().unwrap().into(),
            "auth".into(),
            "login".into()
        ])
    );
    write(&executable, "'acp', '--custom'");
    assert_eq!(auth(), None);
    let wrapper = dir.path().join("wrapper");
    std::fs::copy(&executable, &wrapper).unwrap();
    write(&wrapper, "'acp'");
    assert_eq!(auth(), None);
    let missing = dir.path().join("missing/opencode");
    write(&missing, "'acp'");
    assert_eq!(auth(), None);
}
