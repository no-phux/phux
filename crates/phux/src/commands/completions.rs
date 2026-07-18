//! `phux completions <shell>` — emit a shell completion script to stdout.
//!
//! The script is rendered from the parsed clap command tree, so it can never
//! drift from the real command surface (the same tree the `help_inventory`
//! test pins). Install it per your shell, e.g.:
//!
//! ```text
//! phux completions bash > ~/.local/share/bash-completion/completions/phux
//! phux completions zsh  > "${fpath[1]}/_phux"
//! phux completions fish > ~/.config/fish/completions/phux.fish
//! ```

use std::io;
use std::process::ExitCode;

use clap_complete::Shell;

/// Write the completion script for `shell` to stdout. `cmd` is the fully-built
/// CLI command tree (`Cli::command()`), passed in so this stays decoupled from
/// the `Cli` type that lives in `main`.
pub(crate) fn run_completions(shell: Shell, cmd: clap::Command) -> ExitCode {
    write_completions(shell, cmd, &mut io::stdout());
    ExitCode::SUCCESS
}

/// Render the completion script for `shell` into `out`. Split from
/// [`run_completions`] so it can be exercised against an in-memory buffer.
fn write_completions(shell: Shell, mut cmd: clap::Command, out: &mut impl io::Write) {
    let bin = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, bin, out);
}

#[cfg(test)]
mod tests {
    use super::write_completions;
    use clap_complete::Shell;

    #[test]
    fn writes_a_nonempty_script_naming_the_binary_for_each_shell() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
            let cmd = clap::Command::new("phux")
                .subcommand(clap::Command::new("ls"))
                .subcommand(clap::Command::new("attach"));
            let mut buf = Vec::new();
            write_completions(shell, cmd, &mut buf);
            assert!(
                !buf.is_empty(),
                "{shell} produced an empty completion script"
            );
            let script = String::from_utf8(buf).expect("completion script is valid UTF-8");
            assert!(
                script.contains("phux"),
                "{shell} completion script does not name the binary"
            );
        }
    }
}
