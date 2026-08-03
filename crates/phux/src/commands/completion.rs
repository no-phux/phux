//! `phux completion <SHELL>` — emit a shell completion script on stdout.
//!
//! The script is generated from the same `clap` command tree the binary
//! parses its arguments with, so the completions and the real CLI surface
//! cannot drift: adding a subcommand adds its completion in the same build.
//!
//! This never contacts a server. It is a pure function of the compiled
//! binary, which is what makes it safe to run from a shell rc file.

use std::process::ExitCode;

use clap::CommandFactory;
use clap_complete::Shell;

use crate::Cli;

/// The binary name the generated script completes for.
///
/// Hard-coded rather than read from `argv[0]`: the script is usually piped
/// through a file and sourced later, so it must name the installed command,
/// not whatever path happened to invoke the generator.
const BIN_NAME: &str = "phux";

/// The command tree completions are generated from: the real parser minus
/// every hidden verb and every hidden arg, at every depth.
///
/// `clap_complete`'s shell generators (4.6.7) copy every subcommand AND
/// every arg into the emitted script -- `is_hide_set` is only consulted
/// for possible values -- so generating straight from [`Cli::command`]
/// offers the hidden ADR-0066 deprecation aliases (`remote`, `satellite`,
/// `enroll`), the machine-only `gen-reference-docs` (phux-i0e8.13.2), the
/// deprecated split booleans (`--horizontal`/`--vertical`,
/// phux-i0e8.13.4, phux-c1ry), and the machine plumbing flags (`server
/// --daemonize`, `play --pty-writer`, ...) right next to the visible
/// surface. Since `clap::Command` has no removal API for either
/// subcommands or args, [`prune`] rebuilds each command -- name, about,
/// visible aliases, groups, visible args -- and recurses into only the
/// visible verbs. The hidden spellings still parse; they are just never
/// offered.
fn completion_tree() -> clap::Command {
    // The version the derive stamped on the real root (`--version` still
    // completes); `env!` because `clap::builder::Str` wants `'static`.
    prune(&Cli::command()).version(env!("CARGO_PKG_VERSION"))
}

/// A copy of `cmd` carrying only what the shell generators render --
/// name, about, visible aliases, arg groups -- with hidden args dropped
/// and hidden subcommands not descended into.
fn prune(cmd: &clap::Command) -> clap::Command {
    // Without clap's `string` feature, builder names want `&'static str`,
    // but the copied names are borrowed from the source tree. Leaking is
    // fine here: the set of names is a small constant (the command tree),
    // and the verb is a one-shot that exits after printing the script.
    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_owned().into_boxed_str())
    }
    let mut out = clap::Command::new(leak(cmd.get_name()));
    if let Some(about) = cmd.get_about() {
        out = out.about(about.clone());
    }
    for alias in cmd.get_visible_aliases() {
        out = out.visible_alias(leak(alias));
    }
    for arg in cmd.get_arguments().filter(|arg| !arg.is_hide_set()) {
        out = out.arg(arg.clone());
    }
    // Arg groups must be copied — visible args may reference a group by id
    // (e.g. `requires = "remote"` on `attach --token`), and clap's debug
    // asserts fail the build on a dangling reference. But the derive's
    // implicit per-verb group lists every arg including the hidden ones,
    // so each group is rebuilt with only its surviving members. Group-level
    // `requires`/`conflicts` have no getters and are dropped; this tree is
    // only ever rendered, never parsed with, so only ids matter.
    let visible: Vec<&clap::Id> = cmd
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .map(clap::Arg::get_id)
        .collect();
    for group in cmd.get_groups() {
        let rebuilt = clap::ArgGroup::new(group.get_id().clone())
            .args(group.get_args().filter(|id| visible.contains(id)).cloned())
            .multiple(group.clone().is_multiple())
            .required(group.is_required_set());
        out = out.group(rebuilt);
    }
    out.subcommands(
        cmd.get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(prune),
    )
}

/// Render the completion script for `shell` to stdout.
///
/// Always exits 0 on a successful write. A broken pipe (the common case
/// when a user pipes into `head`) is not an error worth reporting, but any
/// other write failure is: a truncated completion script sourced by a shell
/// is worse than none, so the caller should see it fail.
///
/// That contract used to be prose only, and prose lost: `clap_complete`
/// writes into the sink with `.expect("failed to write completion file")`,
/// so handing it `io::stdout()` directly turned `phux completion bash |
/// head` into a panic — and the script is ~160 KB, well past a pipe buffer,
/// so the write is guaranteed to reach a reader that has already gone
/// (phux-h5hj.8). Rendering into a buffer first puts the write back under
/// `output::bytes`, which owns the decision. The extra allocation is one
/// script, once, in a verb that runs at shell-rc time.
pub(crate) fn run_completion(shell: Shell) -> ExitCode {
    crate::output::bytes(&completion_script(shell));
    ExitCode::SUCCESS
}

/// The completion script for `shell`, exactly as `phux completion` prints
/// it. Shared with the leak tests in `main.rs` so they pin the shipped
/// script, not a lookalike tree.
pub(crate) fn completion_script(shell: Shell) -> Vec<u8> {
    let mut cmd = completion_tree();
    let mut script: Vec<u8> = Vec::new();
    clap_complete::generate(shell, &mut cmd, BIN_NAME, &mut script);
    script
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;

    use super::*;

    /// Every shell `clap_complete` knows about must actually render. This is
    /// the guard that a future clap upgrade adding a shell does not ship a
    /// variant that panics the first time a user asks for it.
    #[test]
    fn every_shell_variant_renders_a_nonempty_script() {
        for shell in Shell::value_variants() {
            assert!(
                !completion_script(*shell).is_empty(),
                "`phux completion {shell}` rendered an empty script"
            );
        }
    }

    /// The generated script has to name the installed binary, not the test
    /// harness that produced it.
    #[test]
    fn generated_script_names_the_phux_binary() {
        let script =
            String::from_utf8(completion_script(Shell::Zsh)).expect("zsh completion is utf-8");
        assert!(
            script.contains(BIN_NAME),
            "generated zsh completion never mentions `{BIN_NAME}`"
        );
    }

    /// A subcommand that exists in the parser must be completable. `agent`
    /// is the canary because it is the surface agents drive phux through.
    #[test]
    fn generated_script_covers_a_known_subcommand() {
        let script =
            String::from_utf8(completion_script(Shell::Bash)).expect("bash completion is utf-8");
        assert!(
            script.contains("snapshot"),
            "generated bash completion omits the `snapshot` verb"
        );
    }

    /// The pruned tree drops exactly the hidden top-level verbs and keeps
    /// everything else, so completions can never offer a verb `--help`
    /// hides. `Cli::command()` is the source of truth for both sides.
    #[test]
    fn completion_tree_is_the_visible_half_of_the_parser() {
        let real: Vec<(String, bool)> = Cli::command()
            .get_subcommands()
            .map(|sub| (sub.get_name().to_owned(), sub.is_hide_set()))
            .collect();
        let offered: Vec<String> = completion_tree()
            .get_subcommands()
            .map(|sub| sub.get_name().to_owned())
            .collect();
        for (name, hidden) in real {
            assert_eq!(
                !hidden,
                offered.contains(&name),
                "`{name}` (hidden: {hidden}) is on the wrong side of the completion tree"
            );
        }
    }

    /// `clap_complete` 4.6.7 copies args into the script without
    /// consulting `is_hide_set`, so the pruned tree must drop hidden args
    /// before generation -- the machine plumbing (`--daemonize`,
    /// `--pty-writer`, ...) and the deprecated split booleans alike. Walk
    /// the real parser for every hidden long at any depth and pin its
    /// absence from the emitted scripts.
    #[test]
    fn completion_scripts_omit_every_hidden_arg() {
        fn hidden_longs(cmd: &clap::Command, acc: &mut Vec<String>) {
            for arg in cmd.get_arguments().filter(|arg| arg.is_hide_set()) {
                if let Some(long) = arg.get_long() {
                    acc.push(format!("--{long}"));
                }
            }
            for sub in cmd.get_subcommands() {
                hidden_longs(sub, acc);
            }
        }
        let mut hidden = Vec::new();
        hidden_longs(&Cli::command(), &mut hidden);
        assert!(
            !hidden.is_empty(),
            "the parser has hidden args today; if that ever stops being \
             true this test is vacuous and should be retired"
        );
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script =
                String::from_utf8(completion_script(shell)).expect("completion script is UTF-8");
            for flag in &hidden {
                assert!(
                    !script.contains(flag.as_str()),
                    "{shell} completions still offer the hidden arg {flag}"
                );
            }
        }
    }

    /// Recursive walk of the real parser against the completion tree:
    /// every command path offers exactly the visible args and exactly the
    /// visible subcommands of its real counterpart (phux-c1ry). This is
    /// the structural guard behind the script-level pins in `main.rs` --
    /// it fails naming the exact path and arg when a future hidden flag
    /// (or nested hidden verb) leaks into the offered tree.
    fn assert_visible_surface(real: &clap::Command, offered: &clap::Command, path: &str) {
        let offered_args: Vec<&str> = offered
            .get_arguments()
            .map(|arg| arg.get_id().as_str())
            .collect();
        for arg in real.get_arguments() {
            assert_eq!(
                !arg.is_hide_set(),
                offered_args.contains(&arg.get_id().as_str()),
                "arg `{}` of `{path}` (hidden: {}) is on the wrong side of the completion tree",
                arg.get_id(),
                arg.is_hide_set()
            );
        }
        for sub in real.get_subcommands() {
            let name = sub.get_name();
            let offered_sub = offered
                .get_subcommands()
                .find(|candidate| candidate.get_name() == name);
            if sub.is_hide_set() {
                assert!(
                    offered_sub.is_none(),
                    "hidden subcommand `{path} {name}` leaked into the completion tree"
                );
            } else {
                let offered_sub = offered_sub.unwrap_or_else(|| {
                    panic!("visible subcommand `{path} {name}` missing from the completion tree")
                });
                assert_visible_surface(sub, offered_sub, &format!("{path} {name}"));
            }
        }
    }

    #[test]
    fn completion_tree_offers_exactly_the_visible_surface_recursively() {
        assert_visible_surface(&Cli::command(), &completion_tree(), BIN_NAME);
    }

    /// The four shells the docs tell users to install for must all be
    /// offered by the parser, so a clap upgrade that drops one fails here
    /// rather than in a user's rc file.
    #[test]
    fn parser_offers_the_documented_shells() {
        let offered: Vec<String> = Shell::value_variants()
            .iter()
            .filter_map(|shell| shell.to_possible_value().map(|v| v.get_name().to_owned()))
            .collect();
        for shell in ["bash", "zsh", "fish", "powershell"] {
            assert!(
                offered.iter().any(|name| name == shell),
                "`{shell}` missing from the offered shell list: {offered:?}"
            );
        }
    }
}
