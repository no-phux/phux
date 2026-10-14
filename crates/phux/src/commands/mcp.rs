//! `phux mcp` — transparent launcher for the bundled MCP adapter.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitCode};

fn command_for(program: &std::path::Path, args: &[OsString]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

/// Replace this process with `phux-mcp`, preserving stdio, signals, and status.
pub(crate) fn run(args: &[OsString]) -> ExitCode {
    let Some(program) = crate::companion::find_live_mcp() else {
        error!("could not find the bundled phux-mcp executable beside phux or on PATH");
        hint!("reinstall phux so both release binaries are present");
        return ExitCode::from(127);
    };

    // `exec` only returns when the executable disappeared or became unusable
    // after discovery. In the normal path there is no intermediate process to
    // buffer stdio, intercept signals, or translate the MCP server's status.
    let err = command_for(&program, args).exec();
    error!("could not exec {}: {err}", program.display());
    ExitCode::from(126)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture assertions")]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn delegated_arguments_are_byte_preserving_and_in_order() {
        let args = [
            OsString::from("--schema"),
            OsString::from("--future-option=value"),
        ];
        let command = command_for(Path::new("/tmp/phux-mcp"), &args);

        assert_eq!(command.get_program(), "/tmp/phux-mcp");
        assert_eq!(command.get_args().collect::<Vec<_>>(), args);
    }
}
