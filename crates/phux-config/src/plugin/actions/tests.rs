//! Plugin action discovery and binding precedence.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;
use crate::keybind::{Feed, parse_chord};

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
    let cfg = crate::parse_with_defaults(
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
fn entries_flatten_manifest_actions_with_keys() {
    // In-memory manifest shape; only the fields entries care about
    // are meaningful here.
    let mut manifest = PluginManifest {
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
            crate::plugin::PluginManifestAction {
                id: "summarize".to_owned(),
                title: "Summarize pane".to_owned(),
                description: None,
                contexts: Vec::new(),
                platforms: None,
                command: vec!["true".to_owned()],
                keys: Some("g".to_owned()),
            },
            crate::plugin::PluginManifestAction {
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
        startup: Vec::new(),
    };
    let mut hidden_manifest = manifest.clone();
    hidden_manifest.id = "com.example.hidden".to_owned();
    hidden_manifest.platforms = Some(Vec::new());
    let hidden_action = PluginManifestAction {
        id: "hidden".to_owned(),
        platforms: Some(Vec::new()),
        ..manifest.actions[0].clone()
    };
    manifest.actions.insert(1, hidden_action);
    let entries = entries_from_manifests(&[hidden_manifest, manifest]);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].plugin_name, "Tools");
    assert_eq!(entries[0].title, "Summarize pane");
    assert_eq!(entries[0].action_id, "summarize");
    assert_eq!(entries[1].action_id, "report");
    assert_eq!(entries[0].keys.as_deref(), Some("g"));
    assert_eq!(entries[1].keys, None);
}
