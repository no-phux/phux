//! Lossless process launch intent. OS arguments, environment and cwd never
//! round-trip through UTF-8 or a shell command string.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    argv: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    removed: BTreeSet<OsString>,
    cwd: Option<OsString>,
    controlling_tty: bool,
}
impl LaunchCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            argv: vec![program.as_ref().to_owned()],
            env: std::env::vars_os().collect(),
            removed: BTreeSet::new(),
            cwd: None,
            controlling_tty: true,
        }
    }
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) {
        self.argv.push(arg.as_ref().to_owned());
    }
    pub fn args<I, S>(&mut self, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.argv
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
    }
    #[must_use]
    pub const fn get_argv(&self) -> &Vec<OsString> {
        &self.argv
    }
    pub const fn get_argv_mut(&mut self) -> &mut Vec<OsString> {
        &mut self.argv
    }
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        self.removed.remove(key.as_ref());
        self.env
            .insert(key.as_ref().to_owned(), value.as_ref().to_owned());
    }
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) {
        self.env.remove(key.as_ref());
        self.removed.insert(key.as_ref().to_owned());
    }
    pub fn env_clear(&mut self) {
        self.env.clear();
        self.removed.insert(OsString::from("SHELL"));
    }
    pub fn get_env(&self, key: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.env.get(key.as_ref()).map(OsString::as_os_str)
    }
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.env
    }
    pub fn was_removed(&self, key: impl AsRef<OsStr>) -> bool {
        self.removed.contains(key.as_ref())
    }
    pub fn cwd(&mut self, cwd: impl AsRef<OsStr>) {
        self.cwd = Some(cwd.as_ref().to_owned());
    }
    pub fn clear_cwd(&mut self) {
        self.cwd = None;
    }
    #[must_use]
    pub const fn get_cwd(&self) -> Option<&OsString> {
        self.cwd.as_ref()
    }
    pub const fn set_controlling_tty(&mut self, value: bool) {
        self.controlling_tty = value;
    }
    #[must_use]
    pub const fn get_controlling_tty(&self) -> bool {
        self.controlling_tty
    }
}

pub fn default_shell_command(shell: &str, login: bool) -> LaunchCommand {
    let mut command = LaunchCommand::new(shell);
    let name = std::path::Path::new(shell)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(shell);
    if login {
        match name {
            "bash" | "zsh" | "sh" => command.arg("-l"),
            "fish" => command.arg("--login"),
            _ => {}
        }
    }
    apply_term(&mut command, "xterm-256color");
    apply_shell_integration(&mut command, name);
    command
}

/// Where the server keeps the files that make a shell mark its prompts and
/// commands (OSC 133); set once at start when `[defaults] shell-integration`
/// is on.
static SHELL_INTEGRATION: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Turn shell integration on for every default shell this process starts.
pub fn enable_shell_integration(dir: std::path::PathBuf) {
    let _ = SHELL_INTEGRATION.set(dir);
}

/// What bash runs before each prompt: the last command's exit code (after
/// the first prompt), then the prompt mark, leaving `$?` as it was.
const BASH_PROMPT: &str = "__phux_c=$?; [ -n \"$__phux_seen\" ] && printf '\\033]133;D;%s\\a' \"$__phux_c\"; \
    __phux_seen=1; printf '\\033]133;A\\a'; (exit $__phux_c)";

/// Each shell is reached the way it allows without touching the person's
/// startup files: zsh through `ZDOTDIR`, bash through the two prompt
/// variables it takes from the environment, fish through a vendor directory.
fn apply_shell_integration(command: &mut LaunchCommand, shell: &str) {
    if let Some(dir) = SHELL_INTEGRATION.get() {
        integrate(command, shell, dir);
    }
}

fn integrate(command: &mut LaunchCommand, shell: &str, dir: &std::path::Path) {
    let current = |key: &str| command.environment().get(OsStr::new(key)).cloned();
    match shell {
        "zsh" => {
            // The wrapper puts this back before it reads the person's own files.
            if let Some(theirs) = current("ZDOTDIR") {
                command.env("PHUX_ZDOTDIR", theirs);
            }
            command.env("ZDOTDIR", dir.join("zsh"));
        }
        "bash" => {
            let mut prompt = OsString::from(BASH_PROMPT);
            if let Some(theirs) = current("PROMPT_COMMAND").filter(|theirs| !theirs.is_empty()) {
                prompt.push("; ");
                prompt.push(theirs);
            }
            command.env("PROMPT_COMMAND", prompt);
            // bash 4.4 and newer print this as a command starts.
            command.env("PS0", "\u{1b}]133;C\u{7}");
        }
        "fish" => {
            let mut dirs = OsString::from(dir.join("fish"));
            dirs.push(":");
            dirs.push(
                current("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
            );
            command.env("XDG_DATA_DIRS", dirs);
        }
        _ => {}
    }
}
pub fn apply_term(command: &mut LaunchCommand, term: &str) {
    command.env("TERM", term);
}
pub fn apply_terminal_id(command: &mut LaunchCommand, wire: &phux_protocol::ids::ResourceId) {
    if let Some(id) = wire.local_id() {
        command.env("PHUX_TERMINAL_ID", id.to_string());
    }
}
pub fn apply_server_socket(command: &mut LaunchCommand, socket: Option<&std::path::Path>) {
    if let Some(path) = socket {
        command.env("PHUX_SOCKET", path);
    }
}
pub fn apply_spawn_cwd(command: &mut LaunchCommand, cwd: Option<&str>, session: &str) {
    let Some(path) = cwd else {
        return;
    };
    if command.get_cwd().is_some() {
        return;
    }
    if std::path::Path::new(path).is_dir()
        && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
    {
        command.cwd(path);
    } else {
        tracing::warn!(%session, cwd = %path, "wire cwd is not an enterable directory; falling back to the default spawn directory");
    }
}
pub fn apply_strict_spawn_cwd(command: &mut LaunchCommand, cwd: &std::path::Path) {
    command.cwd(cwd);
    command.env("PHUX_FD_SHRINK_STRICT_CWD", "1");
}

#[must_use]
pub fn shell_command(shell: &str, text: &str, login: bool) -> LaunchCommand {
    let mut command = default_shell_command(shell, login);
    command.args(["-c", text]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_shell_is_integrated_its_own_way_and_keeps_what_was_set() {
        let dir = std::path::Path::new("/state/shell-integration");
        let value = |command: &LaunchCommand, key: &str| {
            let found = command.environment().get(OsStr::new(key));
            found.map(|value| value.to_string_lossy().into_owned())
        };

        let mut zsh = LaunchCommand::new("zsh");
        zsh.env("ZDOTDIR", "/home/me/.zsh");
        integrate(&mut zsh, "zsh", dir);
        assert_eq!(
            value(&zsh, "ZDOTDIR").as_deref(),
            Some("/state/shell-integration/zsh")
        );
        assert_eq!(
            value(&zsh, "PHUX_ZDOTDIR").as_deref(),
            Some("/home/me/.zsh")
        );

        let mut bash = LaunchCommand::new("bash");
        bash.env("PROMPT_COMMAND", "theirs");
        integrate(&mut bash, "bash", dir);
        let prompt = value(&bash, "PROMPT_COMMAND").unwrap_or_default();
        assert!(
            prompt.starts_with("__phux_c=$?") && prompt.ends_with("; theirs"),
            "{prompt}"
        );
        assert!(value(&bash, "PS0").is_some_and(|ps0| ps0.contains("133;C")));

        let mut fish = LaunchCommand::new("fish");
        fish.env("XDG_DATA_DIRS", "/opt/share");
        integrate(&mut fish, "fish", dir);
        assert_eq!(
            value(&fish, "XDG_DATA_DIRS").as_deref(),
            Some("/state/shell-integration/fish:/opt/share")
        );

        let mut other = LaunchCommand::new("nu");
        let before = other.clone();
        integrate(&mut other, "nu", dir);
        assert_eq!(other, before);
    }
}
