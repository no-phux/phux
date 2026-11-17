//! Alias parity, completions, and the hidden-surface guard.

use super::*;

/// Parse `line` to its resolved [`Command`], panicking on any failure.
fn parsed(line: &str) -> Command {
    parse(line)
        .unwrap_or_else(|err| panic!("{line} must parse: {err}"))
        .command
        .unwrap_or_else(|| panic!("{line} names a verb"))
}

/// Alias parity (ADR-0065, phux-i0e8.8.3): every list- and remove-shaped
/// registry verb answers to both spellings, parsing to the canonical
/// variant. `tag`'s canonical names are the short ones, and `plugin unlink`
/// predates the policy but also answers to `rm` / `remove`.
#[test]
fn aliases_parse_to_the_canonical_variants() {
    for (alias, canonical) in [
        ("phux worktree ls", "phux worktree list"),
        ("phux tag list .", "phux tag ls ."),
        ("phux plugin ls", "phux plugin list"),
        ("phux list", "phux ls"),
        ("phux theme ls", "phux theme list"),
        ("phux worktree rm feat", "phux worktree remove feat"),
        ("phux tag remove . build", "phux tag rm . build"),
        ("phux plugin rm x.y", "phux plugin unlink x.y"),
        ("phux plugin remove x.y", "phux plugin unlink x.y"),
        ("phux theme rm nord", "phux theme remove nord"),
        ("phux plugin log --limit 5", "phux plugin log -n 5"),
    ] {
        assert_eq!(
            format!("{:?}", parsed(alias)),
            format!("{:?}", parsed(canonical)),
            "{alias}"
        );
    }
    assert!(matches!(
        parsed("phux plugin unlink x.y"),
        Command::Plugin(crate::commands::PluginArgs {
            action: crate::commands::PluginAction::Unlink { .. }
        })
    ));
}

/// `phux theme` parses every verb; `set` takes a name or `--file`.
#[test]
fn theme_verbs_parse() {
    use crate::commands::ThemeAction;
    let action = |line| match parsed(line) {
        Command::Theme(crate::commands::ThemeArgs { action }) => action,
        other => panic!("{line}: {other:?}"),
    };
    assert!(matches!(
        action("phux theme list"),
        ThemeAction::List { json: false }
    ));
    assert!(matches!(
        action("phux theme show"),
        ThemeAction::Show {
            name: None,
            json: false
        }
    ));
    assert!(matches!(
        action("phux theme set nord"),
        ThemeAction::Set {
            name: Some(_),
            file: None
        }
    ));
    assert!(matches!(
        action("phux theme set --file ~/x/colors.toml"),
        ThemeAction::Set {
            name: None,
            file: Some(_)
        }
    ));
    assert!(matches!(
        action("phux theme install https://example.com/x.git --json"),
        ThemeAction::Install { json: true, .. }
    ));
    assert!(matches!(
        action("phux theme remove nord"),
        ThemeAction::Remove { .. }
    ));
}

/// `phux plugin log` parses its flags, refuses a non-numeric limit, and
/// its help names the bounded run log it reads.
#[test]
fn plugin_log_parses_flags_and_documents_the_run_log() {
    use crate::commands::PluginAction;

    let log = |line| match parsed(line) {
        Command::Plugin(crate::commands::PluginArgs {
            action:
                PluginAction::Log {
                    limit,
                    failed,
                    json,
                    startup: None,
                },
        }) => (limit, failed, json),
        other => panic!("{line}: {other:?}"),
    };
    assert_eq!(log("phux plugin log"), (None, false, false));
    assert_eq!(
        log("phux plugin log -n 5 --failed --json"),
        (Some(5), true, true)
    );
    assert_parses(&[("phux plugin log -n many", false)]);

    let plugin = Cli::command()
        .subcommands
        .iter()
        .find(|sub| sub.name == "plugin")
        .expect("plugin verb");
    let log = plugin
        .subcommands
        .iter()
        .find(|sub| sub.name == "log")
        .expect("plugin log verb");
    let page =
        crate::render_help_page(log, true, usage::help::Style::PLAIN).expect("plugin log help");
    for needle in [
        "plugin-runs.jsonl",
        "--failed",
        "--limit",
        "--json",
        "hook",
        "--startup",
    ] {
        assert!(page.contains(needle), "{needle} missing from:\n{page}");
    }
}

/// usage-rs completion scripts are thin shells that ask the live
/// binary (`__complete_word__`). Candidates come from that request,
/// not from names baked into the script.
fn complete_line(line: &str) -> String {
    Cli::completion_request(&[
        "__complete_word__".into(),
        "--shell".into(),
        "bash".into(),
        "--line".into(),
        line.into(),
    ])
    .unwrap_or_default()
}

/// The generated completions are built from the same usage spec, so the
/// single global `--socket` declaration must still reach them.
#[test]
fn completions_still_carry_socket() {
    let answer = complete_line("phux --");
    assert!(
        answer.contains("--socket"),
        "live completions lost --socket after the root-settings rework:\n{answer}"
    );
}

/// No hidden subcommand or long flag exists outside the internal
/// allowlist, and every allowlisted entry is still hidden. No CLI spelling
/// is deprecated today, so any other hidden surface is unregistered (see
/// "Deprecating a CLI spelling" in `docs/compatibility.md`).
#[test]
fn no_unregistered_hidden_cli_surface() {
    use std::collections::BTreeSet;

    /// Hidden surfaces that are machine plumbing, not deprecations:
    /// each is hidden because no human should type it, and none has a
    /// replacement spelling to migrate to.
    const INTERNAL: &[&str] = &[
        // The refdocs generator (ADR-0069): machine-only since it
        // shipped.
        "phux gen-reference-docs",
        // The SSH remoting shim (`ssh HOST phux stdio-bridge`): machine
        // -invoked by `attach --host`, hidden from humans (phux-06nn),
        // not deprecated.
        "phux stdio-bridge",
        // Far end of `phux attach --ssh` (ADR-0120): machine-invoked
        // over ssh, same reasoning as stdio-bridge.
        "phux bootstrap",
        // Auto-spawn / upgrade plumbing on `phux server`.
        "phux server --daemonize",
        "phux server --seed-command",
        "phux server --resume",
        // `phux new --empty` starts its server unseeded (ADR-0105).
        "phux server --no-seed",
        // `phux play`'s in-pane writer half.
        "phux play --pty-writer",
        // The Claude shim's stdin JSON reader: invoked only by the
        // generated wrapper, one line of shell-safe tokens out.
        "phux agent hook-payload",
        // Its transcript reader: one `phux.transcript/v1` data object out.
        "phux agent hook-transcript",
        // Server-bootstrapped durable ACP guardian, never a human verb.
        "phux agent host",
        // The Claude Code `SessionStart` hook `phux skill setup` installs.
        "phux skill session-start",
    ];

    /// Collect every hidden row of the tree under `path`: hidden long
    /// flags as `<path> --<flag>`, and each hidden subcommand as one
    /// row, its actions included.
    fn walk(meta: &usage::spec::CommandMeta<'_>, path: &str, rows: &mut BTreeSet<String>) {
        for flag in meta.flags {
            if flag.hide {
                for long in flag.flag.longs {
                    rows.insert(format!("{path} --{long}"));
                }
            }
        }
        for sub in meta.subcommands {
            let sub_path = format!("{path} {}", sub.cmd.name);
            if sub.hide {
                // A hidden verb is hidden whole, its actions with it.
                rows.insert(sub_path);
            } else {
                walk(sub, &sub_path, rows);
            }
        }
    }

    let mut tree_rows = BTreeSet::new();
    walk(Cli::spec().root, "phux", &mut tree_rows);
    for internal in INTERNAL {
        assert!(
            tree_rows.remove(*internal),
            "{internal} is allowlisted as internal but no longer hidden \
                 in the tree; prune the allowlist"
        );
    }

    assert!(
        tree_rows.is_empty(),
        "unregistered hidden clap surface: {tree_rows:?}. Allowlist new \
         internal plumbing in INTERNAL above, or, for a deprecated \
         spelling, restore the deprecation table, its reference page, and \
         its audit from `git show d4d856229^` (see docs/compatibility.md)"
    );
}
