//! Lossless process launch intent. OS arguments, environment and cwd never
//! round-trip through UTF-8 or a shell command string.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};

#[derive(Clone, PartialEq, Eq)]
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
            env: std::env::vars_os()
                // A nested coordinator must not advertise the outer terminal's
                // pane-control socket. Only its own startup can install one.
                .filter(|(key, _)| key != TERMINAL_BROWSER_SOCKET_ENV)
                .chain(pane_env())
                .collect(),
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

impl std::fmt::Debug for LaunchCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: Vec<_> = self
            .env
            .iter()
            .map(|(key, value)| {
                let shown = if key == OSC7577_TOKEN {
                    std::ffi::OsStr::new("[redacted]")
                } else {
                    value.as_os_str()
                };
                (key, shown)
            })
            .collect();
        formatter
            .debug_struct("LaunchCommand")
            .field("argv", &self.argv)
            .field("env", &env)
            .field("removed", &self.removed)
            .field("cwd", &self.cwd)
            .field("controlling_tty", &self.controlling_tty)
            .finish()
    }
}

/// Baseline `TERM` for [`default_shell_command`] and [`shell_command`].
///
/// Ghostty's terminfo advertises kitty keyboard support, which breaks
/// ncurses apps such as htop. This matches the `defaults.term` schema
/// default, which [`apply_term`] overrides at runtime.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// Env mark that forbids the PTY spawner from falling back to another
/// directory when the recorded cwd cannot be entered. The spawner reads it
/// and strips it from the child's environment.
pub const STRICT_CWD_KEY: &str = "PHUX_FD_SHRINK_STRICT_CWD";

/// Discovery endpoint for terminal-browser's optional local pane controls.
pub const TERMINAL_BROWSER_SOCKET_ENV: &str = "TERMINAL_BROWSER_TERMINAL_SOCKET";

/// A plain interactive `shell`. With `login`, a known shell gets its login
/// flag (`-l` for bash/zsh/sh, `--login` for fish); an unknown shell gets
/// none, because an unrecognized flag can fail the exec.
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
    apply_term(&mut command, DEFAULT_TERM);
    apply_shell_integration(&mut command, name);
    command
}

/// Variables every pane's program starts with, beyond the server's own
/// environment. Startup installs configured values, then listener endpoints.
static PANE_ENV: std::sync::Mutex<BTreeMap<OsString, OsString>> =
    std::sync::Mutex::new(BTreeMap::new());

/// Add or replace variables for every program this process launches.
pub fn set_pane_env(vars: Vec<(OsString, OsString)>) {
    PANE_ENV
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(vars);
}

fn pane_env() -> Vec<(OsString, OsString)> {
    PANE_ENV
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Fill startup variables in a command prepared before listeners bound.
/// Explicit overrides and removals remain launch intent.
pub fn apply_pane_env(command: &mut LaunchCommand) {
    for (key, value) in pane_env() {
        if command.get_env(&key).is_none() && !command.was_removed(&key) {
            command.env(key, value);
        }
    }
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
/// Environment variable a pane's first process uses to authenticate OSC 7577.
pub const OSC7577_TOKEN: &str = "OSC7577_TOKEN";

/// Characters a token may contain. 64 symbols, so a uniform byte maps without bias.
const TOKEN_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

/// How long a minted token is. The spec allows 22 to 64 characters.
const TOKEN_LEN: usize = 32;

/// A fresh per-pane token. Random, not derived from a clock, a counter, or a pid.
///
/// # Panics
///
/// When the operating system cannot provide random bytes. A token that is not
/// random would not protect the pane, so there is no fallback.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "a program-resume token that is not from the OS CSPRNG is not a token"
)]
pub fn mint_program_resume_token() -> String {
    let mut bytes = [0u8; TOKEN_LEN];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable for a program-resume token");
    bytes
        .iter()
        .map(|byte| TOKEN_ALPHABET[usize::from(*byte) % TOKEN_ALPHABET.len()] as char)
        .collect()
}

/// Set `OSC7577_TOKEN` on the pane's first process. The value is not logged.
pub fn apply_program_resume_token(command: &mut LaunchCommand, token: &str) {
    command.env(OSC7577_TOKEN, token);
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
    command.env(STRICT_CWD_KEY, "1");
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
    fn two_panes_get_distinct_tokens_and_debug_hides_them() {
        let first = mint_program_resume_token();
        let second = mint_program_resume_token();
        assert_ne!(first, second);
        assert_eq!(first.len(), 32);
        assert!(
            first
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' })
        );
        let mut command = LaunchCommand::new("sh");
        apply_program_resume_token(&mut command, &first);
        let rendered = format!("{command:?}");
        assert!(!rendered.contains(&first));
        assert!(rendered.contains("[redacted]"));
        assert_eq!(
            command
                .get_env(OSC7577_TOKEN)
                .map(|value| value.to_string_lossy().into_owned()),
            Some(first)
        );
    }

    #[test]
    fn pane_variables_reach_every_launch() {
        let mut prepared = LaunchCommand::new("prepared-before-listening");
        let mut cleared = prepared.clone();
        cleared.env_remove("PHUX_TEST_PANE_VAR");
        set_pane_env(vec![("PHUX_TEST_PANE_VAR".into(), "set".into())]);
        apply_pane_env(&mut prepared);
        apply_pane_env(&mut cleared);
        assert_eq!(
            prepared.get_env("PHUX_TEST_PANE_VAR"),
            Some(OsStr::new("set"))
        );
        assert_eq!(cleared.get_env("PHUX_TEST_PANE_VAR"), None);
        let command = LaunchCommand::new("anything");
        let seen = command.environment().get(OsStr::new("PHUX_TEST_PANE_VAR"));
        assert_eq!(
            seen.map(|value| value.to_string_lossy().into_owned())
                .as_deref(),
            Some("set")
        );
    }

    fn argv_strings(command: &LaunchCommand) -> Vec<String> {
        command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn login_flags_by_shell() {
        let cases: &[(&str, bool, &[&str])] = &[
            ("/bin/zsh", false, &["/bin/zsh"]),
            ("/bin/bash", true, &["/bin/bash", "-l"]),
            ("sh", true, &["sh", "-l"]),
            (
                "/opt/homebrew/bin/fish",
                true,
                &["/opt/homebrew/bin/fish", "--login"],
            ),
            ("/opt/exotic/rc", true, &["/opt/exotic/rc"]),
        ];
        for (shell, login, argv) in cases {
            assert_eq!(
                argv_strings(&default_shell_command(shell, *login)),
                *argv,
                "{shell}"
            );
        }
    }

    #[test]
    fn shell_command_applies_login_flag_before_dash_c() {
        let command = shell_command("/bin/zsh", "htop", true);
        assert_eq!(argv_strings(&command), ["/bin/zsh", "-l", "-c", "htop"]);
        let command = shell_command("/opt/fancy/fish", "btop --utf-force", false);
        assert_eq!(
            argv_strings(&command),
            ["/opt/fancy/fish", "-c", "btop --utf-force"]
        );
    }

    /// Ghostty's terminfo advertises `fullkbd`, which makes ncurses apps
    /// (htop) push kitty flags they cannot parse back (phux-7vx).
    #[test]
    fn default_shells_start_on_the_baseline_term() {
        assert_eq!(DEFAULT_TERM, "xterm-256color");
        for command in [
            default_shell_command("/bin/sh", false),
            shell_command("/bin/sh", "true", true),
        ] {
            assert_eq!(command.get_env("TERM"), Some(OsStr::new(DEFAULT_TERM)));
        }
    }

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
