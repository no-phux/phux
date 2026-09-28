//! Boot-time configuration mirrored into [`super::ServerState`] from
//! [`crate::runtime::ServerConfig`]: written by the defaults and once by a
//! `set_*` call before the accept loops start, then never changed.

use portable_pty::CommandBuilder;

/// Write-once boot configuration owned by [`super::ServerState`]
/// (`pub(super)`, so no second public `ServerConfig` is exported).
#[derive(Debug)]
pub(super) struct ServerConfig {
    /// Per-pane scrollback bounds (`defaults.history-limit` / `-bytes`),
    /// read by the attach-create and `SPAWN_RESOURCE` paths.
    pub(super) scrollback: phux_config::ScrollbackLimits,
    /// Bytes of agent-session records retained (`defaults.agent-log-bytes`,
    /// ADR-0103 §4).
    pub(super) agent_log_bytes: u32,
    /// `[voice]` transcriber settings behind `TRANSCRIBE`.
    pub(super) voice: phux_config::VoiceCfg,
    /// Largest stored L3 metadata value (`limits.metadata-value-bytes`,
    /// ADR-0129).
    pub(super) metadata_value_bytes: u32,
    /// How a new pane picks its cwd (`defaults.cwd-inheritance`).
    pub(super) cwd_inheritance: phux_config::CwdInheritance,
    /// `TERM` for server-spawned panes (`defaults.term`); a per-spawn env
    /// entry overrides it.
    pub(super) term: String,
    /// Resolved default shell (`defaults.shell`, `$SHELL`, `/bin/sh`); a wire
    /// `command` wins.
    pub(super) shell: String,
    /// Whether command-less spawns run [`Self::shell`] in login mode.
    pub(super) login_shell: bool,
    /// The UDS path, injected into panes as `PHUX_SOCKET`; `None` in
    /// state-only tests.
    pub(super) server_socket_path: Option<std::path::PathBuf>,
    /// How a Terminal shared by differently sized clients picks its PTY size
    /// (`defaults.window-size`).
    pub(super) window_size: phux_config::WindowSize,
    /// HELLO authorization engine (ADR-0072); permissive unless configured.
    pub(super) policy_engine: std::sync::Arc<dyn crate::policy::PolicyEngine>,
    /// The startup authorization posture (`docs/spec/workload-auth.md` §8).
    pub(super) policy_posture: crate::policy::PolicyPosture,
    /// Whether an attach-time `CreateIfMissing` seeds a real PTY pane
    /// (tests default to the cheaper PTY-less actor).
    pub(super) attach_create_seeds_pty: bool,
    /// Override command for that seed pane; `None` uses the default shell.
    pub(super) attach_create_seed_command: Option<CommandBuilder>,
    /// The pre-seeded session's name: the `AttachTarget::Last` fallback
    /// before any session is touched, while it is live. Never authorizes
    /// creation.
    pub(super) pre_seeded_session: Option<String>,
    /// Retain-on-exit settings (ADR-0124).
    pub(super) retain: super::retained::RetainPolicy,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            scrollback: phux_config::DefaultsCfg::default().scrollback_limits(),
            agent_log_bytes: phux_config::DEFAULT_AGENT_LOG_BYTES,
            voice: phux_config::VoiceCfg::default(),
            metadata_value_bytes: phux_config::DEFAULT_METADATA_VALUE_BYTES,
            cwd_inheritance: phux_config::CwdInheritance::default(),
            term: phux_config::DefaultsCfg::default().term,
            shell: crate::terminal_actor::resolve_shell(None),
            login_shell: false,
            server_socket_path: None,
            window_size: phux_config::WindowSize::default(),
            policy_engine: std::sync::Arc::new(crate::policy::PermissivePolicy::INSTANCE),
            policy_posture: crate::policy::PolicyPosture::Transitional {
                remote_listener: false,
            },
            attach_create_seeds_pty: false,
            attach_create_seed_command: None,
            pre_seeded_session: None,
            retain: super::retained::RetainPolicy::default(),
        }
    }
}
