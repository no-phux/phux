use super::*;

#[test]
fn first_matching_config_entry_wins_and_consumes_the_event() {
    // The noop entry consumes the event, so the catch-all does not fire.
    let catalog = catalog_from_toml(
        r#"
        [[hooks.pane-exit]]
        when = { exit-code = 0 }
        action = "noop"

        [[hooks.pane-exit]]
        when = { exit-code = "*" }
        action = { kind = "run", command = "echo boom" }
        "#,
    );
    let clean = HookEvent::new(PANE_EXIT, [("exit-code".to_owned(), "0".to_owned())]);
    assert!(matched_runs(&catalog, &clean, None).is_empty());

    // A non-zero exit skips entry 0 and lands on the catch-all.
    let dirty = HookEvent::new(PANE_EXIT, [("exit-code".to_owned(), "1".to_owned())]);
    let runs = matched_runs(&catalog, &dirty, None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].label, "hooks.pane-exit[1]");
}

#[test]
fn matched_runs_injects_event_env() {
    let catalog = catalog_from_toml(
        r#"
        [[hooks.pane-exit]]
        action = { kind = "run", command = "true" }
        "#,
    );
    let event = HookEvent::new(
        PANE_EXIT,
        [
            ("exit-code".to_owned(), "0".to_owned()),
            ("terminal-id".to_owned(), "7".to_owned()),
        ],
    );
    let runs = matched_runs(&catalog, &event, None);
    assert_eq!(runs.len(), 1);
    let env = &runs[0].spec.env;
    assert!(env.contains(&("PHUX_EVENT".to_owned(), "pane-exit".to_owned())));
    assert!(env.contains(&("PHUX_EXIT_CODE".to_owned(), "0".to_owned())));
    assert!(env.contains(&("PHUX_TERMINAL_ID".to_owned(), "7".to_owned())));
    // No socket configured: no PHUX_SOCKET.
    assert!(env.iter().all(|(key, _)| key != "PHUX_SOCKET"));
}

#[test]
fn matched_runs_injects_server_socket_when_known() {
    let catalog = catalog_from_toml(
        r#"
        [[hooks.pane-exit]]
        action = { kind = "run", command = "true" }
        "#,
    );
    let event = HookEvent::new(PANE_EXIT, []);
    let socket = Path::new("/tmp/phux-test/alt.sock");
    let runs = matched_runs(&catalog, &event, Some(socket));
    assert_eq!(runs.len(), 1);
    assert!(runs[0].spec.env.contains(&(
        "PHUX_SOCKET".to_owned(),
        "/tmp/phux-test/alt.sock".to_owned()
    )));
}

#[test]
fn plugin_events_match_on_name_only_and_all_fire() {
    let hook = |on: &str, id: &str| PluginEventHook {
        plugin_id: "p".to_owned(),
        event_id: id.to_owned(),
        on: on.to_owned(),
        command: vec!["true".to_owned()],
        plugin_root: PathBuf::from("/tmp"),
    };
    let catalog = HookCatalog {
        config_hooks: BTreeMap::new(),
        plugin_events: vec![
            hook(AFTER_NEW_PANE, "a"),
            hook(PANE_EXIT, "b"),
            hook(AFTER_NEW_PANE, "c"),
        ],
    };
    let event = HookEvent::new(AFTER_NEW_PANE, []);
    let runs = matched_runs(&catalog, &event, None);
    assert_eq!(runs.len(), 2, "both after-new-pane plugin hooks fire");
    assert!(runs.iter().all(|run| {
        run.spec
            .env
            .contains(&("PHUX_PLUGIN_ID".to_owned(), "p".to_owned()))
    }));
    assert_eq!(runs[0].spec.cwd.as_deref(), Some(Path::new("/tmp")));
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write");
}

#[test]
fn catalog_skips_disabled_plugins_and_keeps_enabled_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = |id: &str| {
        format!(
            r#"
            id = "{id}"
            name = "Test"
            version = "0.1.0"
            min_phux_version = "0.0.1"

            [[events]]
            id = "greet"
            title = "Greet"
            on = "after-new-pane"
            command = ["true"]
            "#
        )
    };
    write(
        &dir.path().join("plugins/on/phux-plugin.toml"),
        &manifest("plugin-on"),
    );
    write(
        &dir.path().join("plugins/off/phux-plugin.toml"),
        &manifest("plugin-off"),
    );
    let cfg: Config = toml::from_str(
        r#"
        [[plugins]]
        manifest = "plugins/on/phux-plugin.toml"

        [[plugins]]
        manifest = "plugins/off/phux-plugin.toml"
        enabled = false
        "#,
    )
    .expect("valid config");

    let catalog = HookCatalog::from_config(&cfg, &dir.path().join("config.toml"));
    assert_eq!(catalog.plugin_events.len(), 1);
    assert_eq!(catalog.plugin_events[0].plugin_id, "plugin-on");
    assert_eq!(catalog.plugin_events[0].on, AFTER_NEW_PANE);

    let event = HookEvent::new(AFTER_NEW_PANE, []);
    let runs = matched_runs(&catalog, &event, None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].label, "plugin.plugin-on.greet");
}

#[test]
fn catalog_skips_manifest_load_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg: Config = toml::from_str(
        r#"
        [[plugins]]
        manifest = "plugins/missing/phux-plugin.toml"
        "#,
    )
    .expect("valid config");
    let catalog = HookCatalog::from_config(&cfg, &dir.path().join("config.toml"));
    assert!(catalog.plugin_events.is_empty());
    assert!(catalog.is_empty());
}

/// An event whose `on` is not a hook event can never match, so the catalog
/// leaves it out (with a warning) and keeps the plugin's live events.
#[test]
fn catalog_drops_plugin_events_that_name_no_hook_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        &dir.path().join("plugins/p/phux-plugin.toml"),
        r#"
        id = "p"
        name = "Test"
        version = "0.1.0"
        min_phux_version = "0.0.1"

        [[events]]
        id = "typo"
        title = "Typo"
        on = "pane-exited"
        command = ["true"]

        [[events]]
        id = "live"
        title = "Live"
        on = "pane-exit"
        command = ["true"]
        "#,
    );
    let cfg: Config = toml::from_str("[[plugins]]\nmanifest = \"plugins/p/phux-plugin.toml\"\n")
        .expect("valid config");
    let catalog = HookCatalog::from_config(&cfg, &dir.path().join("config.toml"));
    let ids: Vec<&str> = catalog
        .plugin_events
        .iter()
        .map(|hook| hook.event_id.as_str())
        .collect();
    assert_eq!(ids, ["live"]);
}
