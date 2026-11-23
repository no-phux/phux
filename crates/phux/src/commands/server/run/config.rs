//! Compose the `ServerConfig` a `phux server` run binds from.

use std::path::Path;
use std::time::Duration;

use phux_server::ServerConfig;

/// Compose the `ServerConfig` the runtime binds from, out of the single
/// config snapshot's `defaults` and the flags that override them.
#[allow(
    clippy::too_many_arguments,
    reason = "one config snapshot's independent sections, threaded through the single startup call"
)]
pub(in crate::commands::server) fn build_server_config(
    session: Option<&str>,
    socket_path: &Path,
    defaults: phux_config::DefaultsCfg,
    voice: phux_config::VoiceCfg,
    limits: &phux_config::LimitsCfg,
    hook_catalog: phux_server::hooks::HookCatalog,
    seed_command: Option<&str>,
    seed_cwd: Option<&Path>,
    exit_after_idle: Option<u64>,
) -> ServerConfig {
    // Resolve the default shell once (`defaults.shell`, `$SHELL`, `/bin/sh`)
    // for every server-owned spawn path.
    let shell = phux_config::shell::resolve_shell(defaults.shell.as_deref());

    // A server started from a unit `phux service install` wrote (marked by
    // `SERVICE_MANAGED_ENV`) never ran a login shell, so its panes need login-shell
    // treatment to see profile `PATH` entries. Absent the marker, a login shell
    // would re-run initialization that is not idempotent for every setup.
    let login_shell = std::env::var_os(crate::commands::service::SERVICE_MANAGED_ENV).is_some();

    // `--seed-command` runs via `<shell> -c` as the seeded session's program.
    let seed_command = seed_command
        .map(|command| phux_terminal_api::launch::shell_command(&shell, command, login_shell));

    let mut cfg = ServerConfig {
        socket_path: socket_path.to_path_buf(),
        native_host_executable: None,
        // `None` under `--no-seed` (ADR-0105).
        pre_seeded_session: session.map(str::to_owned),
        seed_with_pty: true,
        seed_command,
        scrollback: defaults.scrollback_limits(),
        agent_log_bytes: defaults.agent_log_bytes,
        event_journal_entries: defaults.event_journal_entries,
        event_journal_bytes: defaults.event_journal_bytes,
        retain: phux_server::state::RetainPolicy {
            by_default: defaults.retain_on_exit,
            default_secs: defaults.retain_on_exit_secs,
            max_secs: defaults.retain_on_exit_max_secs,
            max_count: defaults
                .retain_on_exit_max
                .min(phux_config::MAX_RETAIN_ON_EXIT_MAX),
        },
        cwd_inheritance: defaults.cwd_inheritance,
        fixed_cwd: defaults
            .fixed_start_dir()
            .map(|dir| dir.to_string_lossy().into_owned()),
        term: defaults.term,
        shell,
        login_shell,
        window_size: defaults.window_size,
        keep_awake: defaults.keep_awake,
        voice,
        restore: phux_config::RestoreCfg::default(),
        forward: phux_config::ForwardCfg::default(),
        from_programs: phux_config::FromPrograms::Off,
        // Clamped up so the built-in agent-session record write always fits under
        // the generic metadata cap, whatever the config says.
        metadata_value_bytes: limits.metadata_value_bytes.max(
            u32::try_from(phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES)
                .unwrap_or(u32::MAX),
        ),
        approval_ttl_secs: defaults
            .approval_ttl_secs
            .min(phux_config::MAX_APPROVAL_TTL_SECS),
        approval_max_pending: defaults
            .approval_max_pending
            .min(phux_config::MAX_APPROVAL_MAX_PENDING),
        approval_max_pending_total: defaults
            .approval_max_pending_total
            .min(phux_config::MAX_APPROVAL_MAX_PENDING_TOTAL),
        policy_engine: None,
        policy_mode: None,
        tailnet_owner: false,
        hook_catalog,
        // Ephemeral lifetime (ADR-0063); absent by default.
        exit_after_idle: exit_after_idle.map(Duration::from_secs),
        // The one place the `PHUX_*` process configuration is read.
        env: phux_server::ServerEnv::from_process(),
        // The durable organization store under the instance's state
        // directory (ADR-0162).
        org_store: phux_server::OrgStoreLocation::for_socket(socket_path),
        // This user's work store, the one `phux mail` uses, for the mail
        // and claims of satellites that joined this hub.
        work_store: crate::commands::mission::client(false).ok().map(Box::new),
        plugin_startup: None,
    };
    if let Some(dir) = seed_cwd {
        start_seed_in(&mut cfg, dir);
    }
    cfg
}

/// Run the seed pane in `dir` (the launching client's cwd) unless
/// `defaults.fixed-start-dir` pins every new pane elsewhere.
fn start_seed_in(cfg: &mut phux_server::ServerConfig, dir: &Path) {
    if cfg.fixed_cwd.is_some() {
        return;
    }
    let mut command = cfg.seed_command.take().unwrap_or_else(|| {
        phux_terminal_api::launch::default_shell_command(&cfg.shell, cfg.login_shell)
    });
    phux_terminal_api::launch::apply_spawn_cwd(&mut command, dir.to_str(), "seed");
    cfg.seed_command = Some(command);
}
