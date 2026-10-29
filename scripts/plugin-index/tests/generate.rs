//! Offline catalog: fixture API responses, manifests, and the golden index.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use phux_plugin_index::{
    Index, NOTICE, Options, SCHEMA_VERSION, Source, build_index, render_json, render_markdown,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn fixture_index() -> Index {
    let base = root_pkg();
    build_index(&Options {
        source: Source::Offline {
            fixtures: base.join("fixtures"),
        },
        examples_dir: base.join("fixtures/examples/plugins"),
        examples_repo: "fixture/demos".to_owned(),
        repo_root: base.join("fixtures"),
        generated_at: "2026-01-15T00:00:00Z".to_owned(),
    })
    .expect("fixture index")
}

fn root_pkg() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn fixture_catalog_skips_archived_forks_private_and_invalid_manifests() {
    let index = fixture_index();
    let listed: Vec<_> = index
        .plugins
        .iter()
        .map(|plugin| {
            (
                plugin.repo.as_str(),
                plugin.subdir.as_str(),
                plugin.name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("acme/pack", "plugins/theme", "Theme"),
            ("acme/widgets", "", "Widgets"),
            ("fixture/demos", "examples/plugins/demo", "Demo"),
        ]
    );

    let theme = index
        .plugins
        .iter()
        .find(|plugin| plugin.name == "Theme")
        .expect("theme");
    assert_eq!(theme.kinds, ["themes"]);
    assert_eq!(theme.stars, Some(1));
    assert_eq!(theme.license, None);
    assert_eq!(
        theme.install,
        "phux plugin install https://github.com/acme/pack"
    );
    assert_eq!(theme.min_phux_version, "0.1.0");

    let widgets = index
        .plugins
        .iter()
        .find(|plugin| plugin.name == "Widgets")
        .expect("widgets");
    assert_eq!(widgets.kinds, ["actions", "panes"]);
    assert_eq!(widgets.stars, Some(12));
    assert_eq!(widgets.license.as_deref(), Some("MIT"));
    assert_eq!(widgets.subdir, "");

    let demo = index
        .plugins
        .iter()
        .find(|plugin| plugin.name == "Demo")
        .expect("demo");
    assert_eq!(demo.kinds, ["agents"]);
    assert_eq!(demo.stars, Some(4));
    assert_eq!(demo.license.as_deref(), Some("Apache-2.0"));

    let skipped: Vec<_> = index
        .skipped
        .iter()
        .map(|skip| {
            (
                skip.repo.as_str(),
                skip.subdir.as_str(),
                skip.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        skipped,
        [
            ("acme/baddirs", "", "invalid plugin directory: ../escape"),
            (
                "acme/broken",
                "",
                "invalid manifest: acme/broken/phux-plugin.toml: event `nope` listens for \
                 `on-startup`, which is not a hook event; it will never fire"
            ),
            ("acme/empty", "", "no valid manifest"),
            ("acme/forked", "", "fork"),
            ("acme/old", "", "archived"),
            ("acme/pack", "plugins/missing", "no valid manifest"),
            ("acme/secret", "", "private"),
        ]
    );
    assert!(index.plugins.iter().all(|plugin| plugin.name != "Old"));
    assert!(index.plugins.iter().all(|plugin| plugin.name != "Forked"));
    assert!(index.plugins.iter().all(|plugin| plugin.name != "Secret"));
    assert!(index.plugins.iter().all(|plugin| plugin.name != "Broken"));
    assert_eq!(index.notice, NOTICE);
    assert_eq!(index.schema_version, SCHEMA_VERSION);
}

#[test]
fn golden_json_and_markdown_match() {
    let index = fixture_index();
    let json = render_json(&index).expect("json");
    let markdown = render_markdown(&index);
    let golden = root_pkg().join("tests/golden");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(&golden).expect("golden dir");
        fs::write(golden.join("plugin-index.json"), &json).expect("write json");
        fs::write(golden.join("plugin-index.md"), &markdown).expect("write md");
    }
    let want_json = fs::read_to_string(golden.join("plugin-index.json")).expect("golden json");
    let want_md = fs::read_to_string(golden.join("plugin-index.md")).expect("golden md");
    assert_eq!(json, want_json, "JSON drifted; rerun with UPDATE_GOLDEN=1");
    assert_eq!(
        markdown, want_md,
        "Markdown drifted; rerun with UPDATE_GOLDEN=1"
    );
    assert!(markdown.contains(NOTICE));
    assert!(!markdown.to_ascii_lowercase().contains("reviewed"));
    assert!(!markdown.to_ascii_lowercase().contains("safe to"));
}

#[test]
fn schema_describes_the_golden_document() {
    let schema_path = root().join("docs/spec/plugin-index.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&schema_path).expect("schema")).expect("json");
    assert_eq!(schema["properties"]["notice"]["const"], NOTICE);
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        SCHEMA_VERSION
    );
    let document: serde_json::Value =
        serde_json::from_str(&render_json(&fixture_index()).expect("json")).expect("doc");
    assert_eq!(sorted(keys(&document)), sorted(required(&schema)));
    assert_eq!(
        sorted(keys(&document["plugins"][0])),
        sorted(required_at(&schema, "/$defs/plugin"))
    );
    assert_eq!(
        sorted(keys(&document["skipped"][0])),
        sorted(required_at(&schema, "/$defs/skipped"))
    );
}

fn sorted(mut keys: Vec<String>) -> Vec<String> {
    keys.sort();
    keys
}

#[test]
fn shipped_examples_are_indexed_offline() {
    let repo = root();
    let index = build_index(&Options {
        source: Source::Offline {
            fixtures: root_pkg().join("fixtures/examples-only"),
        },
        examples_dir: repo.join("examples/plugins"),
        examples_repo: "no-phux/phux".to_owned(),
        repo_root: repo,
        generated_at: "2026-01-15T00:00:00Z".to_owned(),
    })
    .expect("examples");
    for dir in [
        "agent-tools",
        "continuum",
        "provider-showcase",
        "theme-pack",
        "usage",
    ] {
        let subdir = format!("examples/plugins/{dir}");
        let plugin = index
            .plugins
            .iter()
            .find(|plugin| plugin.subdir == subdir)
            .unwrap_or_else(|| panic!("missing {subdir}"));
        assert_eq!(plugin.repo, "no-phux/phux");
        assert_eq!(
            plugin.install,
            "phux plugin install https://github.com/no-phux/phux"
        );
        assert!(
            plugin.stars.is_none(),
            "offline examples have no stargazers"
        );
        assert!(
            index.skipped.iter().all(|skip| skip.subdir != subdir),
            "{subdir} was skipped: {:?}",
            index.skipped
        );
    }
    let theme = index
        .plugins
        .iter()
        .find(|plugin| plugin.subdir == "examples/plugins/theme-pack")
        .expect("theme-pack");
    assert_eq!(theme.kinds, ["themes"]);
    let usage = index
        .plugins
        .iter()
        .find(|plugin| plugin.subdir == "examples/plugins/usage")
        .expect("usage");
    assert_eq!(usage.kinds, ["actions", "events"]);
}

#[test]
fn an_unparseable_manifest_is_skipped_without_a_temp_path() {
    let dir = scratch("unparseable");
    write_search(
        &dir,
        r#"{"total_count":1,"incomplete_results":false,"items":[{"full_name":"acme/bad","archived":false,"fork":false,"default_branch":"main","stargazers_count":0,"pushed_at":"2026-01-01T00:00:00Z","license":null}]}"#,
    );
    let repo = dir.join("repos/acme/bad");
    fs::create_dir_all(&repo).expect("repo");
    fs::write(repo.join("phux-plugin.toml"), "id = \"acme.bad\"\n").expect("manifest");
    fs::create_dir_all(dir.join("examples")).expect("examples");
    let index = build_at(&dir, &dir.join("examples"), "fixture/none");
    assert!(index.plugins.is_empty());
    let reason = &index.skipped[0].reason;
    assert!(reason.contains("invalid manifest"), "{reason}");
    assert!(reason.contains("missing field"), "{reason}");
    assert!(!reason.contains(dir.to_str().unwrap_or("")), "{reason}");
}

#[test]
fn a_broken_directory_list_is_skipped() {
    let dir = scratch("baddirs");
    write_search(
        &dir,
        r#"{"total_count":1,"incomplete_results":false,"items":[{"full_name":"acme/bad","archived":false,"fork":false,"default_branch":"main","stargazers_count":0,"pushed_at":"2026-01-01T00:00:00Z","license":null}]}"#,
    );
    let repo = dir.join("repos/acme/bad");
    fs::create_dir_all(&repo).expect("repo");
    fs::write(repo.join("phux-plugin-dirs.toml"), "subdirs =\n").expect("dirs");
    fs::create_dir_all(dir.join("examples")).expect("examples");
    let index = build_at(&dir, &dir.join("examples"), "fixture/none");
    assert!(index.plugins.is_empty());
    assert!(
        index.skipped[0]
            .reason
            .starts_with("invalid plugin directory list:"),
        "{}",
        index.skipped[0].reason
    );
}

#[test]
fn the_offline_binary_does_not_need_a_token() {
    let pkg = root_pkg();
    let out = scratch("cli");
    let status = Command::new(env_bin())
        .arg("--offline")
        .arg(pkg.join("fixtures/examples-only"))
        .arg("--examples")
        .arg(root().join("examples/plugins"))
        .arg("--repo-root")
        .arg(root())
        .arg("--generated-at")
        .arg("2026-01-15T00:00:00Z")
        .arg("--out")
        .arg(&out)
        .env_remove("GITHUB_TOKEN")
        .status()
        .expect("run");
    assert!(status.success(), "{status}");
    let text = fs::read_to_string(out.join("plugin-index.json")).expect("json");
    assert!(text.contains("\"subdir\": \"examples/plugins/usage\""));
    assert!(text.contains(NOTICE));
}

fn env_bin() -> PathBuf {
    // `cargo test` runs this integration test from target/debug/deps/<test>-<hash>.
    // The generator binary is the sibling of `deps`.
    let current = std::env::current_exe().expect("test executable");
    let deps = current.parent().expect("deps directory");
    let debug = deps.parent().expect("target profile directory");
    debug.join("phux-plugin-index")
}

fn build_at(fixtures: &Path, examples: &Path, examples_repo: &str) -> Index {
    build_index(&Options {
        source: Source::Offline {
            fixtures: fixtures.to_path_buf(),
        },
        examples_dir: examples.to_path_buf(),
        examples_repo: examples_repo.to_owned(),
        repo_root: fixtures.to_path_buf(),
        generated_at: "2026-01-15T00:00:00Z".to_owned(),
    })
    .expect("index")
}

fn write_search(dir: &Path, body: &str) {
    fs::write(dir.join("search.json"), body).expect("search");
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "phux-plugin-index-test-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("scratch");
    path
}

fn keys(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect::<Vec<_>>()
}

fn required(schema: &serde_json::Value) -> Vec<String> {
    schema["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|item| item.as_str().expect("string").to_owned())
        .collect()
}

fn required_at(schema: &serde_json::Value, pointer: &str) -> Vec<String> {
    required(
        schema
            .pointer(pointer)
            .unwrap_or_else(|| panic!("missing {pointer}")),
    )
}
