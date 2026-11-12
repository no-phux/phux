use std::path::{Path, PathBuf};

use serde_json::json;

use super::sources::{Setup, Source, Verdict, discover};
use super::{agent_argv, decide, document, prompt, selected_theme, user_executable};
use crate::commands::{MigrateAgent, MigrateSource};

fn testdata() -> PathBuf {
    // Read at run time: a compiled-in checkout path defeats the build cache.
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
        .join("src/commands/migrate/testdata")
}

fn fixture_home() -> PathBuf {
    testdata().join("home")
}

/// The home of the run issue #2255 reports: tmux on its default prefix,
/// with Ghostty.
fn run_home() -> PathBuf {
    testdata().join("run")
}

fn find(sources: &[Source], kind: MigrateSource) -> &Source {
    sources
        .iter()
        .find(|s| s.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} source"))
}

fn verdict<'a>(source: &'a Source, text: &str) -> &'a Verdict {
    &source
        .rows
        .iter()
        .find(|row| row.text == text)
        .unwrap_or_else(|| panic!("no row {text}"))
        .verdict
}

fn assert_golden(text: &str, name: &str) {
    let golden = testdata().join(name);
    if std::env::var_os("PHUX_UPDATE_MIGRATE_GOLDENS").is_some() {
        std::fs::write(&golden, text).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&golden).unwrap_or_default();
    assert_eq!(
        text, expected,
        "{name} drifted; rerun with PHUX_UPDATE_MIGRATE_GOLDENS=1"
    );
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
    let sources = discover(&home, None, &[], None, &Setup::default());
    let text = render(&home, &sources);
    assert_golden(&text, "prompt.golden.txt");
    // No phux theme selected: the Ghostty theme line keeps the pointer.
    assert!(text.contains("pick one with `phux theme list` and `phux theme set`"));
    assert!(!text.contains("kept your phux theme"));
}

#[test]
fn prompt_for_the_issue_run_matches_the_golden() {
    let home = run_home();
    let sources = discover(&home, None, &[], None, &Setup::default());
    assert_golden(&render(&home, &sources), "run.golden.txt");
}

#[test]
fn the_prefix_is_kept_or_taken_from_tmux_as_chosen() {
    let home = run_home();
    let only = [MigrateSource::Tmux];
    let kept = discover(&home, None, &only, None, &Setup::default());
    let tmux = find(&kept, MigrateSource::Tmux);
    assert_eq!(tmux.prefix.as_deref(), Some("C-b"));
    assert!(
        tmux.note
            .as_deref()
            .is_some_and(|n| n.contains("keep phux's own prefix, C-a"))
    );
    let taken = discover(
        &home,
        None,
        &only,
        None,
        &Setup {
            prefix: Some("C-b".into()),
            ..Setup::default()
        },
    );
    let note = find(&taken, MigrateSource::Tmux).note.as_deref();
    assert!(note.is_some_and(|n| n.contains(r#"write `prefix = "C-b"` in `[keybindings]`"#)));

    // A `set prefix` line other than the one chosen is not written.
    let home = fixture_home();
    let taken = discover(
        &home,
        None,
        &only,
        None,
        &Setup {
            prefix: Some("C-b".into()),
            ..Setup::default()
        },
    );
    let tmux = find(&taken, MigrateSource::Tmux);
    assert!(
        matches!(verdict(tmux, "set -g prefix C-a"), Verdict::Covered(r) if r.contains("not tmux's C-a"))
    );
    assert!(
        tmux.note
            .as_deref()
            .is_some_and(|n| n.contains("tmux's prefix was C-a"))
    );
}

#[test]
fn a_selected_phux_theme_is_kept_over_ghostty_s() {
    let home = fixture_home().with_file_name("theme-kept");
    let theme = selected_theme(&home.join(".config/phux/config.toml"));
    assert_eq!(theme.as_deref(), Some("tokyo-night"));
    let sources = discover(&home, None, &[], theme.as_deref(), &Setup::default());
    let text = render(&home, &sources);
    let golden = home.with_file_name("theme-kept.golden.txt");
    if std::env::var_os("PHUX_UPDATE_MIGRATE_GOLDENS").is_some() {
        std::fs::write(&golden, &text).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&golden).unwrap_or_default();
    assert_eq!(
        text, expected,
        "prompt drifted; rerun with PHUX_UPDATE_MIGRATE_GOLDENS=1"
    );
    assert!(text.contains(
        "kept your phux theme `tokyo-night`; Ghostty's `dark:TokyoNight Storm,light:Catppuccin Latte` is not applied"
    ));
    assert!(!text.contains("pick one"));
    // Never written: the row is a skip, not a write.
    let row = sources[0]
        .rows
        .iter()
        .find(|row| row.text.starts_with("theme ="))
        .expect("theme row");
    assert!(matches!(row.verdict, Verdict::Skip(_)));
    // No config, no theme.
    assert_eq!(selected_theme(&home.join("missing.toml")), None);
}

#[test]
fn advanced_takes_tmux_s_prefix_without_asking() {
    let home = run_home();
    let advanced = Setup {
        advanced: true,
        ..Setup::default()
    };
    let (setup, _) = decide(advanced, &home, None, &[], None, false).expect("decides");
    assert_eq!(setup.prefix.as_deref(), Some("C-b"));
    let (setup, _) = decide(Setup::default(), &home, None, &[], None, false).expect("decides");
    assert_eq!(setup.prefix, None);
    let bad = Setup {
        prefix: Some("ctrl+b".into()),
        ..Setup::default()
    };
    assert!(decide(bad, &home, None, &[], None, false).is_err());
}

#[test]
fn lines_with_nothing_to_do_are_kept_apart_from_lost_ones() {
    let home = run_home();
    let sources = discover(&home, None, &[], None, &Setup::default());
    let tmux = find(&sources, MigrateSource::Tmux);
    let ghostty = find(&sources, MigrateSource::Ghostty);
    for text in [
        "set -g renumber-windows on",
        "set -sg escape-time 0",
        "set -g set-clipboard on",
        "setw -g mode-keys vi",
    ] {
        assert!(
            matches!(verdict(tmux, text), Verdict::Covered(r) if r.starts_with("already phux's behavior")),
            "{text}"
        );
    }
    assert!(
        matches!(verdict(tmux, "set -g base-index 1"), Verdict::Skip(r) if r.contains("from 0"))
    );
    assert!(
        matches!(verdict(ghostty, "font-size = 14"), Verdict::Covered(r) if r.contains("keeps working"))
    );
}

#[test]
fn a_reload_binding_with_a_message_maps_to_reload_config() {
    let home = fixture_home();
    let sources = discover(&home, None, &[MigrateSource::Tmux], None, &Setup::default());
    let tmux = find(&sources, MigrateSource::Tmux);
    let verdict = |text| verdict(tmux, text);
    // A trailing `display` is cosmetic: the reload binding maps, and on a
    // key phux binds otherwise it replaces the default.
    assert!(
        matches!(verdict(r#"bind r source-file ~/.tmux.conf \; display "reloaded""#), Verdict::Write { value, replaces: None, .. } if value.as_str() == Some("reload-config"))
    );
    assert!(
        matches!(verdict(r#"bind R source-file ~/.tmux.conf \; display-message "reloaded""#), Verdict::Write { value, replaces: Some(_), .. } if value.as_str() == Some("reload-config"))
    );
}

#[test]
fn tmux_lines_are_decided_one_by_one() {
    let home = fixture_home();
    let sources = discover(&home, None, &[MigrateSource::Tmux], None, &Setup::default());
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
    // Settings phux has go through the `config import-tmux` translator.
    assert!(matches!(
        verdict("set -g mouse on"),
        Verdict::Default { .. }
    ));
    assert!(
        matches!(verdict("set -g status-position bottom"), Verdict::Write { key, value, .. } if key == "position" && value.as_str() == Some("bottom"))
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
fn ghostty_never_binds_cmd_and_names_macos_keys() {
    let home = fixture_home();
    let sources = discover(
        &home,
        None,
        &[MigrateSource::Ghostty],
        None,
        &Setup::default(),
    );
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
    write(
        ".config/rex/init.lua",
        "-- mine\nrex.bind(\"ctrl+b>c\", \"client.tab.new\")\nrex.bind(\"ctrl+x\", function(ctx, ev)\n  return true\nend)\nrex.unbind(\"cmd+k\")\n",
    );
    let sources = discover(home, None, &[], None, &Setup::default());
    let found: Vec<_> = sources.iter().map(|s| (s.kind, s.found())).collect();
    assert_eq!(
        found,
        vec![
            (MigrateSource::Tmux, (1, 0, 0)),
            (MigrateSource::Zellij, (1, 1, 1)),
            (MigrateSource::Ghostty, (1, 0, 1)),
            (MigrateSource::Herdr, (2, 0, 1)),
            (MigrateSource::Rex, (2, 1, 0)),
        ]
    );
    // No `set prefix` line: tmux used C-b, and the agent is told phux keeps its own.
    assert!(
        sources[0]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("C-b"))
    );
    // zellij's and herdr's theme lines follow the same rule as Ghostty's.
    let kept = discover(home, None, &[], Some("mine"), &Setup::default());
    let themes: Vec<_> = kept
        .iter()
        .flat_map(|s| &s.rows)
        .filter(|r| r.text.starts_with("theme"))
        .collect();
    assert_eq!(themes.len(), 2, "{themes:?}");
    for row in themes {
        assert!(
            matches!(&row.verdict, Verdict::Skip(r) if r.starts_with("kept your phux theme `mine`")),
            "{row:?}"
        );
    }
    let only = discover(home, None, &[MigrateSource::Herdr], None, &Setup::default());
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].kind, MigrateSource::Herdr);

    // An empty home has nothing to migrate.
    let empty = tempfile::tempdir().expect("tempdir");
    assert!(discover(empty.path(), None, &[], None, &Setup::default()).is_empty());
}

#[test]
fn json_document_shape_is_stable() {
    let home = run_home();
    let sources = discover(&home, None, &[], None, &Setup::default());
    let doc = document(
        &sources,
        Some(MigrateAgent::Claude),
        Path::new("/c/config.toml"),
        Some("tokyo-night"),
        Some(&json!({ "pane": "@3" })),
        42,
        &Setup::default(),
    );
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["agent"], "claude");
    assert_eq!(doc["config_path"], "/c/config.toml");
    assert_eq!(doc["theme"], "tokyo-night");
    assert_eq!(doc["launched"], json!({ "pane": "@3" }));
    assert_eq!(doc["prompt_bytes"], 42);
    assert_eq!(doc["sources"][0]["name"], "tmux");
    assert_eq!(doc["sources"][1]["name"], "ghostty");
    let found = &doc["sources"][0]["found"];
    assert!(found["bindings"].as_u64() > Some(0));
    assert!(found["unbindings"].is_u64() && found["settings"].is_u64());
    assert!(doc["sources"][0]["files"][0].is_string());

    assert_eq!(
        doc["prefix"],
        json!({ "chosen": "C-a", "tmux": "C-b", "phux": "C-a" })
    );

    let none = document(
        &[],
        None,
        Path::new("/c/config.toml"),
        None,
        None,
        0,
        &Setup::default(),
    );
    assert_eq!(none["sources"], json!([]));
    assert!(none["agent"].is_null() && none["launched"].is_null() && none["theme"].is_null());
    assert!(none["prefix"]["tmux"].is_null());
    assert_eq!(none["mode"], "beginner");
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
