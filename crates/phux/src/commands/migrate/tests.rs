use std::path::{Path, PathBuf};

use serde_json::json;

use super::sources::{Verdict, discover};
use super::{agent_argv, document, prompt};
use crate::commands::{MigrateAgent, MigrateSource};

fn fixture_home() -> PathBuf {
    // Read at run time: a compiled-in checkout path defeats the build cache.
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
        .join("src/commands/migrate/testdata/home")
}

fn render(home: &Path, sources: &[super::sources::Source]) -> String {
    prompt::render(
        sources,
        &prompt::Context {
            home,
            phux: "phux",
            config: Path::new("/home/me/.config/phux/config.toml"),
            date: "2026-01-02",
            prefix: "C-a",
            reference: "- `split-pane` (direction): Split the focused pane\n",
        },
    )
}

#[test]
fn prompt_for_tmux_and_ghostty_matches_the_golden() {
    let home = fixture_home();
    let sources = discover(&home, None, &[]);
    let text = render(&home, &sources);
    let golden = home.parent().expect("testdata").join("prompt.golden.txt");
    if std::env::var_os("PHUX_UPDATE_MIGRATE_GOLDENS").is_some() {
        std::fs::write(&golden, &text).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&golden).unwrap_or_default();
    assert_eq!(
        text, expected,
        "prompt drifted; rerun with PHUX_UPDATE_MIGRATE_GOLDENS=1"
    );
}

#[test]
fn tmux_lines_are_decided_one_by_one() {
    let home = fixture_home();
    let sources = discover(&home, None, &[MigrateSource::Tmux]);
    let [tmux] = sources.as_slice() else {
        panic!("one source: {sources:?}");
    };
    // The sourced file is read, after the file that sources it.
    assert_eq!(tmux.files.len(), 2);
    let verdict = |text: &str| {
        &tmux
            .rows
            .iter()
            .find(|row| row.text == text)
            .unwrap_or_else(|| panic!("no row {text}"))
            .verdict
    };
    assert!(matches!(
        verdict("set -g prefix C-a"),
        Verdict::Default { .. }
    ));
    assert!(matches!(
        verdict("bind C-a send-prefix"),
        Verdict::Default { .. }
    ));
    assert!(
        matches!(verdict("bind -r K resize-pane -U 3"), Verdict::Write { note: Some(note), .. } if note.contains("prefix each time"))
    );
    assert!(
        matches!(verdict("bind -T copy-mode-vi v send -X begin-selection"), Verdict::Skip(reason) if reason.contains("copy-mode"))
    );
    assert!(matches!(verdict("set -g mouse on"), Verdict::Skip(_)));
    // `base-index 1` makes tmux window 1 the first phux tab.
    let Verdict::Write { value, .. } = verdict("bind -n M-1 select-window -t :1") else {
        panic!("select-window is written");
    };
    assert_eq!(
        value.to_string(),
        r#"{ action = "select-window", index = 0 }"#
    );
}

#[test]
fn ghostty_never_binds_cmd_and_names_macos_keys() {
    let home = fixture_home();
    let sources = discover(&home, None, &[MigrateSource::Ghostty]);
    let [ghostty] = sources.as_slice() else {
        panic!("one source: {sources:?}");
    };
    let verdict = |text: &str| {
        &ghostty
            .rows
            .iter()
            .find(|row| row.text == text)
            .unwrap_or_else(|| panic!("no row {text}"))
            .verdict
    };
    assert_eq!(
        verdict("keybind = cmd+q=quit"),
        &Verdict::Skip("macOS uses this key itself".into())
    );
    assert!(
        matches!(verdict("keybind = cmd+d=new_split:right"), Verdict::Skip(reason) if reason.contains("no Cmd"))
    );
    assert!(
        matches!(verdict("keybind = ctrl+shift+t=new_tab"), Verdict::Write { key, .. } if key == "C-S-t")
    );
    assert_eq!(verdict("keybind = ctrl+a>c=new_tab"), &Verdict::Raw);
    assert_eq!(
        verdict("keybind = ctrl+shift+c=copy_to_clipboard"),
        &Verdict::Raw
    );
}

#[test]
fn every_source_is_found_under_a_temp_home() {
    let home = tempfile::tempdir().expect("tempdir");
    let home = home.path();
    let write = |rel: &str, text: &str| {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    };
    write(".config/tmux/tmux.conf", "bind v split-window -h\n");
    write(
        ".config/zellij/config.kdl",
        "theme \"dracula\"\nkeybinds {\n    normal {\n        bind \"Alt h\" { MoveFocus \"Left\"; }\n        unbind \"Ctrl g\"\n    }\n}\n",
    );
    write(
        "Library/Application Support/com.mitchellh.ghostty/config",
        "keybind = ctrl+shift+t=new_tab\nfont-size = 14\n",
    );
    write(
        ".config/herdr/config.toml",
        "[ui]\ntheme = \"x\"\n\n[keys]\nprefix = \"ctrl+b\"\nnew_tab = \"prefix+c\"\n",
    );
    let sources = discover(home, None, &[]);
    let found: Vec<_> = sources.iter().map(|s| (s.kind, s.found())).collect();
    assert_eq!(
        found,
        vec![
            (MigrateSource::Tmux, (1, 0, 0)),
            (MigrateSource::Zellij, (1, 1, 1)),
            (MigrateSource::Ghostty, (1, 0, 1)),
            (MigrateSource::Herdr, (2, 0, 1)),
        ]
    );
    // No `set prefix` line: tmux used C-b, and the agent is told phux keeps its own.
    assert!(
        sources[0]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("C-b"))
    );
    let only = discover(home, None, &[MigrateSource::Herdr]);
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].kind, MigrateSource::Herdr);

    // An empty home has nothing to migrate.
    let empty = tempfile::tempdir().expect("tempdir");
    assert!(discover(empty.path(), None, &[]).is_empty());
}

#[test]
fn json_document_shape_is_stable() {
    let home = fixture_home();
    let sources = discover(&home, None, &[]);
    let doc = document(
        &sources,
        Some(MigrateAgent::Claude),
        Path::new("/c/config.toml"),
        Some(&json!({ "pane": "@3" })),
        42,
    );
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["agent"], "claude");
    assert_eq!(doc["config_path"], "/c/config.toml");
    assert_eq!(doc["launched"], json!({ "pane": "@3" }));
    assert_eq!(doc["prompt_bytes"], 42);
    assert_eq!(doc["sources"][0]["name"], "tmux");
    assert_eq!(doc["sources"][1]["name"], "ghostty");
    let found = &doc["sources"][0]["found"];
    assert!(found["bindings"].as_u64() > Some(0));
    assert!(found["unbindings"].is_u64() && found["settings"].is_u64());
    assert!(doc["sources"][0]["files"][0].is_string());

    let none = document(&[], None, Path::new("/c/config.toml"), None, 0);
    assert_eq!(none["sources"], json!([]));
    assert!(none["agent"].is_null() && none["launched"].is_null());
}

#[test]
fn reference_lists_every_dispatcher_action() {
    let reference = prompt::reference();
    for name in phux_config::vocab::ACTION_NAMES {
        assert!(reference.contains(&format!("- `{name}`")), "{name} missing");
    }
}

#[test]
fn each_agent_takes_the_prompt_its_own_way() {
    let argv = |agent| agent_argv(agent, Path::new("/bin/a"), "hi".into());
    assert_eq!(argv(MigrateAgent::Claude), ["/bin/a", "--", "hi"]);
    assert_eq!(argv(MigrateAgent::Codex), ["/bin/a", "--", "hi"]);
    assert_eq!(argv(MigrateAgent::Opencode), ["/bin/a", "--prompt", "hi"]);
}
