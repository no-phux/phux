//! `phux config shell-integration <SHELL>`: print the snippet that makes a shell
//! emit prompt and command marks (OSC 133). Contacts no server and reads no
//! config, so it is safe to `eval` from a shell startup file.

use std::process::ExitCode;

use super::CompletionShell;

/// The snippet for `shell`, or `None` when phux ships none for it.
const fn snippet(shell: CompletionShell) -> Option<&'static str> {
    match shell {
        CompletionShell::Zsh => Some(include_str!("shell_integration/phux.zsh")),
        CompletionShell::Bash => Some(include_str!("shell_integration/phux.bash")),
        CompletionShell::Fish => Some(include_str!("shell_integration/phux.fish")),
        _ => None,
    }
}

pub(crate) fn run(shell: CompletionShell) -> ExitCode {
    let Some(text) = snippet(shell) else {
        error!("no shell integration for this shell; phux ships zsh, bash and fish");
        return ExitCode::FAILURE;
    };
    crate::output::bytes(text.as_bytes());
    ExitCode::SUCCESS
}
