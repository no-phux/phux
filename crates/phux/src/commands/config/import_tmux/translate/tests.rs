use super::*;

fn make_plan(keys: &str) -> Plan {
    translate(&Snapshot {
        session: "prefix C-b\nprefix2 None\nmouse on\nhistory-limit 10000\ndefault-shell '/bin/sh'\nstatus-position bottom\nbase-index 1\n".into(),
        window: "mode-keys vi\npane-base-index 1\n".into(),
        keys: keys.into(),
    }).unwrap()
}

fn table(plan: &Plan) -> Table {
    toml::from_str(&plan.toml).unwrap()
}

#[test]
fn maps_common_keys_preferences_and_unbinds_phux_only_actions() {
    let plan = make_plan(
        r##"bind-key -T prefix v split-window -h
bind-key -r -T prefix h select-pane -L
bind-key -T root M-Up resize-pane -U 5
bind-key -T prefix 1 select-window -t :=1
bind-key -T prefix C-b send-prefix
bind-key -T prefix r source-file ~/.tmux.conf
bind-key -T prefix & confirm-before -p "kill-window #W?" kill-window
bind-key -T prefix x confirm-before -p "kill-pane #P?" kill-pane
bind-key -T prefix , command-prompt -I "#W" { rename-window "%%" }
"##,
    );
    let cfg = phux_config::parse_with_defaults(&plan.toml, Path::new("import.toml")).unwrap();
    assert_eq!(cfg.keybindings.prefix, "C-b");
    assert_eq!(cfg.defaults.history_limit, 10000);
    assert_eq!(cfg.defaults.shell.as_deref(), Some("/bin/sh"));
    let table = table(&plan);
    let keys = &table["keybindings"]["prefix-table"];
    assert_eq!(keys["v"]["direction"].as_str(), Some("vertical"));
    assert_eq!(keys["1"]["index"].as_integer(), Some(0));
    assert_eq!(keys["C-b"].as_str(), Some("none"));
    assert_eq!(keys["q"].as_str(), Some("none"));
    assert_eq!(keys["x"].as_str(), Some("kill-pane"));
    assert_eq!(keys["&"].as_str(), Some("kill-window"));
    assert_eq!(keys[","].as_str(), Some("rename-window"));
    assert_eq!(
        table["keybindings"]["global"]["M-Up"]["amount"].as_integer(),
        Some(5)
    );
    assert!(
        plan.entries
            .iter()
            .any(|e| e.detail.contains("repeat timing"))
    );
}

#[test]
fn rejects_scripts_chains_unknown_flags_targets_and_confirmation_loss() {
    let plan = make_plan(
        r##"bind-key -T prefix x confirm-before -p "kill?" kill-window -t :2
bind-key -T prefix c new-window \; kill-session
bind-key -T prefix v split-window -h -c "#{pane_current_path}"
bind-key -T prefix q run-shell 'touch /tmp/must-not-run'
bind-key -T prefix z resize-pane -Z -t %123
bind-key -T prefix 2 select-window -t other:2
bind-key -T prefix 0 select-window -t :=0
bind-key -T prefix 3 select-window -t :+1
bind-key -T copy-mode-vi y send-keys -X copy-pipe-and-cancel 'pbcopy'
"##,
    );
    let table = table(&plan);
    for key in ["x", "c", "v", "q", "z", "2", "0", "3"] {
        assert_eq!(
            table["keybindings"]["prefix-table"][key].as_str(),
            Some("none"),
            "{key}"
        );
    }
    assert_eq!(
        plan.entries
            .iter()
            .filter(|e| e.status == "unsupported" && e.source.starts_with("bind-key"))
            .count(),
        8
    );
    assert!(
        plan.entries
            .iter()
            .any(|e| e.source == "table copy-mode-vi")
    );
}

#[test]
fn quoted_punctuation_and_aliases_are_single_keys() {
    let plan = make_plan(
        r#"bind-key -T prefix \" split-window
bind-key -T prefix \% split-window -h
bind-key -T prefix BSpace last-pane
bind-key -T prefix Space new-window
bind-key -T prefix \; last-pane
bind-key -T root C-b new-window
"#,
    );
    let table = table(&plan);
    let keys = &table["keybindings"]["prefix-table"];
    assert_eq!(keys["\""]["direction"].as_str(), Some("horizontal"));
    assert_eq!(keys["Backspace"].as_str(), Some("last-pane"));
    assert_eq!(keys["Space"].as_str(), Some("new-window"));
    assert_eq!(keys[";"].as_str(), Some("last-pane"));
    assert!(
        table["keybindings"]["global"]
            .as_table()
            .unwrap()
            .is_empty()
    );
    assert!(chord("a b").is_err());
}

#[test]
fn phux_key_collisions_are_disabled_and_shifted_defaults_replaced() {
    let plan = make_plan("bind-key -T prefix S-x new-window\nbind-key -T prefix X kill-pane\n");
    let table = table(&plan);
    let keys = table["keybindings"]["prefix-table"].as_table().unwrap();
    assert!(!keys.contains_key("S-x"));
    assert_eq!(keys["X"].as_str(), Some("none"));
    assert!(plan.entries.iter().any(|e| e.detail.contains("collides")));
    assert!(
        plan.entries
            .iter()
            .filter(|e| e.source.starts_with("bind-key"))
            .all(|e| e.status == "unsupported")
    );
    let second = make_plan("bind-key -T prefix S-x new-window\n");
    assert!(
        phux_config::check::check(&second.toml, Path::new("import.toml"))
            .unwrap()
            .is_ok()
    );
}

#[test]
fn refuses_malformed_acquisition_and_invalid_prefix() {
    assert!(binding("bind-key -T prefix q run-shell 'broken").is_err());
    assert!(binding("not-a-binding").is_err());
    assert!(parse_options("prefix C-b unexpected").is_err());
    assert_eq!(
        parse_options("pane-colours\nstatus-left ''\n").unwrap()["pane-colours"],
        ""
    );
    for session in ["", "prefix None\n", "prefix C-b\nmouse invalid\n"] {
        let translated = translate(&Snapshot {
            session: session.into(),
            window: String::new(),
            keys: String::new(),
        });
        if session.contains("mouse") {
            assert!(
                translated
                    .unwrap()
                    .entries
                    .iter()
                    .any(|e| e.source == "mouse" && e.status == "unsupported")
            );
        } else {
            assert!(translated.is_err());
        }
    }
}

#[test]
fn a_trailing_display_message_is_dropped_before_mapping() {
    let plan = make_plan(
        r#"bind-key -T prefix r source-file ~/.tmux.conf \; display-message "tmux.conf reloaded"
bind-key -T prefix c new-window \; display hi \; kill-session
"#,
    );
    let table = table(&plan);
    let prefix = &table["keybindings"]["prefix-table"];
    assert_eq!(prefix["r"].as_str(), Some("reload-config"));
    // Only a chain that ends in the message: anything after it still refuses.
    assert_eq!(prefix["c"].as_str(), Some("none"));
}

#[test]
fn functional_display_tails_are_not_discarded_during_import() {
    for tail in [
        "display-message -I -t %1",
        "display-message -I",
        "display-message -t %1 hi",
        "display -p hi",
        "display-message -d 0 hi",
        "display-message",
        "display hi there",
        "display-message '#(not-executed)'",
        "display-message '#{pane_id}'",
        "display hi \\; kill-session",
    ] {
        let plan = make_plan(&format!(
            "bind-key -T prefix r source-file ~/.tmux.conf \\; {tail}\n"
        ));
        assert_eq!(
            table(&plan)["keybindings"]["prefix-table"]["r"].as_str(),
            Some("none"),
            "{tail}"
        );
        assert!(
            plan.entries.iter().any(|entry| {
                entry.source.starts_with("bind-key") && entry.status == "unsupported"
            }),
            "{tail}"
        );
    }
}

#[test]
fn clipboard_privacy_preferences_are_imported() {
    for (source, expected) in [("off", "deny"), ("on", "allow"), ("external", "deny")] {
        let plan = translate(&Snapshot {
            session: format!("prefix C-b\nset-clipboard {source}\n"),
            window: String::new(),
            keys: String::new(),
        })
        .expect("translation");
        assert_eq!(
            table(&plan)["defaults"]["clipboard-write"].as_str(),
            Some(expected)
        );
    }
}
