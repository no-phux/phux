#![allow(clippy::expect_used, reason = "tests")]

//! Launch executor resolution (phux-ark7, ADR-0042): a named integration
//! template shipped by an enabled plugin resolves to a spawnable argv with
//! `${PHUX_PLUGIN_ROOT}` expanded and the working directory chosen per the
//! template.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use phux_config::integration::LaunchWorkingDirectory;
use phux_plugin::{LaunchError, resolve_launch};
use tempfile::TempDir;

/// Write a plugin (manifest + `integrations/` templates) and a `config.toml`
/// referencing it, returning `(config_path, plugin_root)`.
fn write_plugin(tmp: &TempDir, enabled: bool) -> (PathBuf, PathBuf) {
    let plugin_dir = tmp.path().join("plugin");
    let integrations = plugin_dir.join("integrations");
    std::fs::create_dir_all(&integrations).expect("create integrations dir");
    std::fs::write(
        plugin_dir.join("phux-plugin.toml"),
        r#"
id = "example.launch"
name = "Launch"
version = "0.1.0"
min_phux_version = "0.0.2"
"#,
    )
    .expect("write manifest");

    // Launchable, workspace-rooted.
    std::fs::write(
        integrations.join("codex.toml"),
        r#"
id = "codex"
display_name = "Codex"
kind = "terminal-agent"
first_party = true

[agent_identity]
name = "codex"
kind = "codex"

[launch]
command = ["sh", "${PHUX_PLUGIN_ROOT}/scripts/wrap.sh", "--name", "codex", "--", "codex"]
working_directory = "workspace"
"#,
    )
    .expect("write codex template");

    // Launchable, plugin-root-rooted.
    std::fs::write(
        integrations.join("rooted.toml"),
        r#"
id = "rooted"
[launch]
command = ["sh", "-c", "true"]
working_directory = "plugin-root"
"#,
    )
    .expect("write rooted template");

    // Parseable but not launchable (no [launch]).
    std::fs::write(
        integrations.join("detect-only.toml"),
        r#"
id = "detect-only"
display_name = "Detect Only"
"#,
    )
    .expect("write detect-only template");

    let config_dir = tmp.path().join("xdg").join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let config_path = config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[[plugins]]
manifest = "{}"
enabled = {enabled}
"#,
            plugin_dir.join("phux-plugin.toml").display()
        ),
    )
    .expect("write config");

    let plugin_root = plugin_dir.canonicalize().expect("canonical plugin root");
    (config_path, plugin_root)
}

fn workspace(tmp: &TempDir) -> PathBuf {
    let dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&dir).expect("create workspace");
    dir
}

#[test]
fn resolves_named_integration_expanding_plugin_root_and_appending_extra_args() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, plugin_root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    let resolved =
        resolve_launch(&config, "codex", &["--resume".to_owned()], &ws).expect("codex resolves");

    assert_eq!(resolved.plugin_id, "example.launch");
    assert_eq!(resolved.integration_id, "codex");
    assert_eq!(resolved.display_name.as_deref(), Some("Codex"));
    // The `[agent_identity]` block rides the resolution: its `kind` is the
    // detection slug, distinct from the category `kind` above.
    let identity = resolved
        .agent_identity
        .as_ref()
        .expect("agent identity carried");
    assert_eq!(identity.name.as_deref(), Some("codex"));
    assert_eq!(identity.kind.as_deref(), Some("codex"));
    assert_eq!(
        resolved.argv,
        vec![
            "sh".to_owned(),
            format!("{}/scripts/wrap.sh", plugin_root.display()),
            "--name".to_owned(),
            "codex".to_owned(),
            "--".to_owned(),
            "codex".to_owned(),
            "--resume".to_owned(),
        ]
    );
    // No argv element still carries the unexpanded placeholder.
    assert!(
        !resolved
            .argv
            .iter()
            .any(|arg| arg.contains("${PHUX_PLUGIN_ROOT}")),
        "placeholder must be expanded: {:?}",
        resolved.argv
    );
    // workspace working directory -> the caller's cwd.
    assert_eq!(
        resolved.working_directory,
        LaunchWorkingDirectory::Workspace
    );
    assert_eq!(resolved.cwd, ws);
}

#[test]
fn plugin_root_working_directory_runs_in_the_plugin_tree() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, plugin_root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    let resolved = resolve_launch(&config, "rooted", &[], &ws).expect("rooted resolves");

    assert_eq!(
        resolved.working_directory,
        LaunchWorkingDirectory::PluginRoot
    );
    assert_eq!(resolved.cwd, plugin_root);
}

#[test]
fn unknown_integration_reports_available_ids() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    let err = resolve_launch(&config, "nope", &[], &ws).expect_err("unknown id");
    match err {
        LaunchError::NotFound { name, available } => {
            assert_eq!(name, "nope");
            // Only the two launchable templates surface; detect-only does not.
            assert!(available.contains(&"codex".to_owned()));
            assert!(available.contains(&"rooted".to_owned()));
            assert!(!available.contains(&"detect-only".to_owned()));
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn integration_without_launch_command_is_reported() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    let err = resolve_launch(&config, "detect-only", &[], &ws).expect_err("no launch");
    assert!(
        matches!(err, LaunchError::NoLaunchCommand { name } if name == "detect-only"),
        "expected NoLaunchCommand",
    );
}

#[test]
fn disabled_plugin_ships_no_launchable_integrations() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _root) = write_plugin(&tmp, false);
    let ws = workspace(&tmp);

    let err = resolve_launch(&config, "codex", &[], &ws).expect_err("disabled plugin");
    match err {
        LaunchError::NotFound { available, .. } => assert!(available.is_empty()),
        other => panic!("expected NotFound with empty available, got {other:?}"),
    }
    assert!(
        phux_plugin::list_launchable(&config)
            .expect("list")
            .is_empty()
    );
}

#[test]
fn list_launchable_enumerates_only_templates_with_a_launch_command() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _root) = write_plugin(&tmp, true);

    let items = phux_plugin::list_launchable(&config).expect("list");
    let mut ids: Vec<&str> = items
        .iter()
        .map(|item| item.integration_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["codex", "rooted"]);

    // The listing surfaces each template's `[agent_identity]` (or its
    // absence), so a caller can map a detection kind to the integration
    // that launches it.
    let codex = items
        .iter()
        .find(|item| item.integration_id == "codex")
        .expect("codex listed");
    assert_eq!(
        codex
            .agent_identity
            .as_ref()
            .and_then(|identity| identity.kind.as_deref()),
        Some("codex")
    );
    let rooted = items
        .iter()
        .find(|item| item.integration_id == "rooted")
        .expect("rooted listed");
    assert_eq!(rooted.agent_identity, None);
}

/// A broken sibling template must not block resolving a healthy one, but a
/// broken template whose filename is the requested id surfaces its error.
#[test]
fn broken_sibling_template_is_skipped_but_target_errors_surface() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, plugin_root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    std::fs::write(
        Path::new(&plugin_root)
            .join("integrations")
            .join("busted.toml"),
        "this = = not valid toml",
    )
    .expect("write busted template");

    // codex still resolves despite the broken sibling.
    assert!(resolve_launch(&config, "codex", &[], &ws).is_ok());

    // Asking for the broken template surfaces its parse error.
    let err = resolve_launch(&config, "busted", &[], &ws).expect_err("busted target");
    assert!(matches!(err, LaunchError::Template { .. }), "got {err:?}");
}

#[test]
fn duplicate_enabled_plugin_ids_are_rejected_before_resolution() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _plugin_root) = write_plugin(&tmp, true);
    let imposter = tmp.path().join("imposter");
    std::fs::create_dir_all(&imposter).expect("create imposter");
    let imposter_manifest = imposter.join("phux-plugin.toml");
    std::fs::write(
        &imposter_manifest,
        r#"
id = "example.launch"
name = "Imposter"
version = "0.1.0"
min_phux_version = "0.0.2"
"#,
    )
    .expect("write imposter manifest");
    let mut config_text = std::fs::read_to_string(&config).expect("read config");
    writeln!(
        config_text,
        "\n[[plugins]]\nmanifest = {:?}\nenabled = true",
        imposter_manifest.display().to_string()
    )
    .expect("extend config text");
    std::fs::write(&config, config_text).expect("extend config");

    let err = resolve_launch(&config, "codex", &[], &workspace(&tmp))
        .expect_err("duplicate owner must fail closed");
    assert!(
        matches!(err, LaunchError::DuplicatePluginId { ref id, .. } if id == "example.launch"),
        "got {err:?}"
    );
}

#[test]
fn duplicate_enabled_integration_ids_are_rejected_before_resolution() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _plugin_root) = write_plugin(&tmp, true);
    let second = tmp.path().join("second");
    let integrations = second.join("integrations");
    std::fs::create_dir_all(&integrations).expect("create second integrations");
    let manifest = second.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        r#"
id = "example.second"
name = "Second"
version = "0.1.0"
min_phux_version = "0.0.2"
"#,
    )
    .expect("write second manifest");
    std::fs::write(
        integrations.join("codex.toml"),
        r#"
id = "codex"
[launch]
command = ["codex"]
"#,
    )
    .expect("write duplicate integration");
    let mut config_text = std::fs::read_to_string(&config).expect("read config");
    writeln!(
        config_text,
        "\n[[plugins]]\nmanifest = {:?}\nenabled = true",
        manifest.display().to_string()
    )
    .expect("extend config text");
    std::fs::write(&config, config_text).expect("extend config");

    let err = resolve_launch(&config, "codex", &[], &workspace(&tmp))
        .expect_err("duplicate integration owner must fail closed");
    assert!(
        matches!(err, LaunchError::DuplicateIntegrationId { ref id, .. } if id == "codex"),
        "got {err:?}"
    );
}

/// The default `phux agent start --kind K` resolution, in one walk of the
/// plugin tree: the unique `[agent_identity]` claimant of the kind, with the
/// shared trim/case tolerance.
#[test]
fn a_kind_resolves_the_integration_that_claims_it() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    let resolved = phux_plugin::resolve_launch_for_kind(&config, None, "codex", &[], &ws)
        .expect("the codex template claims kind codex");
    assert_eq!(resolved.integration_id, "codex");

    let insensitive = phux_plugin::resolve_launch_for_kind(&config, None, " CODEX ", &[], &ws)
        .expect("claims are matched with the shared tolerance");
    assert_eq!(insensitive.integration_id, "codex");
}

/// `--integration` stays the explicit override: taken verbatim, with the
/// `[agent_identity]` map never consulted.
#[test]
fn an_explicit_integration_overrides_the_kind_claim() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    let resolved = phux_plugin::resolve_launch_for_kind(&config, Some("rooted"), "codex", &[], &ws)
        .expect("the explicit id wins");
    assert_eq!(resolved.integration_id, "rooted");
}

/// A kind nothing claims falls back to the integration spelled like the kind
/// — the pre-`agent_identity` default, and the reason the caller must hand
/// this the *canonical* kind rather than the string a user typed: the
/// fallback id is that string, verbatim.
#[test]
fn an_unclaimed_kind_falls_back_to_the_kind_as_given() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, _root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);

    // `rooted.toml` declares no `[agent_identity]`, so nothing claims the
    // kind and the id spelled like it still resolves.
    let resolved = phux_plugin::resolve_launch_for_kind(&config, None, "rooted", &[], &ws)
        .expect("the id-spelled-like-the-kind default still resolves");
    assert_eq!(resolved.integration_id, "rooted");

    // Uncanonicalized input is looked up exactly as handed over. Canonicalizing
    // a detection kind needs the manifests, which live above this crate, so the
    // CLI resolves `--kind CLAUDE` to `claude` before calling
    // (`an_unclaimed_kind_falls_back_to_the_canonical_kind_not_the_typed_one`).
    let err = phux_plugin::resolve_launch_for_kind(&config, None, " CLAUDE ", &[], &ws)
        .expect_err("no template is named ' CLAUDE '");
    match err {
        phux_plugin::KindLaunchError::Resolve { integration_id, .. } => {
            assert_eq!(integration_id, " CLAUDE ");
        }
        phux_plugin::KindLaunchError::Ambiguous { claimants, .. } => {
            panic!("expected a resolution failure, got claimants {claimants:?}")
        }
    }
}

/// Two enabled templates claiming one kind is refused by name rather than
/// picked between.
#[test]
fn two_claimants_for_one_kind_are_refused_naming_both() {
    let tmp = TempDir::new().expect("tempdir");
    let (config, plugin_root) = write_plugin(&tmp, true);
    let ws = workspace(&tmp);
    std::fs::write(
        plugin_root.join("integrations").join("codex-fork.toml"),
        r#"
id = "codex-fork"

[agent_identity]
kind = "codex"

[launch]
command = ["sh", "-c", "true"]
"#,
    )
    .expect("write a second claimant");

    let err = phux_plugin::resolve_launch_for_kind(&config, None, "codex", &[], &ws)
        .expect_err("two claimants must not be picked between");
    match err {
        phux_plugin::KindLaunchError::Ambiguous { kind, claimants } => {
            assert_eq!(kind, "codex");
            assert_eq!(claimants, vec!["codex".to_owned(), "codex-fork".to_owned()]);
        }
        phux_plugin::KindLaunchError::Resolve { integration_id, .. } => {
            panic!("expected an ambiguity, got a resolution of {integration_id:?}")
        }
    }

    // The explicit override still cuts through it.
    let resolved = phux_plugin::resolve_launch_for_kind(&config, Some("codex"), "codex", &[], &ws)
        .expect("an explicit id cannot be vetoed by an ambiguous claim set");
    assert_eq!(resolved.integration_id, "codex");
}
