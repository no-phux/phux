#![allow(clippy::expect_used, reason = "tests")]

//! Launch executor resolution (phux-ark7, ADR-0042): a named integration
//! template shipped by an enabled plugin resolves to a spawnable argv with
//! `${PHUX_PLUGIN_ROOT}` expanded and the working directory chosen per the
//! template.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use phux_config::integration::LaunchWorkingDirectory;
use phux_plugin::{KindLaunchError, LaunchError, LaunchMode, ResolvedLaunch, resolve_launch};
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
required_executables = ["sh"]
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

    // Structurally launchable, but hidden because its declared prerequisite
    // cannot exist on a real PATH.
    std::fs::write(
        integrations.join("unavailable.toml"),
        r#"
id = "unavailable"
[launch]
command = ["definitely-not-a-phux-test-executable"]
required_executables = ["definitely-not-a-phux-test-executable"]
"#,
    )
    .expect("write unavailable template");

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

include!("launch_runtime/opencode.rs");
include!("launch_runtime/fx.rs");

/// An enabled [`write_plugin`] tree and its workspace.
struct Fixture {
    tmp: TempDir,
    config: PathBuf,
    root: PathBuf,
    ws: PathBuf,
}

impl Fixture {
    fn new(enabled: bool) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let (config, root) = write_plugin(&tmp, enabled);
        let ws = workspace(&tmp);
        Self {
            tmp,
            config,
            root,
            ws,
        }
    }

    fn resolve(&self, name: &str) -> Result<ResolvedLaunch, LaunchError> {
        resolve_launch(&self.config, name, &[], &self.ws, LaunchMode::Terminal)
    }

    fn for_kind(
        &self,
        explicit: Option<&str>,
        kind: &str,
    ) -> Result<ResolvedLaunch, KindLaunchError> {
        phux_plugin::resolve_launch_for_kind(
            &self.config,
            explicit,
            kind,
            &[],
            &self.ws,
            LaunchMode::Terminal,
        )
    }

    fn listed(&self) -> Vec<String> {
        let mut ids: Vec<String> = phux_plugin::list_launchable(&self.config, LaunchMode::Terminal)
            .expect("list")
            .into_iter()
            .map(|item| item.integration_id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Enable a second plugin `id` shipping `integrations`.
    fn add_plugin(&self, id: &str, integrations: &[(&str, &str)]) {
        let dir = self.tmp.path().join(id);
        std::fs::create_dir_all(dir.join("integrations")).expect("create plugin");
        let manifest = dir.join("phux-plugin.toml");
        std::fs::write(
            &manifest,
            format!("id = \"{id}\"\nname = \"Second\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\n"),
        )
        .expect("write manifest");
        for (file, body) in integrations {
            std::fs::write(dir.join("integrations").join(file), body).expect("write integration");
        }
        let mut config = std::fs::read_to_string(&self.config).expect("read config");
        writeln!(
            config,
            "\n[[plugins]]\nmanifest = {:?}\nenabled = true",
            manifest.display().to_string()
        )
        .expect("extend config text");
        std::fs::write(&self.config, config).expect("extend config");
    }
}

#[test]
fn resolves_named_integration_expanding_plugin_root_and_appending_extra_args() {
    let fx = Fixture::new(true);
    let resolved = resolve_launch(
        &fx.config,
        "codex",
        &["--resume".to_owned()],
        &fx.ws,
        LaunchMode::Terminal,
    )
    .expect("codex resolves");
    assert_eq!(resolved.plugin_id, "example.launch");
    assert_eq!(resolved.integration_id, "codex");
    assert_eq!(resolved.display_name.as_deref(), Some("Codex"));
    // `[agent_identity].kind` is the detection slug, not the category `kind`.
    let identity = resolved
        .agent_identity
        .as_ref()
        .expect("agent identity carried");
    assert_eq!(identity.name.as_deref(), Some("codex"));
    assert_eq!(identity.kind.as_deref(), Some("codex"));
    let wrap = format!("{}/scripts/wrap.sh", fx.root.display());
    assert_eq!(
        resolved.argv,
        ["sh", &wrap, "--name", "codex", "--", "codex", "--resume"]
    );
    assert_eq!(
        resolved.working_directory,
        LaunchWorkingDirectory::Workspace
    );
    assert_eq!(resolved.cwd, fx.ws);

    let rooted = fx.resolve("rooted").expect("rooted resolves");
    assert_eq!(rooted.working_directory, LaunchWorkingDirectory::PluginRoot);
    assert_eq!(rooted.cwd, fx.root);
}

#[test]
fn unresolvable_names_are_typed_refusals() {
    let fx = Fixture::new(true);
    std::fs::write(
        fx.root.join("integrations/busted.toml"),
        "this = = not valid toml",
    )
    .expect("write busted template");
    match fx.resolve("nope").expect_err("unknown id") {
        LaunchError::NotFound { name, available } => {
            assert_eq!(name, "nope");
            assert!(available.contains(&"codex".to_owned()));
            assert!(available.contains(&"rooted".to_owned()));
            assert!(!available.contains(&"detect-only".to_owned()));
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    let err = fx.resolve("detect-only").expect_err("no launch");
    assert!(
        matches!(err, LaunchError::NoLaunchCommand { ref name } if name == "detect-only"),
        "{err:?}"
    );
    let err = fx.resolve("busted").expect_err("busted target");
    assert!(matches!(err, LaunchError::Template { .. }), "got {err:?}");
    // A missing prerequisite hides the template from the list but explains
    // itself when named.
    assert_eq!(fx.listed(), ["codex", "rooted"]);
    let err = fx.resolve("unavailable").expect_err("missing prerequisite");
    assert!(
        matches!(
            err,
            LaunchError::MissingExecutables { ref name, ref missing }
                if name == "unavailable" && missing == &["definitely-not-a-phux-test-executable"]
        ),
        "got {err:?}"
    );
}

#[test]
fn disabled_plugin_ships_no_launchable_integrations() {
    let fx = Fixture::new(false);
    match fx.resolve("codex").expect_err("disabled plugin") {
        LaunchError::NotFound { available, .. } => assert!(available.is_empty()),
        other => panic!("expected NotFound with empty available, got {other:?}"),
    }
    assert!(fx.listed().is_empty());
}

#[test]
fn list_launchable_carries_each_agent_identity() {
    let fx = Fixture::new(true);
    let items = phux_plugin::list_launchable(&fx.config, LaunchMode::Terminal).expect("list");
    let identity = |id: &str| {
        items
            .iter()
            .find(|item| item.integration_id == id)
            .expect("listed")
            .agent_identity
            .as_ref()
            .and_then(|identity| identity.kind.clone())
    };
    assert_eq!(identity("codex").as_deref(), Some("codex"));
    assert_eq!(identity("rooted"), None);
}

#[test]
fn duplicate_enabled_plugin_and_integration_ids_fail_closed() {
    let fx = Fixture::new(true);
    fx.add_plugin(
        "example.second",
        &[(
            "codex.toml",
            "id = \"codex\"\n[launch]\ncommand = [\"codex\"]\n",
        )],
    );
    let err = fx
        .resolve("codex")
        .expect_err("duplicate integration owner");
    assert!(
        matches!(err, LaunchError::DuplicateIntegrationId { ref id, .. } if id == "codex"),
        "{err:?}"
    );

    let fx = Fixture::new(true);
    fx.add_plugin("example.launch", &[]);
    let err = fx.resolve("codex").expect_err("duplicate plugin owner");
    assert!(
        matches!(err, LaunchError::DuplicatePluginId { ref id, .. } if id == "example.launch"),
        "{err:?}"
    );
}

/// `phux agent start --kind K`: the unique `[agent_identity]` claimant, with
/// trim/case tolerance; `--integration` overrides without consulting claims.
#[test]
fn a_kind_resolves_the_integration_that_claims_it() {
    let fx = Fixture::new(true);
    for kind in ["codex", " CODEX "] {
        assert_eq!(
            fx.for_kind(None, kind).expect("claimed").integration_id,
            "codex"
        );
    }
    assert_eq!(
        fx.for_kind(Some("rooted"), "codex")
            .expect("explicit")
            .integration_id,
        "rooted"
    );
}

/// An unclaimed kind falls back to the integration spelled like the kind, as
/// given: the CLI canonicalizes `--kind CLAUDE` before calling.
#[test]
fn an_unclaimed_kind_falls_back_to_the_kind_as_given() {
    let fx = Fixture::new(true);
    assert_eq!(
        fx.for_kind(None, "rooted")
            .expect("fallback")
            .integration_id,
        "rooted"
    );
    match fx
        .for_kind(None, " CLAUDE ")
        .expect_err("no template is named ' CLAUDE '")
    {
        KindLaunchError::Resolve { integration_id, .. } => assert_eq!(integration_id, " CLAUDE "),
        KindLaunchError::Ambiguous { claimants, .. } => panic!("got claimants {claimants:?}"),
    }
}

/// Two claimants of one kind are refused by name; an explicit id still wins.
#[test]
fn two_claimants_for_one_kind_are_refused_naming_both() {
    let fx = Fixture::new(true);
    std::fs::write(
        fx.root.join("integrations/codex-fork.toml"),
        "id = \"codex-fork\"\n[agent_identity]\nkind = \"codex\"\n[launch]\ncommand = [\"sh\", \"-c\", \"true\"]\n",
    )
    .expect("write a second claimant");
    match fx.for_kind(None, "codex").expect_err("ambiguous") {
        KindLaunchError::Ambiguous { kind, claimants } => {
            assert_eq!(kind, "codex");
            assert_eq!(claimants, ["codex", "codex-fork"]);
        }
        KindLaunchError::Resolve { integration_id, .. } => panic!("resolved {integration_id:?}"),
    }
    assert_eq!(
        fx.for_kind(Some("codex"), "codex")
            .expect("explicit")
            .integration_id,
        "codex"
    );
}
