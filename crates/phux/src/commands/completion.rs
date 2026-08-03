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
/// its hidden top-level verbs.
///
/// `clap_complete`'s shell generators (4.6.7) copy every subcommand into
/// the emitted script -- `is_hide_set` is only consulted for possible
/// values -- so generating straight from [`Cli::command`] offers the
/// hidden ADR-0066 deprecation aliases (`remote`, `satellite`, `enroll`)
/// and the machine-only `gen-reference-docs` right next to `host`
/// (phux-i0e8.13.2). Since `clap::Command` has no subcommand-removal API,
/// this rebuilds the root -- same name, version, and root args -- and
/// re-adds only the visible verbs. The hidden verbs still parse; they are
/// just never offered.
///
/// Root-level pruning only: no nested subcommand is hidden today, and the
/// tests in `main.rs` pin the generated scripts, so a future nested hidden
/// verb that leaks will fail there rather than ship silently.
fn completion_tree() -> clap::Command {
    let root = Cli::command();
    // The version the derive stamped on the real root (`--version` still
    // completes); `env!` because `clap::builder::Str` wants `'static`.
    let mut pruned = clap::Command::new(BIN_NAME).version(env!("CARGO_PKG_VERSION"));
    if let Some(about) = root.get_about() {
        pruned = pruned.about(about.clone());
    }
    for arg in root.get_arguments() {
        pruned = pruned.arg(arg.clone());
    }
    pruned.subcommands(
        root.get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .cloned(),
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
