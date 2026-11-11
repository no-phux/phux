use std::path::{Path, PathBuf};

use serde_json::json;

use super::sources::{Mode, Table, Verdict, apply_prefix, discover};
use super::{Choices, agent_argv, document, prompt, user_executable};
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
    let sources = discover(&home, None, &[], Mode::Beginner);
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
    let sources = discover(&home, None, &[MigrateSource::Tmux], Mode::Beginner);
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
fn tmux_settings_reloads_and_redundant_unbinds() {
    let home = fixture_home();
    let sources = discover(&home, None, &[MigrateSource::Tmux], Mode::Beginner);
    let verdict = |text: &str| {
        &sources[0]
            .rows
            .iter()
            .find(|row| row.text == text)
            .unwrap_or_else(|| panic!("no row {text}"))
            .verdict
    };
    // Settings: phux's own, translated, phux's behaviour already, or lost.
    assert!(matches!(
        verdict("set -g mouse on"),
        Verdict::Default {
            table: Table::Defaults,
            ..
        }
    ));
    assert!(
        matches!(verdict("set -g history-limit 100000"), Verdict::Write { table: Table::Defaults, key, .. } if key == "history-limit")
    );
    assert!(matches!(verdict("setw -g mode-keys vi"), Verdict::Same(_)));
    assert!(
        matches!(verdict("set -g base-index 1"), Verdict::Skip(reason) if reason.contains("from 0"))
    );
    // A trailing `display` does not hide the reload.
    assert!(
        matches!(verdict(r#"bind r source-file ~/.tmux.conf \; display "reloaded""#), Verdict::Write { value, .. } if value.as_str() == Some("reload-config"))
    );
    // Beginner keeps a split key phux also has on `-` and `|`.
    assert!(matches!(verdict("unbind '\"'"), Verdict::Same(reason) if reason.contains("`-`")));
    assert!(matches!(verdict("unbind %"), Verdict::Same(_)));
}

#[test]
fn advanced_mode_writes_tmux_unbinds() {
    let home = fixture_home();
    let sources = discover(&home, None, &[MigrateSource::Tmux], Mode::Advanced);
    let row = sources[0]
        .rows
        .iter()
        .find(|row| row.text == "unbind %")
        .expect("unbind row");
    assert!(
        matches!(&row.verdict, Verdict::Write { value, replaces: Some(_), .. } if value.as_str() == Some("none"))
    );
}

#[test]
fn the_prefix_follows_the_users_choice() {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home.path().join(".tmux.conf");
    std::fs::write(&path, "bind v split-window -h\n").expect("write");
    let choose = |chosen: &str| {
        let mut sources = discover(home.path(), None, &[], Mode::Beginner);
        assert_eq!(sources[0].prefix.as_deref(), Some("C-b"));
        apply_prefix(&mut sources, chosen, "C-a");
        sources.remove(0)
    };
    // Keeping phux's prefix writes nothing, and the note says so.
    let kept = choose("C-a");
    assert_eq!(kept.rows.len(), 1);
    assert!(
        kept.note
            .as_deref()
            .is_some_and(|n| n.contains("keep phux's"))
    );
    // tmux's built-in C-b gets a stand-in line, written.
    let tmux = choose("C-b");
    let row = tmux.rows.last().expect("prefix row");
    assert_eq!(row.line, 0);
    assert!(
        matches!(&row.verdict, Verdict::Write { table: Table::Keybindings, value, .. } if value.as_str() == Some("C-b"))
    );
    let text = render(home.path(), std::slice::from_ref(&tmux));
    assert!(text.contains("(built-in default, no line) `set -g prefix C-b`"));
    assert!(text.contains(r#"write `prefix = "C-b"` in `[keybindings]`"#));
}

#[test]
fn ghostty_never_binds_cmd_and_names_macos_keys() {
    let home = fixture_home();
    let sources = discover(&home, None, &[MigrateSource::Ghostty], Mode::Beginner);
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
        matches!(verdict("keybind = cmd+d=new_split:right"), Verdict::Skip(reason) if reason.contains("no Cmd") && reason.contains("Ghostty's own"))
    );
    assert!(
        matches!(verdict("keybind = ctrl+shift+t=new_tab"), Verdict::Write { key, .. } if key == "C-S-t")
    );
    assert_eq!(verdict("keybind = ctrl+a>c=new_tab"), &Verdict::Raw);
    assert!(matches!(
        verdict("font-family = JetBrains Mono"),
        Verdict::Same(_)
    ));
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
    let sources = discover(home, None, &[], Mode::Beginner);
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
    // No `set prefix` line: tmux used C-b.
    assert_eq!(sources[0].prefix.as_deref(), Some("C-b"));
    let only = discover(home, None, &[MigrateSource::Herdr], Mode::Beginner);
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].kind, MigrateSource::Herdr);

    // An empty home has nothing to migrate.
    let empty = tempfile::tempdir().expect("tempdir");
    assert!(discover(empty.path(), None, &[], Mode::Beginner).is_empty());
}

#[test]
fn json_document_shape_is_stable() {
    let home = fixture_home();
    let sources = discover(&home, None, &[], Mode::Beginner);
    let choices = Choices {
        mode: Mode::Beginner,
        phux_prefix: "C-a".into(),
        tmux_prefix: Some("C-b".into()),
        prefix: "C-a".into(),
    };
    let doc = document(
        &sources,
        Some(MigrateAgent::Claude),
        Path::new("/c/config.toml"),
        Some(&json!({ "pane": "@3" })),
        42,
        &choices,
    );
    assert_eq!(doc["mode"], "beginner");
    assert_eq!(
        doc["prefix"],
        json!({ "phux": "C-a", "tmux": "C-b", "chosen": "C-a" })
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

    let none = document(&[], None, Path::new("/c/config.toml"), None, 0, &choices);
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

#[cfg(unix)]
#[test]
fn an_agent_off_path_is_found_where_its_installer_puts_it() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().expect("tempdir");
    let bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let claude = bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(
        user_executable("claude", Some(home.path())),
        Some(claude.canonicalize().expect("canonical"))
    );
    assert_eq!(
        user_executable("no-such-agent-xyz", Some(home.path())),
        None
    );
}
