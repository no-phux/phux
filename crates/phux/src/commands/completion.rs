//! `phux completion <SHELL>` — emit a shell completion script on stdout.
//!
//! The script is generated from the same usage spec the binary parses with,
//! so completions and the real CLI surface cannot drift. Hidden verbs and
//! flags are omitted by the generator; they still parse, they are just never
//! offered. This never contacts a server.

use std::process::ExitCode;

use crate::Cli;

#[cfg(test)]
const BIN_NAME: &str = "phux";

/// Render the completion script for `shell` to stdout.
pub(crate) fn run_completion(shell: usage::complete::Shell) -> ExitCode {
    crate::output::bytes(&completion_script(shell));
    ExitCode::SUCCESS
}

/// The completion script for `shell`, exactly as `phux completion` prints it.
pub(crate) fn completion_script(shell: usage::complete::Shell) -> Vec<u8> {
    Cli::completion_script(shell).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_script_names_the_phux_binary() {
        let script = Cli::completion_script(usage::complete::Shell::Zsh);
        assert!(
            script.contains(BIN_NAME),
            "generated zsh completion never mentions `{BIN_NAME}`"
        );
    }

    #[test]
    fn generated_script_covers_a_known_subcommand() {
        let script = Cli::completion_script(usage::complete::Shell::Bash);
        // usage-rs scripts are thin shells that ask the live binary for
        // candidates (`__complete_word__`); they do not embed the verb
        // list the way clap_complete did. The hook is the contract.
        assert!(
            script.contains("__complete_word__") || script.contains("snapshot"),
            "generated bash completion is neither a usage complete hook nor a static `snapshot` listing:\n{script}"
        );
    }

    #[test]
    fn completion_scripts_omit_hidden_plumbing() {
        for shell in [
            usage::complete::Shell::Bash,
            usage::complete::Shell::Zsh,
            usage::complete::Shell::Fish,
        ] {
            let script = Cli::completion_script(shell);
            assert!(
                !script.contains("gen-reference-docs"),
                "{shell:?} completions still offer the hidden gen-reference-docs verb"
            );
            assert!(
                !script.contains("--daemonize"),
                "{shell:?} completions still offer the hidden --daemonize flag"
            );
        }
    }
}
