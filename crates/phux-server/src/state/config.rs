//! Boot-time configuration mirrored into [`super::ServerState`].
//!
//! Every field here is written exactly twice in production: once by
//! [`ServerConfig::default`] (through `ServerState::new`) and once by the
//! matching `set_*` method, called from `ServerRuntime::run_async` before
//! the accept loops start. Nothing mutates them while the server is
//! serving clients, which is what separates them from the live runtime
//! state they used to sit next to.
//!
//! The accessors and setters stay on `ServerState` (see
//! `state::defaults` and `state::policy`) so the crate's public surface is
//! unchanged; this struct is an internal grouping only.
//!
//! Not to be confused with [`crate::runtime::ServerConfig`], the
//! caller-supplied runtime configuration this is mirrored *from*.

use portable_pty::CommandBuilder;

/// Write-once boot configuration owned by [`super::ServerState`].
///
/// The struct and its fields are `pub(super)` so the `state` submodules
/// that already read the flat fields keep reading them directly, while
/// nothing outside the module tree can name this type — matching the
/// visibility the flat fields had, and keeping the crate from exporting a
/// second public `ServerConfig` alongside [`crate::runtime::ServerConfig`].
/// (Workspace lints set `unreachable_pub = "warn"`, and `just lint` runs
/// with `-D warnings`, so a bare `pub` here would fail CI.)
#[derive(Debug)]
pub(super) struct ServerConfig {
    /// Lines of scrollback retained per pane (`defaults.history-limit`).
    /// Mirrors [`crate::runtime::ServerConfig::history_limit`] so the
    /// attach-time creation path (`AttachTarget::CreateIfMissing`) and
    /// `SPAWN_TERMINAL` build their `TerminalActor`s with the configured
    /// cap without an extra channel to the runtime. Set by the runtime
    /// via [`super::ServerState::set_history_limit`] right after
    /// `SharedState::new`.
    ///
    /// Defaults to the `phux_config` schema default so tests that never
    /// call the setter still get a sane bound.
    pub(super) history_limit: u32,
    /// How a freshly-spawned pane chooses its working directory
    /// (`defaults.cwd-inheritance`). Mirrors
    /// [`crate::runtime::ServerConfig::cwd_inheritance`] so the
    /// `SPAWN_TERMINAL` handler resolves the new pane's CWD without an
    /// extra channel to the runtime. Set by the runtime via
    /// [`super::ServerState::set_cwd_inheritance`] right after
    /// `SharedState::new`.
    ///
    /// Defaults to the `phux_config` schema default
    /// ([`phux_config::CwdInheritance::InheritFocused`]) so tests that
    /// never call the setter exercise the tmux-default behavior.
    pub(super) cwd_inheritance: phux_config::CwdInheritance,
    /// `TERM` advertised to the inner program of every server-spawned pane
    /// (`defaults.term`, phux-ign). Mirrors
    /// [`crate::runtime::ServerConfig::term`] so the attach-time creation
    /// path and `SPAWN_TERMINAL` apply it as the PTY's `TERM` baseline
    /// without an extra channel to the runtime. A per-spawn
    /// `SPAWN_TERMINAL.env` entry for `TERM` overrides it. Set by the
    /// runtime via [`super::ServerState::set_term`] right after
    /// `SharedState::new`.
    ///
    /// Defaults to the `phux_config` schema default (`xterm-256color`) so
    /// tests that never call the setter get the safe baseline.
    pub(super) term: String,
    /// Resolved default shell for server-spawned panes (phux-i0e8.4.1):
    /// `defaults.shell` → `$SHELL` → `/bin/sh`, resolved once by the
    /// binary from its single config load. Mirrors
    /// [`crate::runtime::ServerConfig::shell`] so the attach-time
    /// creation path, `SESSION_CREATE_KEY`, and a command-less
    /// `SPAWN_TERMINAL` spawn the configured shell without an extra
    /// channel to the runtime. A wire `command` always wins over this
    /// default. Set by the runtime via [`super::ServerState::set_shell`]
    /// right after `SharedState::new`.
    ///
    /// Defaults to `resolve_shell(None)` (`$SHELL`, else `/bin/sh`) so
    /// state-only tests keep the pre-config behavior.
    pub(super) shell: String,
    /// The UDS path this server listens on. Mirrors
    /// [`crate::runtime::ServerConfig::socket_path`] so every pane spawn
    /// site can inject it into the child environment as `PHUX_SOCKET`
    /// (phux-cufw) without an extra channel to the runtime. Set by the
    /// runtime via [`super::ServerState::set_server_socket_path`] right
    /// after `SharedState::new`.
    ///
    /// `None` when the runtime never mirrors it (state-only tests):
    /// spawned panes then carry no `PHUX_SOCKET` and an in-pane `phux`
    /// falls back to the default-socket resolution.
    pub(super) server_socket_path: Option<std::path::PathBuf>,
    /// How a Terminal viewed by clients of differing sizes resolves its one
    /// authoritative PTY geometry (`defaults.window-size`, phux-nk07). Mirrors
    /// [`crate::runtime::ServerConfig::window_size`] so `handle_viewport_resize`
    /// can apply the policy across every subscriber's viewport instead of
    /// last-writer-wins. Set by the runtime via
    /// [`super::ServerState::set_window_size`] right after `SharedState::new`;
    /// defaults to the `phux_config` schema default
    /// ([`phux_config::WindowSize::Smallest`] — never crops).
    pub(super) window_size: phux_config::WindowSize,
    /// HELLO authorization engine (ADR-0072). Defaults to
    /// [`crate::policy::PermissivePolicy`]; the runtime overwrites it only
    /// when [`crate::runtime::ServerConfig::policy_engine`] is `Some`.
    pub(super) policy_engine: std::sync::Arc<dyn crate::policy::PolicyEngine>,
    /// Whether an `AttachTarget::CreateIfMissing` that fires seeds a real
    /// PTY pane. Mirrors [`crate::runtime::ServerConfig::seed_with_pty`] so
    /// the attach-time creation path matches the server's startup
    /// configuration. Set by the runtime via
    /// [`super::ServerState::set_attach_create_pty`] right after
    /// `SharedState::new`.
    ///
    /// Defaults to `false` so tests that never call the setter exercise
    /// the cheaper no-PTY actor (matches every existing integration
    /// test that uses `spawn_server`).
    pub(super) attach_create_seeds_pty: bool,
    /// Optional pre-built `CommandBuilder` used when
    /// [`Self::attach_create_seeds_pty`] is `true` and `CreateIfMissing`
    /// fires. `None` falls back to
    /// [`crate::terminal_actor::default_shell_command`] over the resolved
    /// default shell ([`Self::shell`]: `defaults.shell`, `$SHELL`, or
    /// `/bin/sh`).
    pub(super) attach_create_seed_command: Option<CommandBuilder>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            history_limit: phux_config::DefaultsCfg::default().history_limit,
            cwd_inheritance: phux_config::CwdInheritance::default(),
            term: phux_config::DefaultsCfg::default().term,
            shell: crate::terminal_actor::resolve_shell(None),
            server_socket_path: None,
            window_size: phux_config::WindowSize::default(),
            policy_engine: std::sync::Arc::new(crate::policy::PermissivePolicy::INSTANCE),
            attach_create_seeds_pty: false,
            attach_create_seed_command: None,
        }
    }
}
