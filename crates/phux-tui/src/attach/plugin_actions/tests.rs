//! Plugin action entries, binding merges, and failure toasts.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;
use phux_config::keybind::{Feed, parse_chord};

fn entry(plugin: &str, action: &str, keys: Option<&str>) -> PluginActionEntry {
    PluginActionEntry {
        plugin_id: plugin.to_owned(),
        plugin_name: format!("{plugin} name"),
        action_id: action.to_owned(),
        title: format!("{action} title"),
        keys: keys.map(ToOwned::to_owned),
    }
}

fn base_kb() -> KeybindingsCfg {
    let mut prefix_table = BTreeMap::new();
    prefix_table.insert("d".to_owned(), Action::Bare("detach".to_owned()));
    KeybindingsCfg {
        prefix: "C-a".to_owned(),
        prefix_table,
        global: BTreeMap::new(),
        ..KeybindingsCfg::default()
    }
}

#[test]
fn merge_inserts_plugin_binding_and_resolver_fires_it() {
    let mut kb = base_kb();
    merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("g"))]);
    assert!(kb.prefix_table.contains_key("g"), "binding merged");

    // The merged table resolves `C-a g` to the plugin action.
    let mut resolver = Resolver::new(&kb).expect("merged table builds");
    assert_eq!(
        resolver.feed(parse_chord("C-a").unwrap()),
        Feed::Partial,
        "prefix chord starts the sequence"
    );
    let Feed::Resolved(fired) = resolver.feed(parse_chord("g").unwrap()) else {
        panic!("expected plugin binding to resolve");
    };
    assert_eq!(fired.action, PLUGIN_ACTION_NAME);
    assert_eq!(plugin_action_target(&fired.args), Some(("p", "a")));
    assert_eq!(
        fired.args.get("id"),
        Some(&toml::Value::String("a".to_owned())),
        "merged bindings spell the action id as a config binding would"
    );
}

#[test]
fn merge_leaves_a_key_the_config_unbound_unbound_in_any_spelling() {
    let cfg = phux_config::parse_with_defaults(
        "[keybindings.prefix-table]\n\"?\" = \"none\"\n\"g s\" = \"none\"\n",
        std::path::Path::new("config.toml"),
    )
    .expect("config parses");
    let mut kb = cfg.keybindings;
    merge_plugin_bindings(
        &mut kb,
        &[
            entry("p", "help", Some("S-/")),
            entry("p", "seq", Some("g s")),
            entry("p", "free", Some("M-g")),
        ],
    );
    assert!(!kb.prefix_table.contains_key("S-/"), "`?` stays unbound");
    assert!(!kb.prefix_table.contains_key("g s"), "`g s` stays unbound");
    assert!(
        kb.prefix_table.contains_key("M-g"),
        "other plugin keys merge"
    );
}

#[test]
fn plugin_action_target_reads_id_or_the_older_action_arg() {
    let args = |pairs: &[(&str, &str)]| -> BTreeMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), toml::Value::String((*v).to_owned())))
            .collect()
    };
    assert_eq!(
        plugin_action_target(&args(&[("plugin", "p"), ("id", "a")])),
        Some(("p", "a"))
    );
    assert_eq!(
        plugin_action_target(&args(&[("plugin", "p"), ("action", "a")])),
        Some(("p", "a"))
    );
    assert_eq!(
        plugin_action_target(&args(&[("plugin", "p"), ("id", "new"), ("action", "old")])),
        Some(("p", "new")),
        "`id` wins"
    );
    assert_eq!(plugin_action_target(&args(&[("plugin", "p")])), None);
    assert_eq!(plugin_action_target(&args(&[("id", "a")])), None);
}

#[test]
fn merge_user_binding_wins_on_exact_chord_conflict() {
    let mut kb = base_kb(); // user binds `d` -> detach
    merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("d"))]);
    assert_eq!(
        kb.prefix_table.get("d"),
        Some(&Action::Bare("detach".to_owned())),
        "user's binding must survive the merge untouched"
    );
    assert_eq!(kb.prefix_table.len(), 1, "plugin binding dropped");
}

#[test]
fn merge_user_binding_wins_on_chord_equivalent_spelling() {
    // `?` and `S-/` are the same chord (shifted-glyph rule); a plugin
    // spelling the user's chord differently must still lose.
    let mut kb = base_kb();
    kb.prefix_table
        .insert("?".to_owned(), Action::Bare("show-help".to_owned()));
    merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("S-/"))]);
    assert!(
        !kb.prefix_table.contains_key("S-/"),
        "chord-equivalent plugin binding dropped"
    );
    assert_eq!(
        kb.prefix_table.get("?"),
        Some(&Action::Bare("show-help".to_owned()))
    );
    // The surviving table still builds a resolver (nothing poisoned).
    Resolver::new(&kb).expect("table remains valid");
}

#[test]
fn merge_drops_ambiguous_prefix_binding_without_panicking() {
    // User binds the two-chord sequence `g s`; a plugin claiming bare
    // `g` would shadow it (ambiguous prefix). The plugin loses.
    let mut kb = base_kb();
    kb.prefix_table
        .insert("g s".to_owned(), Action::Bare("new-window".to_owned()));
    merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("g"))]);
    assert!(
        !kb.prefix_table.contains_key("g"),
        "ambiguous plugin chord dropped"
    );
    Resolver::new(&kb).expect("user bindings unaffected");
}

#[test]
fn merge_drops_unparsable_chord_without_panicking() {
    let mut kb = base_kb();
    merge_plugin_bindings(&mut kb, &[entry("p", "a", Some("NotAKey-"))]);
    assert!(!kb.prefix_table.contains_key("NotAKey-"));
    Resolver::new(&kb).expect("user bindings unaffected");
}

#[test]
fn merge_first_plugin_wins_between_two_plugins() {
    let mut kb = base_kb();
    merge_plugin_bindings(
        &mut kb,
        &[
            entry("first", "a", Some("g")),
            entry("second", "b", Some("g")),
        ],
    );
    let Some(Action::Parameterized(p)) = kb.prefix_table.get("g") else {
        panic!("expected merged parameterized binding");
    };
    assert_eq!(
        p.args.get("plugin"),
        Some(&toml::Value::String("first".to_owned())),
        "config-order first plugin keeps the chord"
    );
}

#[test]
fn failure_toast_is_none_on_success() {
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Ok(phux_plugin::PluginActionOutput {
            schema_version: 1,
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            command: vec!["true".to_owned()],
            cwd: std::path::PathBuf::from("/"),
            outcome: phux_plugin::PluginActionOutcome::Completed,
            exit_code: Some(0),
            stdout: "fine".to_owned(),
            stderr: String::new(),
            duration_ms: 1,
        }),
    };
    assert!(run_succeeded(&result));
    assert!(failure_toast(&result).is_none());
}

#[test]
fn failure_toast_carries_exit_code_and_stderr_tail() {
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Ok(phux_plugin::PluginActionOutput {
            schema_version: 1,
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            command: vec!["false".to_owned()],
            cwd: std::path::PathBuf::from("/"),
            outcome: phux_plugin::PluginActionOutcome::Completed,
            exit_code: Some(2),
            stdout: String::new(),
            stderr: "boom: file not found".to_owned(),
            duration_ms: 1,
        }),
    };
    let Toast { title, lines } = failure_toast(&result).expect("failure produces a toast");
    assert_eq!(title, "Plugin action failed");
    assert_eq!(lines[0], "p a");
    assert!(lines.iter().any(|l| l.contains("exit code 2")));
    assert!(lines.iter().any(|l| l.contains("boom: file not found")));
}

#[test]
fn failure_toast_elides_long_output_keeping_the_tail() {
    let mut stderr = String::new();
    for i in 0..100 {
        use std::fmt::Write as _;
        let _ = writeln!(stderr, "line {i}");
    }
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Ok(phux_plugin::PluginActionOutput {
            schema_version: 1,
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            command: vec!["x".to_owned()],
            cwd: std::path::PathBuf::from("/"),
            outcome: phux_plugin::PluginActionOutcome::Completed,
            exit_code: Some(1),
            stdout: String::new(),
            stderr,
            duration_ms: 1,
        }),
    };
    let lines = failure_toast(&result).expect("toast").lines;
    assert!(lines.len() <= TOAST_MAX_LINES + 1, "got {}", lines.len());
    assert!(
        lines.iter().any(|l| l.contains("line 99")),
        "tail line kept: {lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("elided")));
}

#[test]
fn failure_toast_reports_pre_exec_error() {
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Err("plugin \"p\" is not configured".to_owned()),
    };
    let lines = failure_toast(&result).expect("toast").lines;
    assert!(lines[1].contains("not configured"));
}

#[test]
fn entries_flatten_manifest_actions_with_keys() {
    // In-memory manifest shape; only the fields entries care about
    // are meaningful here.
    let manifest = PluginManifest {
        id: "com.example.tools".to_owned(),
        name: "Tools".to_owned(),
        version: "0.1.0".to_owned(),
        min_phux_version: "0.0.2".to_owned(),
        description: None,
        manifest_path: std::path::PathBuf::from("/x/phux-plugin.toml"),
        plugin_root: std::path::PathBuf::from("/x"),
        platforms: None,
        build: Vec::new(),
        agents: Vec::new(),
        actions: vec![
            phux_config::plugin::PluginManifestAction {
                id: "summarize".to_owned(),
                title: "Summarize pane".to_owned(),
                description: None,
                contexts: Vec::new(),
                platforms: None,
                command: vec!["true".to_owned()],
                keys: Some("g".to_owned()),
            },
            phux_config::plugin::PluginManifestAction {
                id: "report".to_owned(),
                title: "Report".to_owned(),
                description: None,
                contexts: Vec::new(),
                platforms: None,
                command: vec!["true".to_owned()],
                keys: None,
            },
        ],
        events: Vec::new(),
        panes: Vec::new(),
        links: Vec::new(),
        profiles: Vec::new(),
        widgets: Vec::new(),
        sidebar: Vec::new(),
        themes: Vec::new(),
    };
    let entries = entries_from_manifests(&[manifest]);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].palette_label(), "plugin: Tools: Summarize pane");
    assert_eq!(entries[0].keys.as_deref(), Some("g"));
    assert_eq!(entries[1].keys, None);
}
