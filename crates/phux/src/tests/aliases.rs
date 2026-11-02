//! Alias parity, completions, and the deprecation table.

use super::*;

/// Parse `argv` to its resolved [`Command`], panicking with the argv on
/// any failure — the shared front door for the alias-parity tests.
fn parsed(argv: &[&str]) -> Command {
    crate::parse_cli(argv)
        .unwrap_or_else(|err| panic!("{argv:?} must parse: {err}"))
        .command
        .unwrap_or_else(|| panic!("{argv:?} names a verb"))
}

/// Every list-shaped registry verb answers to both `list` and `ls`, parsing
/// to the canonical variant.
#[test]
fn list_aliases_map_to_the_canonical_variants() {
    use crate::commands::{PluginAction, TagAction};

    for argv in [["phux", "worktree", "list"], ["phux", "worktree", "ls"]] {
        assert!(matches!(
            parsed(&argv),
            Command::Worktree(crate::commands::WorktreeArgs {
                action: crate::commands::WorktreeAction::List { .. },
            })
        ));
    }
    // tag's canonical name is the short one; `list` is the alias.
    for argv in [["phux", "tag", "ls", "."], ["phux", "tag", "list", "."]] {
        assert!(matches!(
            parsed(&argv),
            Command::Tag(crate::commands::TagArgs {
                action: TagAction::Ls { .. }
            })
        ));
    }
    for argv in [["phux", "plugin", "list"], ["phux", "plugin", "ls"]] {
        assert!(matches!(
            parsed(&argv),
            Command::Plugin(crate::commands::PluginArgs {
                action: PluginAction::List { .. }
            })
        ));
    }
    // The root registry verb keeps its established pair.
    for argv in [["phux", "ls"], ["phux", "list"]] {
        assert!(matches!(parsed(&argv), Command::Ls(..)));
    }
}

/// `phux theme` parses every verb; `set` takes a name or `--file`, and
/// `remove` answers to `rm` (ADR-0065 alias policy).
#[test]
fn theme_verbs_parse() {
    use crate::commands::ThemeAction;

    for argv in [["phux", "theme", "list"], ["phux", "theme", "ls"]] {
        assert!(matches!(
            parsed(&argv),
            Command::Theme(crate::commands::ThemeArgs {
                action: ThemeAction::List { json: false }
            })
        ));
    }
    assert!(matches!(
        parsed(&["phux", "theme", "show"]),
        Command::Theme(crate::commands::ThemeArgs {
            action: ThemeAction::Show {
                name: None,
                json: false
            }
        })
    ));
    assert!(matches!(
        parsed(&["phux", "theme", "set", "nord"]),
        Command::Theme(crate::commands::ThemeArgs {
            action: ThemeAction::Set {
                name: Some(_),
                file: None
            }
        })
    ));
    assert!(matches!(
        parsed(&["phux", "theme", "set", "--file", "~/x/colors.toml"]),
        Command::Theme(crate::commands::ThemeArgs {
            action: ThemeAction::Set {
                name: None,
                file: Some(_)
            }
        })
    ));
    assert!(matches!(
        parsed(&[
            "phux",
            "theme",
            "install",
            "https://example.com/x.git",
            "--json"
        ]),
        Command::Theme(crate::commands::ThemeArgs {
            action: ThemeAction::Install { json: true, .. }
        })
    ));
    for argv in [
        ["phux", "theme", "remove", "nord"],
        ["phux", "theme", "rm", "nord"],
    ] {
        assert!(matches!(
            parsed(&argv),
            Command::Theme(crate::commands::ThemeArgs {
                action: ThemeAction::Remove { .. }
            })
        ));
    }
}

/// `phux plugin log` parses its flags, refuses a non-numeric limit, and
/// its help names the bounded run log it reads.
#[test]
fn plugin_log_parses_flags_and_documents_the_run_log() {
    use crate::commands::PluginAction;

    assert!(matches!(
        parsed(&["phux", "plugin", "log"]),
        Command::Plugin(crate::commands::PluginArgs {
            action: PluginAction::Log {
                limit: None,
                failed: false,
                json: false,
                startup: None,
            }
        })
    ));
    for argv in [
        &["phux", "plugin", "log", "-n", "5", "--failed", "--json"][..],
        &[
            "phux", "plugin", "log", "--limit", "5", "--failed", "--json",
        ][..],
    ] {
        assert!(matches!(
            parsed(argv),
            Command::Plugin(crate::commands::PluginArgs {
                action: PluginAction::Log {
                    limit: Some(5),
                    failed: true,
                    json: true,
                    startup: None,
                }
            })
        ));
    }
    assert!(crate::parse_cli(["phux", "plugin", "log", "-n", "many"]).is_err());

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

/// Alias parity, remove half (phux-i0e8.8.3): every remove-shaped
/// registry verb answers to both spellings — including `plugin unlink`,
/// whose canonical name predates the policy and now also answers to
/// `rm` / `remove`.
#[test]
fn remove_aliases_map_to_the_canonical_variants() {
    use crate::commands::{PluginAction, TagAction};

    for argv in [
        ["phux", "worktree", "remove", "feat"],
        ["phux", "worktree", "rm", "feat"],
    ] {
        assert!(matches!(
            parsed(&argv),
            Command::Worktree(crate::commands::WorktreeArgs {
                action: crate::commands::WorktreeAction::Remove { .. },
            })
        ));
    }
    // tag's canonical name is the short one; `remove` is the alias.
    for argv in [
        ["phux", "tag", "rm", ".", "build"],
        ["phux", "tag", "remove", ".", "build"],
    ] {
        assert!(matches!(
            parsed(&argv),
            Command::Tag(crate::commands::TagArgs {
                action: TagAction::Rm { .. }
            })
        ));
    }
    for argv in [
        ["phux", "plugin", "unlink", "x.y"],
        ["phux", "plugin", "rm", "x.y"],
        ["phux", "plugin", "remove", "x.y"],
    ] {
        assert!(matches!(
            parsed(&argv),
            Command::Plugin(crate::commands::PluginArgs {
                action: PluginAction::Unlink { .. }
            })
        ));
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

/// Every hidden subcommand or long flag outside the internal allowlist
/// must be a `deprecations::DEPRECATED` row, and every row must still be
/// hidden in the parser, so neither side can drift.
#[test]
fn deprecation_table_matches_the_clap_tree_bidirectionally() {
    use std::collections::BTreeSet;

    use crate::deprecations::DEPRECATED;

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
    ];

    /// Collect every hidden row of the tree under `path`: hidden long
    /// flags as `<path> --<flag>`, and each hidden subcommand as one
    /// row, its actions included (matching the table's `old` spellings).
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
                // A hidden verb is deprecated whole, its actions with it.
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

    let table_rows: BTreeSet<String> = DEPRECATED.iter().map(|row| row.old.to_owned()).collect();
    assert_eq!(
        table_rows.len(),
        DEPRECATED.len(),
        "duplicate `old` spellings in the deprecation table"
    );
    assert_eq!(
        tree_rows, table_rows,
        "hidden clap surface and the deprecation table must agree: a \
             row only in the tree is an unregistered hidden alias (add it \
             to deprecations::DEPRECATED); a row only in the table is \
             stale (the alias is gone — delete the row and regenerate \
             docs/reference/deprecations.md)"
    );
}

/// Every verb row's stderr line is the alias table's exact rendering of
/// its own `old`/`new` columns, and every flag row's line names the
/// deprecated flag and its `--split` replacement — so the generated
/// deprecations page, the warning, and the audit test all say the same
/// thing.
#[test]
fn deprecation_rows_render_their_own_notes() {
    use crate::deprecations::{DEPRECATED, DeprecatedSurface};

    for row in DEPRECATED {
        match row.surface {
            DeprecatedSurface::Verb => assert_eq!(
                row.note,
                format!(
                    "phux: warn: `{}` is deprecated and will be removed; use `{}`",
                    row.old, row.new
                ),
                "verb row {} must render its note from its own columns",
                row.old
            ),
            DeprecatedSurface::Flag => {
                let flag = row.old_flag().expect("flag rows end in a long flag");
                let axis = flag.trim_start_matches("--");
                assert_eq!(
                    row.note,
                    format!(
                        "phux: warn: {flag} is deprecated and will be removed; \
                             use `--split {axis}` (or `--split {short}`)",
                        short = &axis[..1]
                    ),
                    "flag row {} must warn toward its --split replacement",
                    row.old
                );
                assert!(
                    row.new.ends_with(&format!("--split {axis}")),
                    "flag row {} must advertise the --split spelling",
                    row.old
                );
            }
        }
    }
}
