//! `phux completion <SHELL>` — emit a shell completion script on stdout.
//!
//! The script is a thin usage-rs shell that asks the live binary for
//! candidates, so completions follow the real CLI surface (hidden verbs and
//! flags are never offered). This never contacts a server.

use std::process::ExitCode;

use crate::Cli;

/// Render the completion script for `shell` to stdout.
pub(crate) fn run_completion(shell: usage::complete::Shell) -> ExitCode {
    crate::output::bytes(Cli::completion_script(shell).as_bytes());
    ExitCode::SUCCESS
}
