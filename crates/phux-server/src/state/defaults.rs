use portable_pty::CommandBuilder;

use super::ServerState;

/// Boot-configuration setters and getters.
impl ServerState {
    /// Mirror [`crate::runtime::ServerConfig::pre_seeded_session`] into state.
    pub(crate) fn set_pre_seeded_session(&mut self, name: Option<String>) {
        self.config.pre_seeded_session = name;
    }

    /// Read the configured seed name used as `AttachTarget::Last`'s no-touch
    /// fallback. `None` means this server was not configured with a seed.
    #[must_use]
    pub(crate) fn pre_seeded_session(&self) -> Option<&str> {
        self.config.pre_seeded_session.as_deref()
    }

    /// Configure the attach-time `CreateIfMissing` seed: a PTY pane
    /// (`cmd`, or the default shell when `None`) or, without `with_pty`, a
    /// PTY-less actor.
    pub fn set_attach_create_pty(&mut self, with_pty: bool, cmd: Option<CommandBuilder>) {
        self.config.attach_create_seeds_pty = with_pty;
        self.config.attach_create_seed_command = cmd;
    }

    /// Read the PTY-mode flag set by [`Self::set_attach_create_pty`].
    #[must_use]
    pub const fn attach_create_seeds_pty(&self) -> bool {
        self.config.attach_create_seeds_pty
    }

    /// A fresh clone of the seed command for one create.
    #[must_use]
    pub fn attach_create_seed_command(&self) -> Option<CommandBuilder> {
        self.config.attach_create_seed_command.clone()
    }

    /// Set the per-pane scrollback bounds.
    pub const fn set_scrollback_limits(&mut self, scrollback: phux_config::ScrollbackLimits) {
        self.config.scrollback = scrollback;
    }

    /// Read the per-pane scrollback bounds set by
    /// [`Self::set_scrollback_limits`].
    #[must_use]
    pub const fn scrollback_limits(&self) -> phux_config::ScrollbackLimits {
        self.config.scrollback
    }

    /// Set the per-session agent-log byte ceiling. Called once at startup
    /// to mirror `defaults.agent-log-bytes`.
    pub const fn set_agent_log_bytes(&mut self, bytes: u32) {
        self.config.agent_log_bytes = bytes;
    }

    /// Bytes of records each new agent session retains (ADR-0103 §4).
    #[must_use]
    pub const fn agent_log_bytes(&self) -> u32 {
        self.config.agent_log_bytes
    }

    /// Set the `[voice]` transcriber settings `TRANSCRIBE` runs. Called once
    /// at server startup to mirror [`crate::runtime::ServerConfig::voice`].
    pub fn set_voice(&mut self, voice: phux_config::VoiceCfg) {
        self.config.voice = voice;
    }

    /// Set the metadata value cap (ADR-0129).
    pub const fn set_metadata_value_bytes(&mut self, bytes: u32) {
        self.config.metadata_value_bytes = bytes;
    }

    /// Largest L3 metadata value the server stores at one key, set by
    /// [`Self::set_metadata_value_bytes`].
    #[must_use]
    pub const fn metadata_value_bytes(&self) -> u32 {
        self.config.metadata_value_bytes
    }

    /// Set the `PHUX_*` process configuration. Called once at server
    /// startup to mirror [`crate::runtime::ServerConfig::env`].
    pub fn set_server_env(&mut self, env: crate::runtime::ServerEnv) {
        self.config.server_env = std::sync::Arc::new(env);
    }

    /// The `PHUX_*` process configuration set by [`Self::set_server_env`]:
    /// listener addresses, TLS pair, credential store, workload locations.
    #[must_use]
    pub fn server_env(&self) -> std::sync::Arc<crate::runtime::ServerEnv> {
        std::sync::Arc::clone(&self.config.server_env)
    }

    /// Read the `[voice]` settings set by [`Self::set_voice`].
    #[must_use]
    pub fn voice(&self) -> phux_config::VoiceCfg {
        self.config.voice.clone()
    }

    /// Set the cwd-inheritance policy.
    pub const fn set_cwd_inheritance(&mut self, mode: phux_config::CwdInheritance) {
        self.config.cwd_inheritance = mode;
    }

    /// Read the working-directory inheritance policy set by
    /// [`Self::set_cwd_inheritance`].
    #[must_use]
    pub const fn cwd_inheritance(&self) -> phux_config::CwdInheritance {
        self.config.cwd_inheritance
    }

    /// Set the default `TERM` for spawned panes.
    pub fn set_term(&mut self, term: String) {
        self.config.term = term;
    }

    /// Read the default `TERM` set by [`Self::set_term`]. A per-spawn
    /// `SPAWN_RESOURCE.env` entry for `TERM` overrides this baseline.
    #[must_use]
    pub fn term(&self) -> &str {
        &self.config.term
    }

    /// Set the resolved default shell.
    pub fn set_shell(&mut self, shell: String) {
        self.config.shell = shell;
    }

    /// Read the resolved default shell set by [`Self::set_shell`].
    #[must_use]
    pub fn shell(&self) -> &str {
        &self.config.shell
    }

    /// Set whether command-less spawns use login mode (service-managed
    /// servers).
    pub const fn set_login_shell(&mut self, login_shell: bool) {
        self.config.login_shell = login_shell;
    }

    /// Read the login-shell flag set by [`Self::set_login_shell`].
    #[must_use]
    pub const fn login_shell(&self) -> bool {
        self.config.login_shell
    }

    /// Set the UDS path injected into panes as `PHUX_SOCKET`.
    pub fn set_server_socket_path(&mut self, path: std::path::PathBuf) {
        self.config.server_socket_path = Some(path);
    }

    /// Read the socket path set by [`Self::set_server_socket_path`].
    /// `None` until the runtime mirrors it (e.g. in state-only tests).
    #[must_use]
    pub fn server_socket_path(&self) -> Option<&std::path::Path> {
        self.config.server_socket_path.as_deref()
    }

    /// Set the multi-client window-size policy.
    pub const fn set_window_size(&mut self, window_size: phux_config::WindowSize) {
        self.config.window_size = window_size;
    }

    /// Read the window-size policy set by [`Self::set_window_size`].
    #[must_use]
    pub const fn window_size(&self) -> phux_config::WindowSize {
        self.config.window_size
    }
}
