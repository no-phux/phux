//! `phux man` — emit a roff manual page for the CLI to stdout.
//!
//! The page is rendered from the parsed clap command tree (the same tree the
//! `help_inventory` test pins), so it can never drift from the real command
//! surface. Install or preview it, e.g.:
//!
//! ```text
//! phux man > ~/.local/share/man/man1/phux.1
//! phux man | man -l -            # preview without installing
//! ```

use std::io;
use std::process::ExitCode;

/// Render the top-level `phux.1` man page to stdout. `cmd` is the fully-built
/// CLI command tree (`Cli::command()`), passed in so this stays decoupled from
/// the `Cli` type that lives in `main`.
pub(crate) fn run_man(cmd: clap::Command) -> ExitCode {
    match write_man(cmd, &mut io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux man: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Render the roff man page for `cmd` into `out`. Split from [`run_man`] so it
/// can be exercised against an in-memory buffer.
fn write_man(cmd: clap::Command, out: &mut impl io::Write) -> io::Result<()> {
    clap_mangen::Man::new(cmd).render(out)
}

#[cfg(test)]
mod tests {
    use super::write_man;

    #[test]
    fn writes_a_roff_page_naming_the_binary() {
        let cmd = clap::Command::new("phux")
            .about("a terminal control plane")
            .subcommand(clap::Command::new("ls"))
            .subcommand(clap::Command::new("attach"));
        let mut buf = Vec::new();
        write_man(cmd, &mut buf).expect("man render succeeds");
        let page = String::from_utf8(buf).expect("man page is valid UTF-8");
        assert!(page.contains(".TH"), "missing roff .TH title header");
        assert!(page.contains("phux"), "man page does not name the binary");
    }
}
