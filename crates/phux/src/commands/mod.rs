use std::path::Path;
use std::process::ExitCode;

use pair::PairAction;
use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult};

mod cli;
mod diagnostics;
mod targets;

pub(crate) use cli::*;
pub(crate) use diagnostics::{print_attach_error, report_no_server};
pub(crate) use targets::{
    parse_selector, report_agent_resolve_error, resolve_target, resolve_target_for_input,
    resolve_targets,
};

/// The verb-scoped `--remote` this command carries, if any.
///
/// One accessor for every verb that takes the flag, so the post-parse checks
/// (a malformed target, the `--socket` collision) cover each of them without
/// a per-verb list of their own.
pub(crate) fn verb_remote(command: &Command) -> Option<&str> {
    match command {
        Command::Attach(AttachArgs { remote, .. }) => remote.as_deref(),
        Command::Ls(LsArgs { remote, .. })
        | Command::Whoami(WhoamiArgs { remote, .. })
        | Command::New(NewArgs { remote, .. })
        | Command::Kill(KillArgs { remote, .. })
        | Command::Rename(RenameArgs { remote, .. })
        | Command::Detach(DetachArgs { remote, .. })
        | Command::Upload(UploadArgs { remote, .. })
        | Command::Download(DownloadArgs { remote, .. })
        | Command::Project(ProjectArgs {
            action: ProjectAction::Link { remote, .. } | ProjectAction::Unlink { remote, .. },
        }) => remote.remote.as_deref(),
        _ => None,
    }
}

pub(crate) mod agent;
pub(crate) mod approvals;
pub(crate) mod archive;
pub(crate) mod ask;
pub(crate) mod attach;
pub(crate) mod bootstrap;
pub(crate) mod channel;
pub(crate) mod completion;
pub(crate) mod config;
pub(crate) mod config_action;
pub(crate) mod confirm;
pub(crate) mod coord;
pub(crate) mod detach;
pub(crate) mod doctor;
pub(crate) mod download;
pub(crate) mod enroll;
pub(crate) mod gen_reference_docs;
pub(crate) mod git;
pub(crate) mod host;
pub(crate) mod idea;
pub(crate) mod json_err;
pub(crate) mod kill;
pub(crate) mod launch;
pub(crate) mod logs;
pub(crate) mod ls;
pub(crate) mod mcp;
pub(crate) mod mission;
pub(crate) mod mission_project;
pub(crate) mod mission_start;
pub(crate) mod new;
pub(crate) mod org;
pub(crate) mod organize;
pub(crate) mod pair;
pub(crate) mod partial;
pub(crate) mod paste;
pub(crate) mod perf;
pub(crate) mod perf_desktop;
pub(crate) mod plan_usage;
pub(crate) mod play;
pub(crate) mod plugin;
pub(crate) mod project;
pub(crate) mod rec;
pub(crate) mod relay;
pub(crate) mod remote;
pub(crate) mod remote_target;
pub(crate) mod rename;
pub(crate) mod report;
pub(crate) mod resize;
pub(crate) mod resource;
pub(crate) mod run;
pub(crate) mod runtime_info;
pub(crate) mod satellite;
pub(crate) mod send_keys;
pub(crate) mod server;
pub(crate) mod server_target;
pub(crate) mod service;
pub(crate) mod snapshot;
pub(crate) mod spatial;
pub(crate) mod spawn;
pub(crate) mod ssh_bootstrap;
pub(crate) mod status;
pub(crate) mod stdio_bridge;
pub(crate) mod supervise;
pub(crate) mod tag;
pub(crate) mod theme;
pub(crate) mod toml_registry;
pub(crate) mod update;
pub(crate) mod upgrade;
pub(crate) mod upload;
pub(crate) mod wait;
// Stalled peers for the run/wait deadline tests.
#[cfg(test)]
mod stall_peer;
pub(crate) mod watch;
pub(crate) mod whoami;
pub(crate) mod workload;
pub(crate) mod worktree;

/// Default name the `phux server` subcommand pre-seeds, and the name
/// the `phux attach` auto-spawn path requests when the user doesn't
/// provide one. Keeping both halves on a single constant means
/// "`phux` with no arguments after a fresh boot" Just Works.
pub(crate) const DEFAULT_SESSION_NAME: &str = "default";

/// The verb's display name when the resolved (sub)command never dials a
/// server socket, or `None` when it consumes the root `--socket` global.
///
/// `--socket` is declared once, `global`, on the root `Cli`
/// (ADR-0065), so clap accepts it on every invocation path — including the
/// verbs that are pure local operations (config scaffolding, registry
/// edits, completions). Those must refuse a provided `--socket` rather
/// than silently ignore it: a user who typed `phux pair --socket X`
/// believes the flag did something. `main` turns a `Some` from here into
/// a one-line teaching error.
#[allow(
    clippy::match_same_arms,
    reason = "one arm per verb, grouped by namespace; merging arms across namespaces would hide which verbs are listed"
)]
pub(crate) const fn socketless_verb(command: &Command) -> Option<&'static str> {
    match command {
        Command::Agent(AgentArgs { action }) => match action {
            agent::AgentAction::InstallClaude { .. } => Some("agent install-claude"),
            agent::AgentAction::UninstallClaude => Some("agent uninstall-claude"),
            agent::AgentAction::HookPayload => Some("agent hook-payload"),
            agent::AgentAction::HookTranscript => Some("agent hook-transcript"),
            _ => None,
        },
        Command::Config(ConfigArgs { action }) => match action {
            config_action::ConfigAction::Init { .. } => Some("config init"),
            config_action::ConfigAction::Path => Some("config path"),
            config_action::ConfigAction::Check { .. } => Some("config check"),
            config_action::ConfigAction::Show { .. } => Some("config show"),
            config_action::ConfigAction::Keys { .. } => Some("config keys"),
            config_action::ConfigAction::Plugins { .. } => Some("config plugins"),
            config_action::ConfigAction::Run { .. } => Some("config run"),
            // `agents` best-effort reads live state; `reload` rings the
            // server doorbell.
            config_action::ConfigAction::Agents { .. } | config_action::ConfigAction::Reload => {
                None
            }
        },
        Command::Archive(ArchiveArgs { action }) => match action {
            ArchiveAction::Import { plan: true, .. } => Some("archive import --plan"),
            ArchiveAction::Import { .. } | ArchiveAction::Export { .. } => None,
        },
        Command::Service(ServiceArgs { action }) => match action {
            // `install` bakes the socket into the generated unit.
            ServiceAction::Install { .. } => None,
            // `reconcile` reads the socket out of the unit it is reconciling —
            // the only socket whose liveness is relevant — so a `--socket` on
            // the command line would be silently ignored.
            ServiceAction::Reconcile { .. } => Some("service reconcile"),
            ServiceAction::Uninstall => Some("service uninstall"),
            ServiceAction::Status => Some("service status"),
            ServiceAction::Logs { .. } => Some("service logs"),
            ServiceAction::PruneLogs { .. } => Some("service prune-logs"),
        },
        Command::Plugin(..) => Some("plugin"),
        // `set` rings the reload doorbell after editing the file.
        Command::Theme(ThemeArgs { action }) => match action {
            ThemeAction::Set { .. } => None,
            ThemeAction::List { .. }
            | ThemeAction::Show { .. }
            | ThemeAction::Install { .. }
            | ThemeAction::Remove { .. } => Some("theme"),
        },
        Command::Host(..) => Some("host"),
        Command::Relay(..) => Some("relay"),
        // Minting asks the server what it has bound (ADR-0141); the
        // lifecycle actions only edit the store.
        Command::Pair(PairArgs { action, .. }) => match action {
            None => None,
            Some(PairAction::Ls) => Some("pair ls"),
            Some(PairAction::Prune { .. }) => Some("pair prune"),
            Some(PairAction::Rotate { .. }) => Some("pair rotate"),
            Some(PairAction::Revoke { .. }) => Some("pair revoke"),
        },
        Command::Project(ProjectArgs { action }) => match action {
            ProjectAction::Open { .. }
            | ProjectAction::List { .. }
            | ProjectAction::Link { .. }
            | ProjectAction::Unlink { .. } => None,
            ProjectAction::Init { .. } => Some("project init"),
            ProjectAction::Trust { .. } => Some("project trust"),
            ProjectAction::Untrust { .. } => Some("project untrust"),
            ProjectAction::Status { .. } => Some("project status"),
            ProjectAction::Inspect { .. } => Some("project inspect"),
        },
        Command::Workload(..) => Some("workload"),
        Command::Completion(..) => Some("completion"),
        Command::Mcp(..) => Some("mcp"),
        Command::Skill(..) => Some("skill"),
        Command::Logs(..) => Some("logs"),
        Command::RuntimeInfo(..) => Some("runtime-info"),
        Command::Report(..) => Some("report"),
        Command::GenReferenceDocs(..) => Some("gen-reference-docs"),
        Command::Claim(..) => Some("claim"),
        Command::Mail(MailArgs { action }) => match action {
            coord::MailAction::Send { .. } => Some("mail send"),
            coord::MailAction::Read { .. } => Some("mail read"),
            coord::MailAction::Ack { .. } => Some("mail ack"),
            coord::MailAction::Wait { .. } => Some("mail wait"),
            coord::MailAction::HookContext { .. } => Some("mail hook-context"),
            coord::MailAction::Release { .. } => Some("mail release"),
        },
        // Every verb starts the keeper, which watches this server, and
        // `start` / `add --start` dial it. `--socket` names that server.
        Command::Mission(..) => None,
        // Capture talks to the ideas endpoint, never the phux server socket.
        Command::Idea { .. } => Some("idea"),
        _ => None,
    }
}

/// Quote one word for a POSIX shell, unconditionally (`'` becomes `'\''`).
/// No "looks safe" fast path: paths and native session ids may carry
/// spaces, `$(...)`, or `;`. [`ssh_bootstrap::shell_quote`] is the variant
/// that leaves an already-safe word bare.
pub(crate) fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Fail fast when `socket_path` cannot fit in a `sockaddr_un` (phux-iwuc).
///
/// A too-long path can never bind or connect, so naming the platform's
/// UDS path-length limit here beats the downstream misdirection (a 2s
/// auto-spawn timeout, or a raw "path must be shorter than `SUN_LEN`").
/// Prints the diagnostic and returns the failure exit code to bubble.
pub(crate) fn ensure_socket_path_fits(socket_path: &Path) -> Result<(), ExitCode> {
    phux_server::runtime::validate_socket_path_len(socket_path).map_err(|err| {
        error!("{err}");
        ExitCode::FAILURE
    })
}

/// Build a current-thread tokio runtime, or print why and return the
/// failure exit code.
pub(crate) fn cli_runtime() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            error!("failed to build runtime: {err}");
            ExitCode::FAILURE
        })
}

/// Send one command over `conn` and return the matching `COMMAND_RESULT`,
/// reporting anything the server interleaved ahead of it (SPEC §5).
///
/// The CLI is a request/response consumer: it opens a connection, runs a
/// verb, and exits without attaching or subscribing, so the only frames that
/// can precede an ack here are the ones a handler emits itself. In practice
/// that is the hub's federation degradation notice — one uncorrelated `ERROR`
/// per unreachable satellite, pushed ahead of the merged `GET_STATE` snapshot
/// by `handle_get_state_federated` precisely so the caller can say the view is
/// partial. The loop this replaced discarded them, which turned every
/// federated CLI verb into a confident report of an incomplete fleet.
///
/// Unlike the library paths, this one owns stderr, so it prints rather than
/// logging into a `tracing` subscriber a CLI user has not installed. Any
/// *other* interleaved frame is dropped: on a connection that never attached
/// or subscribed there is no consumer for a `RESOURCE_OUTPUT` or an `EVENT`,
/// and no verb here can act on one.
pub(crate) async fn command_on(
    conn: &mut Connection,
    request_id: u32,
    command: WireCommand,
) -> Result<CommandResult, AttachError> {
    let (result, interleaved) = conn.request(request_id, command).await?.into_parts();
    warn_interleaved_degradation(&phux_client::state::Degradation::from_interleaved(
        &interleaved,
    ));
    Ok(result)
}

/// Print `degradation`'s notices exactly as [`command_on`] always has.
///
/// A `phux-client` function that returns its own `Degradation` (the
/// `kill`/`detach`/`spawn`/`tags` library homes) uses this instead of
/// `command_on`'s inline loop, so every verb prints the identical line for
/// an uncorrelated `ERROR` interleaved ahead of its reply — a hub's
/// per-satellite unreachability notice — regardless of which path fetched
/// it.
pub(crate) fn warn_interleaved_degradation(degradation: &phux_client::state::Degradation) {
    for message in degradation.notices() {
        warn!("partial results — {message}");
    }
}

/// One-shot: open a fresh connection, send `command`, return its result.
pub(crate) async fn request_command(
    socket_path: &Path,
    command: WireCommand,
) -> Result<CommandResult, AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    command_on(&mut conn, 1, command).await
}

#[cfg(test)]
mod tests;
