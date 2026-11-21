//! Boot-time configuration owned by [`super::ServerState`]: built once from
//! [`crate::runtime::ServerConfig`] and the resolved policy posture before the
//! state exists, then never changed (tests use `config_mut`).

use std::time::Duration;

/// Approval bounds handed to the approval table at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApprovalLimits {
    /// How long a held action waits before it expires.
    pub(crate) ttl: Duration,
    /// Held actions one connection may have pending.
    pub(crate) max_pending: u32,
    /// Held actions the whole server may have pending.
    pub(crate) max_total: u32,
}

impl Default for ApprovalLimits {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(u64::from(phux_config::DEFAULT_APPROVAL_TTL_SECS)),
            max_pending: phux_config::DEFAULT_APPROVAL_MAX_PENDING,
            max_total: phux_config::DEFAULT_APPROVAL_MAX_PENDING_TOTAL,
        }
    }
}

/// Event-journal bounds handed to the journal at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JournalBounds {
    /// Most entries the replay ring keeps.
    pub(crate) entries: usize,
    /// Most bytes the replay ring keeps.
    pub(crate) bytes: usize,
}

impl JournalBounds {
    fn from_config(entries: u32, bytes: u32) -> Self {
        Self {
            entries: usize::try_from(entries).unwrap_or(usize::MAX),
            bytes: usize::try_from(bytes).unwrap_or(usize::MAX),
        }
    }
}

impl Default for JournalBounds {
    fn default() -> Self {
        Self::from_config(
            phux_config::DEFAULT_EVENT_JOURNAL_ENTRIES,
            phux_config::DEFAULT_EVENT_JOURNAL_BYTES,
        )
    }
}

/// Immutable boot configuration owned by [`super::ServerState`].
#[derive(Debug)]
pub(crate) struct StateConfig {
    /// Per-pane scrollback bounds (`defaults.history-limit` / `-bytes`),
    /// read by the attach-create and `SPAWN_RESOURCE` paths.
    pub(crate) scrollback: phux_config::ScrollbackLimits,
    /// Bytes of records each new agent session retains
    /// (`defaults.agent-log-bytes`, ADR-0103 §4).
    pub(crate) agent_log_bytes: u32,
    /// `[voice]` transcriber settings behind `TRANSCRIBE`.
    pub(crate) voice: phux_config::VoiceCfg,
    /// `[restore]`: the commands a relaunch may resume a program with.
    pub(crate) restore: phux_config::RestoreCfg,
    /// `[forward]`: whether `FORWARD_*` is served and what it may dial.
    pub(crate) forward: phux_config::ForwardCfg,
    /// `notifications.from-programs`: whether OSC 9 / OSC 777 become notes.
    pub(crate) from_programs: phux_config::FromPrograms,
    /// Largest L3 metadata value the server stores at one key
    /// (`limits.metadata-value-bytes`, ADR-0129).
    pub(crate) metadata_value_bytes: u32,
    /// How a new pane picks its cwd (`defaults.cwd-inheritance`).
    pub(crate) cwd_inheritance: phux_config::CwdInheritance,
    /// The start directory under `cwd-inheritance = "fixed"`.
    pub(crate) fixed_cwd: Option<String>,
    /// `TERM` for server-spawned panes (`defaults.term`); a per-spawn
    /// `SPAWN_RESOURCE.env` entry for `TERM` overrides it.
    pub(crate) term: String,
    /// Resolved default shell (`defaults.shell`, `$SHELL`, `/bin/sh`); a wire
    /// `command` wins.
    pub(crate) shell: String,
    /// Whether command-less spawns run [`Self::shell`] in login mode
    /// (service-managed servers).
    pub(crate) login_shell: bool,
    /// The UDS path, injected into panes as `PHUX_SOCKET`; `None` in
    /// state-only tests.
    pub(crate) server_socket_path: Option<std::path::PathBuf>,
    /// The server's own executable, which native hosts and mirror listings
    /// run; `None` resolves the running binary. Tests point it at a fixture.
    pub(crate) native_host_executable: Option<std::path::PathBuf>,
    /// How a Terminal shared by differently sized clients picks its PTY size
    /// (`defaults.window-size`).
    pub(crate) window_size: phux_config::WindowSize,
    /// When to hold an idle-sleep inhibitor (`defaults.keep-awake`).
    pub(crate) keep_awake: phux_config::KeepAwake,
    /// HELLO authorization engine (ADR-0072); the `local`
    /// [`crate::policy::ScopedPolicy`] unless configured.
    pub(crate) policy_engine: std::sync::Arc<dyn crate::policy::PolicyEngine>,
    /// The startup authorization posture (`docs/remote.md#who-can-connect`).
    pub(crate) policy_posture: crate::policy::PolicyPosture,
    /// Whether QUIC listeners admit the owner's own tailnet devices
    /// (`[policy] tailnet-owner` under `paired`,
    /// `docs/config.md#tailnet-owner-devices`).
    pub(crate) tailnet_owner: bool,
    /// Whether an attach-time `CreateIfMissing` seeds a real PTY pane
    /// running the request's command or the default shell (tests default to
    /// the cheaper PTY-less actor).
    pub(crate) attach_create_seeds_pty: bool,
    /// The pre-seeded session's name: the `AttachTarget::Last` fallback
    /// before any session is touched, while it is live. Never authorizes
    /// creation.
    pub(crate) pre_seeded_session: Option<String>,
    /// Retain-on-exit settings (ADR-0124).
    pub(crate) retain: super::retained::RetainPolicy,
    /// The `PHUX_*` process configuration the remote surface reads
    /// (listener addresses, TLS pair, credential store, workload locations).
    pub(crate) server_env: std::sync::Arc<crate::runtime::ServerEnv>,
    /// Event-journal bounds; the journal is built with them.
    pub(crate) journal_bounds: JournalBounds,
    /// Approval bounds; the approval table is built with them.
    pub(crate) approval_limits: ApprovalLimits,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            scrollback: phux_config::DefaultsCfg::default().scrollback_limits(),
            agent_log_bytes: phux_config::DEFAULT_AGENT_LOG_BYTES,
            voice: phux_config::VoiceCfg::default(),
            restore: phux_config::RestoreCfg::default(),
            forward: phux_config::ForwardCfg::default(),
            from_programs: phux_config::FromPrograms::Off,
            metadata_value_bytes: phux_config::DEFAULT_METADATA_VALUE_BYTES,
            cwd_inheritance: phux_config::CwdInheritance::default(),
            fixed_cwd: None,
            term: phux_config::DefaultsCfg::default().term,
            shell: phux_config::shell::resolve_shell(None),
            login_shell: false,
            server_socket_path: None,
            native_host_executable: None,
            window_size: phux_config::WindowSize::default(),
            keep_awake: phux_config::KeepAwake::default(),
            policy_engine: std::sync::Arc::new(crate::policy::ScopedPolicy::local()),
            policy_posture: crate::policy::PolicyPosture::Local,
            tailnet_owner: false,
            attach_create_seeds_pty: false,
            pre_seeded_session: None,
            retain: super::retained::RetainPolicy::default(),
            server_env: std::sync::Arc::default(),
            journal_bounds: JournalBounds::default(),
            approval_limits: ApprovalLimits::default(),
        }
    }
}

impl StateConfig {
    /// Build the state's configuration from the runtime configuration and
    /// the resolved policy posture. `engine` is the posture's engine; `None`
    /// keeps the embedder's injected engine, else the `local` default.
    pub(crate) fn from_runtime(
        cfg: &crate::runtime::ServerConfig,
        socket_path: &std::path::Path,
        posture: crate::policy::PolicyPosture,
        engine: Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>,
    ) -> Self {
        let defaults = Self::default();
        let policy_engine = engine
            .or_else(|| cfg.policy_engine.clone())
            .unwrap_or(defaults.policy_engine);
        Self {
            scrollback: cfg.scrollback,
            agent_log_bytes: cfg.agent_log_bytes,
            voice: cfg.voice.clone(),
            restore: cfg.restore.clone(),
            forward: cfg.forward.clone(),
            from_programs: cfg.from_programs,
            metadata_value_bytes: cfg.metadata_value_bytes,
            cwd_inheritance: cfg.cwd_inheritance,
            fixed_cwd: cfg.fixed_cwd.clone(),
            term: cfg.term.clone(),
            shell: cfg.shell.clone(),
            login_shell: cfg.login_shell,
            // Injected as `PHUX_SOCKET` into every pane.
            server_socket_path: Some(socket_path.to_path_buf()),
            native_host_executable: cfg.native_host_executable.clone(),
            window_size: cfg.window_size,
            keep_awake: cfg.keep_awake,
            policy_engine,
            policy_posture: posture,
            tailnet_owner: cfg.tailnet_owner && posture.requires_workload_mtls(),
            // Only the PTY mode is taken: a `CreateIfMissing` session runs
            // its own wire command, never the pre-seeded session's
            // `seed_command`.
            attach_create_seeds_pty: cfg.seed_with_pty,
            pre_seeded_session: cfg.pre_seeded_session.clone(),
            retain: cfg.retain,
            server_env: std::sync::Arc::new(cfg.env.clone()),
            journal_bounds: JournalBounds::from_config(
                cfg.event_journal_entries,
                cfg.event_journal_bytes,
            ),
            approval_limits: ApprovalLimits {
                // A zero TTL would expire every hold before anyone could
                // see it.
                ttl: Duration::from_secs(u64::from(cfg.approval_ttl_secs.max(1))),
                max_pending: cfg.approval_max_pending,
                max_total: cfg.approval_max_pending_total,
            },
        }
    }

    /// The configured seed name used as `AttachTarget::Last`'s no-touch
    /// fallback. `None` means this server was not configured with a seed.
    #[must_use]
    pub(crate) fn pre_seeded_session(&self) -> Option<&str> {
        self.pre_seeded_session.as_deref()
    }

    /// The UDS path injected into panes as `PHUX_SOCKET`; `None` in
    /// state-only tests.
    #[must_use]
    pub(crate) fn server_socket_path(&self) -> Option<&std::path::Path> {
        self.server_socket_path.as_deref()
    }

    /// [`Self::native_host_executable`], else the running binary's path
    /// (the installed file, after `phux update` replaced it).
    pub(crate) fn native_host_executable(&self) -> Result<std::path::PathBuf, std::io::Error> {
        self.native_host_executable
            .clone()
            .map_or_else(phux_config::instance::running_executable, Ok)
    }

    /// Whether TLS listeners must require a workload client certificate.
    #[must_use]
    pub(crate) const fn workload_mtls_required(&self) -> bool {
        self.policy_posture.requires_workload_mtls()
    }
}
