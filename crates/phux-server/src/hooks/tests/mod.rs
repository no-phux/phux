use std::time::Instant;

use phux_config::{Config, HookEntry};

use super::catalog::offending_config_hooks;
use super::*;

mod catalog;
mod dispatch;

/// A catalog of just the config hooks in `hooks_toml`.
fn catalog_from_toml(hooks_toml: &str) -> HookCatalog {
    let cfg: Config = toml::from_str(hooks_toml).expect("valid config");
    HookCatalog {
        config_hooks: cfg.hooks,
        plugin_events: Vec::new(),
    }
}

fn ctx(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn when(toml_inline: &str) -> BTreeMap<String, toml::Value> {
    toml::from_str(toml_inline).expect("valid when table")
}

#[test]
fn when_clauses_match_context() {
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);
    let cases: &[Case<'_>] = &[
        ("", &[], true),
        ("", &[("exit-code", "1")], true),
        ("exit-code = 0", &[("exit-code", "0")], true),
        ("exit-code = 0", &[("exit-code", "1")], false),
        ("exit-code = 0", &[], false),
        ("session = \"work\"", &[("session", "work")], true),
        ("session = \"work\"", &[("session", "home")], false),
        // `"*"` fires even without the key (a signal-killed child).
        ("exit-code = \"*\"", &[("exit-code", "137")], true),
        ("exit-code = \"*\"", &[], true),
        (
            "cwd-startswith = \"/Users/x/work\"",
            &[("cwd", "/Users/x/work/repo")],
            true,
        ),
        (
            "cwd-startswith = \"/Users/x/work\"",
            &[("cwd", "/tmp")],
            false,
        ),
        ("cwd-startswith = \"/Users/x/work\"", &[], false),
        (
            "exit-code = 0\nsession = \"work\"",
            &[("exit-code", "0"), ("session", "work")],
            true,
        ),
        (
            "exit-code = 0\nsession = \"work\"",
            &[("exit-code", "0"), ("session", "home")],
            false,
        ),
    ];
    for (clauses, context, want) in cases {
        assert_eq!(
            when_matches(&when(clauses), &ctx(context)),
            *want,
            "{clauses:?} vs {context:?}"
        );
    }
}

fn action(toml_inline: &str) -> Action {
    #[derive(serde::Deserialize)]
    struct Holder {
        action: Action,
    }
    let holder: Holder = toml::from_str(toml_inline).expect("valid action");
    holder.action
}

#[test]
fn action_argv_shapes() {
    // Bare `noop` (and any bare string) is not server-executable.
    assert_eq!(action_argv(&action("action = \"noop\"")), None);
    // `run` with a string command goes through /bin/sh -c.
    assert_eq!(
        action_argv(&action(
            "action = { kind = \"run\", command = \"echo hi\" }"
        )),
        Some(vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "echo hi".to_owned()
        ])
    );
    // `run` with an argv array executes directly.
    assert_eq!(
        action_argv(&action(
            "action = { kind = \"run\", command = [\"say\", \"done\"] }"
        )),
        Some(vec!["say".to_owned(), "done".to_owned()])
    );
    // Client-side kinds are skipped.
    assert_eq!(
        action_argv(&action(
            "action = { kind = \"message\", text = \"in work tree\" }"
        )),
        None
    );
    // A run action with a malformed command is skipped.
    assert_eq!(
        action_argv(&action("action = { kind = \"run\", command = 3 }")),
        None
    );
    assert_eq!(
        action_argv(&action("action = { kind = \"run\", command = [] }")),
        None
    );
}

/// `vocab::hook_action_is_executable` agrees with [`action_argv`] on every
/// shape.
#[test]
fn executability_predicate_agrees_with_action_argv() {
    let cases = [
        "action = \"noop\"",
        "action = \"kill-pane\"",
        "action = { kind = \"noop\" }",
        "action = { kind = \"message\", text = \"hi\" }",
        "action = { kind = \"run\", command = \"echo hi\" }",
        "action = { kind = \"run\", command = \"   \" }",
        "action = { kind = \"run\", command = [\"say\", \"done\"] }",
        "action = { kind = \"run\", command = [] }",
        "action = { kind = \"run\", command = [\"say\", 3] }",
        "action = { kind = \"run\", command = 3 }",
        "action = { kind = \"run\" }",
    ];
    for case in cases {
        let action = action(case);
        assert_eq!(
            vocab::hook_action_is_executable(&action),
            action_argv(&action).is_some(),
            "predicate disagrees with action_argv for: {case}",
        );
    }
}

/// `vocab::hook_context_keys` matches the constructors exactly.
#[test]
fn vocab_context_keys_match_the_event_constructors() {
    let terminal = phux_protocol::ids::ResourceId::local(7);
    let client = crate::state::ClientId(3);
    let events = [
        HookEvent::after_new_pane(&terminal, Some("work")),
        HookEvent::pane_exit(&terminal, Some(0)),
        HookEvent::focus_changed(&terminal, client),
        HookEvent::client_attached(client, "work"),
        HookEvent::client_detached(client, Some("work")),
        HookEvent::agent_state_changed(&terminal, "claude", "reviewer", Some("busy"), "idle"),
        HookEvent::worktree_created(
            Path::new("/w/phux-feat"),
            "feat",
            Path::new("/w/phux"),
            "phux-feat",
        ),
    ];
    assert_eq!(
        events.len(),
        vocab::HOOK_EVENTS.len(),
        "an event is missing from this agreement test",
    );
    for event in events {
        let expected = vocab::hook_context_keys(&event.name)
            .unwrap_or_else(|| panic!("constructor built unknown event `{}`", event.name));
        let got: Vec<&str> = event.context.keys().map(String::as_str).collect();
        assert_eq!(got, expected, "context keys drifted for `{}`", event.name);
    }
}

/// Each offending entry yields one problem record; a clean config none.
#[test]
fn offending_config_hooks_flags_each_bad_entry_once() {
    let cfg: Config = toml::from_str(
        r#"
        [[hooks.pane-exited]]
        action = "noop"

        [[hooks.pane-exit]]
        when = { exit-code = 0 }
        action = "noop"

        [[hooks.pane-exit]]
        when = { exitcode = "*" }
        action = { kind = "message", text = "bye" }

        [[hooks.after-new-pane]]
        when = { session-startswith = "work" }
        action = { kind = "run", command = "true" }
        "#,
    )
    .expect("valid config");
    let problems = offending_config_hooks(&cfg.hooks);
    let labels: Vec<&str> = problems.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["hooks.pane-exit[1]", "hooks.pane-exited"],
        "problems: {problems:?}",
    );
    let entry_problem = &problems[0].detail;
    assert!(
        entry_problem.contains("when key `exitcode` can never match")
            && entry_problem.contains("action `message` never executes server-side"),
        "entry problems not aggregated: {entry_problem}",
    );
    assert!(
        problems[1].detail.contains("did you mean `pane-exit`?"),
        "no suggestion in: {}",
        problems[1].detail,
    );
}

#[test]
fn env_var_naming_uppercases_and_underscores() {
    assert_eq!(context_env_var("exit-code"), "PHUX_EXIT_CODE");
    assert_eq!(context_env_var("terminal-id"), "PHUX_TERMINAL_ID");
    assert_eq!(context_env_var("session"), "PHUX_SESSION");
}

/// The spec table and the constructors describe the same events and
/// keys, with prose for each.
#[test]
fn spec_table_roundtrips_through_the_constructors() {
    let specs = hook_event_specs();
    assert_eq!(
        specs.iter().map(|spec| spec.name).collect::<Vec<_>>(),
        vocab::HOOK_EVENTS,
        "one spec per vocab event, in vocab order",
    );
    for spec in &specs {
        assert!(!spec.doc.is_empty(), "no doc prose for `{}`", spec.name);
        assert_eq!(
            Some(spec.context_keys),
            vocab::hook_context_keys(spec.name),
            "spec keys for `{}` must come from the vocab",
            spec.name,
        );
    }

    let terminal = phux_protocol::ids::ResourceId::local(7);
    let client = crate::state::ClientId(3);
    let events = [
        HookEvent::after_new_pane(&terminal, Some("work")),
        HookEvent::pane_exit(&terminal, Some(0)),
        HookEvent::focus_changed(&terminal, client),
        HookEvent::client_attached(client, "work"),
        HookEvent::client_detached(client, Some("work")),
        HookEvent::agent_state_changed(&terminal, "claude", "reviewer", Some("busy"), "idle"),
        HookEvent::worktree_created(
            Path::new("/w/phux-feat"),
            "feat",
            Path::new("/w/phux"),
            "phux-feat",
        ),
    ];
    assert_eq!(
        events.len(),
        specs.len(),
        "a constructor is missing from this roundtrip",
    );
    for event in events {
        let spec = specs
            .iter()
            .find(|spec| spec.name == event.name)
            .unwrap_or_else(|| panic!("constructor built unspecced event `{}`", event.name));
        let got: Vec<&str> = event.context.keys().map(String::as_str).collect();
        assert_eq!(
            got, spec.context_keys,
            "context keys drifted for `{}`",
            event.name,
        );
    }
}
