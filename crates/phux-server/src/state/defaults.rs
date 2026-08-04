use portable_pty::CommandBuilder;

use super::ServerState;

impl ServerState {
    /// Configure the PTY mode and seed command used by
    /// `crate::runtime::handle_attach`'s
    /// `AttachTarget::CreateIfMissing` branch (phux-k61.3).
    ///
    /// Called once at server startup to mirror
    /// [`crate::runtime::ServerConfig::seed_with_pty`] /
    /// [`crate::runtime::ServerConfig::seed_command`] into state, so the
    /// attach-time creation path can read them without an extra channel
    /// to the runtime.
    ///
    /// When `with_pty` is `false`, `cmd` is ignored — the create path
    /// spawns a no-PTY actor instead. Setting `cmd = None` with
    /// `with_pty = true` falls back to
    /// [`crate::terminal_actor::default_shell_command`] at create time.
    pub fn set_attach_create_pty(&mut self, with_pty: bool, cmd: Option<CommandBuilder>) {
        self.config.attach_create_seeds_pty = with_pty;
        self.config.attach_create_seed_command = cmd;
    }

    /// Read the PTY-mode flag set by [`Self::set_attach_create_pty`].
    #[must_use]
    pub const fn attach_create_seeds_pty(&self) -> bool {
        self.config.attach_create_seeds_pty
    }

    /// Clone the optional pre-built seed command. Used by the create
    /// path inside `handle_attach`: each `AttachTarget::CreateIfMissing`
    /// that fires gets a fresh clone, so the slot stays populated for
    /// future creates. `CommandBuilder` is `Clone` (per portable-pty
    /// 0.8), so this is cheap.
    #[must_use]
    pub fn attach_create_seed_command(&self) -> Option<CommandBuilder> {
        self.config.attach_create_seed_command.clone()
    }

    /// Set the per-pane scrollback cap (`defaults.history-limit`) used
    /// by the attach-time creation path and `SPAWN_TERMINAL`. Called
    /// once at server startup to mirror
    /// [`crate::runtime::ServerConfig::history_limit`] into state.
    pub const fn set_history_limit(&mut self, history_limit: u32) {
        self.config.history_limit = history_limit;
    }

    /// Read the per-pane scrollback cap set by [`Self::set_history_limit`].
    #[must_use]
    pub const fn history_limit(&self) -> u32 {
        self.config.history_limit
    }

    /// Set the working-directory inheritance policy
    /// (`defaults.cwd-inheritance`) used by `SPAWN_TERMINAL`. Called once
    /// at server startup to mirror
    /// [`crate::runtime::ServerConfig::cwd_inheritance`] into state.
    pub const fn set_cwd_inheritance(&mut self, mode: phux_config::CwdInheritance) {
        self.config.cwd_inheritance = mode;
    }

    /// Read the working-directory inheritance policy set by
    /// [`Self::set_cwd_inheritance`].
    #[must_use]
    pub const fn cwd_inheritance(&self) -> phux_config::CwdInheritance {
        self.config.cwd_inheritance
    }

    /// Set the default `TERM` (`defaults.term`) advertised to
    /// server-spawned panes. Called once at server startup to mirror
    /// [`crate::runtime::ServerConfig::term`] into state.
    pub fn set_term(&mut self, term: String) {
        self.config.term = term;
    }

    /// Read the default `TERM` set by [`Self::set_term`]. A per-spawn
    /// `SPAWN_TERMINAL.env` entry for `TERM` overrides this baseline.
    #[must_use]
    pub fn term(&self) -> &str {
        &self.config.term
    }

    /// Set the resolved default shell (`defaults.shell` → `$SHELL` →
    /// `/bin/sh`, phux-i0e8.4.1) server-spawned panes run when no wire
    /// `command` names a program. Called once at server startup to
    /// mirror [`crate::runtime::ServerConfig::shell`] into state.
    pub fn set_shell(&mut self, shell: String) {
        self.config.shell = shell;
    }

    /// Read the resolved default shell set by [`Self::set_shell`].
    #[must_use]
    pub fn shell(&self) -> &str {
        &self.config.shell
    }

    /// Set the UDS path this server listens on. Called once at server
    /// startup to mirror [`crate::runtime::ServerConfig::socket_path`]
    /// into state so every pane spawn site can inject it as
    /// `PHUX_SOCKET` (phux-cufw).
    pub fn set_server_socket_path(&mut self, path: std::path::PathBuf) {
        self.config.server_socket_path = Some(path);
    }

    /// Read the socket path set by [`Self::set_server_socket_path`].
    /// `None` until the runtime mirrors it (e.g. in state-only tests).
    #[must_use]
    pub fn server_socket_path(&self) -> Option<&std::path::Path> {
        self.config.server_socket_path.as_deref()
    }

    /// Set the multi-client window-size policy (`defaults.window-size`,
    /// phux-nk07). Called once at server startup to mirror
    /// [`crate::runtime::ServerConfig::window_size`] into state.
    pub const fn set_window_size(&mut self, window_size: phux_config::WindowSize) {
        self.config.window_size = window_size;
    }

    /// Read the window-size policy set by [`Self::set_window_size`].
    #[must_use]
    pub const fn window_size(&self) -> phux_config::WindowSize {
        self.config.window_size
    }
}
