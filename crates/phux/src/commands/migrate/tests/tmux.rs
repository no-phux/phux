use super::*;
use crate::commands::migrate::sources::Table;

fn source(config: &str, advanced: bool) -> Source {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(home.path().join(".tmux.conf"), config).expect("config");
    discover(
        home.path(),
        None,
        &[MigrateSource::Tmux],
        None,
        &Setup {
            advanced,
            ..Setup::default()
        },
    )
    .remove(0)
}

#[test]
fn redundant_unbinds_do_not_resurrect_overridden_bindings() {
    for advanced in [false, true] {
        let tmux = source(
            "bind '\"' new-window\nunbind '\"'\nbind - split-window -v\n",
            advanced,
        );
        assert!(
            matches!(verdict(&tmux, "bind '\"' new-window"), Verdict::Covered(reason) if reason.contains("later"))
        );
        let unbind = verdict(&tmux, "unbind '\"'");
        if advanced {
            assert!(
                matches!(unbind, Verdict::Write { value, .. } if value.as_str() == Some("none"))
            );
        } else {
            assert!(
                matches!(unbind, Verdict::Covered(reason) if reason.contains("advanced mode writes it"))
            );
        }
    }
}

#[test]
fn an_overridden_alternative_cannot_make_an_unbind_redundant() {
    for replacement in ["bind - new-window", "unbind -"] {
        let tmux = source(
            &format!("unbind '\"'\nbind - split-window -v\n{replacement}\n"),
            false,
        );
        assert!(
            matches!(verdict(&tmux, "bind - split-window -v"), Verdict::Covered(reason) if reason.contains("later"))
        );
        assert!(
            matches!(verdict(&tmux, "unbind '\"'"), Verdict::Write { value, .. } if value.as_str() == Some("none")),
            "{replacement}"
        );
    }
}

#[test]
fn included_overrides_are_settled_before_beginner_policy() {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(
        home.path().join(".tmux.conf"),
        "bind '\"' new-window\nsource-file keys.conf\nbind - split-window -v\n",
    )
    .expect("config");
    std::fs::write(home.path().join("keys.conf"), "unbind '\"'\n").expect("include");
    let sources = discover(home.path(), None, &[], None, &Setup::default());
    let tmux = find(&sources, MigrateSource::Tmux);
    assert!(
        matches!(verdict(tmux, "bind '\"' new-window"), Verdict::Covered(reason) if reason.contains("keys.conf"))
    );
    assert!(matches!(verdict(tmux, "unbind '\"'"), Verdict::Covered(_)));
}

#[test]
fn ghostty_overrides_still_use_the_shared_last_line_order() {
    let home = tempfile::tempdir().expect("home");
    let config = home.path().join(".config/ghostty");
    std::fs::create_dir_all(&config).expect("directory");
    std::fs::write(
        config.join("config"),
        "keybind = ctrl+h=goto_split:left\nkeybind = ctrl+h=goto_split:right\n",
    )
    .expect("config");
    let sources = discover(home.path(), None, &[], None, &Setup::default());
    let ghostty = find(&sources, MigrateSource::Ghostty);
    assert!(matches!(
        verdict(ghostty, "keybind = ctrl+h=goto_split:left"),
        Verdict::Covered(_)
    ));
    assert!(
        matches!(verdict(ghostty, "keybind = ctrl+h=goto_split:right"), Verdict::Write { value, .. } if value["direction"].as_str() == Some("right"))
    );
}

#[test]
fn functional_display_tails_are_not_discarded_during_migration() {
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
        let line = format!("bind r source-file ~/.tmux.conf \\; {tail}");
        let tmux = source(&line, false);
        assert!(matches!(verdict(&tmux, &line), Verdict::Skip(_)), "{tail}");
    }
}

#[test]
fn clipboard_denial_is_written_and_allowance_remains_the_default() {
    let tmux = source("set -g set-clipboard off\n", false);
    assert!(
        matches!(verdict(&tmux, "set -g set-clipboard off"), Verdict::Write { table: Table::Defaults, key, value, .. } if key == "clipboard-write" && value.as_str() == Some("deny"))
    );
    for value in ["on", "external"] {
        let line = format!("set -g set-clipboard {value}");
        let tmux = source(&line, false);
        assert!(
            matches!(verdict(&tmux, &line), Verdict::Default { table: Table::Defaults, key } if key == "clipboard-write")
        );
    }
}

#[test]
fn only_the_fixed_escape_wait_is_equivalent() {
    for value in ["10", "010"] {
        let line = format!("set -sg escape-time {value}");
        let tmux = source(&line, false);
        assert!(matches!(verdict(&tmux, &line), Verdict::Covered(_)));
    }
    for value in ["0", "500", "-1", "invalid"] {
        let line = format!("set -sg escape-time {value}");
        let tmux = source(&line, false);
        assert!(
            matches!(verdict(&tmux, &line), Verdict::Skip(reason) if reason.contains("fixed 10 ms") && reason.contains(value)),
            "{value}"
        );
    }
}
