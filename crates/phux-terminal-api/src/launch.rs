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
    command
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
