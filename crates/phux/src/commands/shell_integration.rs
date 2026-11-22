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

/// What zsh reads first when `ZDOTDIR` points at the server's directory:
/// put the person's own `ZDOTDIR` back, read their `.zshenv`, then the marks.
/// zsh takes the rest of its startup files from the restored directory.
const ZSH_WRAPPER: &str = "\
if [[ -n ${PHUX_ZDOTDIR+x} ]]; then
  export ZDOTDIR=$PHUX_ZDOTDIR
  unset PHUX_ZDOTDIR
else
  unset ZDOTDIR
fi
[[ -r ${ZDOTDIR:-$HOME}/.zshenv ]] && source ${ZDOTDIR:-$HOME}/.zshenv
";

/// Write the files `[defaults] shell-integration` points shells at: a zsh
/// `.zshenv` and a fish vendor snippet. bash needs none.
pub(crate) fn write_files(dir: &std::path::Path) -> std::io::Result<()> {
    let zsh = dir.join("zsh");
    let fish = dir.join("fish/fish/vendor_conf.d");
    std::fs::create_dir_all(&zsh)?;
    std::fs::create_dir_all(&fish)?;
    let marks = include_str!("shell_integration/phux.zsh");
    std::fs::write(zsh.join(".zshenv"), format!("{ZSH_WRAPPER}{marks}"))?;
    std::fs::write(
        fish.join("phux.fish"),
        include_str!("shell_integration/phux.fish"),
    )
}

pub(crate) fn run(shell: CompletionShell) -> ExitCode {
    let Some(text) = snippet(shell) else {
        error!("no shell integration for this shell; phux ships zsh, bash and fish");
        return ExitCode::FAILURE;
    };
    crate::output::bytes(text.as_bytes());
    ExitCode::SUCCESS
}
