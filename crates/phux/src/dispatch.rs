//! The verb table: one arm per subcommand, each handing its parsed
//! arguments to the verb's entry point.

use std::process::ExitCode;

use crate::commands::{self, Command};
use crate::skill;

/// The verb table: one arm per subcommand. `socket` is the shared root
/// global; `root_rec` and `root_remote` are read only by the naked (`None`)
/// arm.
#[allow(
    clippy::too_many_lines,
    reason = "one match arm per CLI subcommand; the dispatch is a flat verb table, clearer whole than split."
)]
pub(crate) fn dispatch(
    command: Option<Command>,
    socket: Option<std::path::PathBuf>,
    root_rec: &commands::RecOpts,
    root_remote: Option<String>,
) -> ExitCode {
    match command {
        Some(Command::Attach(args)) => commands::attach_cli::run(args, socket),
        Some(Command::Native(args)) => commands::native_terminal::run(args, socket),
        Some(Command::Server(args)) => commands::server::run(args, socket),
        Some(Command::Ls(args)) => commands::ls::run(args, socket),
        Some(Command::Whoami(args)) => commands::whoami::run(args, socket),
        Some(Command::Version(args)) => commands::version::run(args, socket),
        Some(Command::Status(args)) => commands::status::run(args, socket),
        Some(Command::RuntimeInfo(args)) => commands::runtime_info::run(args.json.json),
        Some(Command::Perf(args)) => commands::perf::run(args, socket),
        Some(Command::New(args)) => commands::new::run(args, socket),
        Some(Command::Spawn(args)) => commands::spawn::run(args, socket),
        Some(Command::Launch(args)) => commands::launch::run(args, socket),
        Some(Command::Kill(args)) => commands::kill::run(args, socket),
        Some(Command::Detach(args)) => commands::detach::run(args, socket),
        Some(Command::InsertPane(args)) => commands::spatial::insert_pane(args, socket),
        Some(Command::Mv(args)) => commands::spatial::mv(args, socket),
        Some(Command::Rm(args)) => commands::org::run_rm(&args.target, args.yes, args.json, socket),
        Some(Command::Relaunch(args)) => {
            commands::org::run_relaunch(&args.target, args.json, socket)
        }
        Some(Command::Restore(args)) => {
            commands::org::run_restore(args.session.as_deref(), args.verbose, args.json, socket)
        }
        Some(Command::Organize(args)) => commands::organize::run(args, socket),
        Some(Command::SwapPane(args)) => commands::spatial::swap_pane(args, socket),
        Some(Command::Slot(args)) => commands::spatial::slot(args, socket),
        Some(Command::Resize(args)) => commands::resize::run(args, socket),
        Some(Command::Take(args)) => commands::supervise::take(args, socket),
        Some(Command::Give(args)) => commands::supervise::give(args, socket),
        Some(Command::Signal(args)) => commands::supervise::signal(args, socket),
        Some(Command::Notify(commands::NotifyArgs {
            title,
            body,
            level,
            pane,
            json,
        })) => commands::agent::run_notify(
            &title,
            body.as_deref(),
            level,
            pane.as_deref(),
            json.json,
            socket,
        ),
        Some(Command::NotifyMode(commands::NotifyModeArgs { target, mode, json })) => {
            commands::agent::run_notify_mode(&target, mode, json.json, socket)
        }
        Some(Command::Seen(commands::SeenArgs { target, json })) => {
            commands::agent::run_seen(&target, json.json, socket)
        }
        Some(Command::Approvals(args)) => commands::approvals::run(args, socket),
        Some(Command::Credentials(args)) => commands::credentials::run(args, socket),
        Some(Command::Approve(args)) => commands::approvals::approve(args, socket),
        Some(Command::Deny(args)) => commands::approvals::deny(args, socket),
        Some(Command::Update(args)) => commands::update::run(args, socket),
        Some(Command::Channel(args)) => commands::channel::run(args, socket),
        Some(Command::Upgrade(args)) => {
            commands::upgrade::run_upgrade(socket, args.close_agent_sessions)
        }
        Some(Command::Rename(args)) => commands::rename::run(args, socket),
        Some(Command::Lock(args)) => commands::org::run_lock(
            &args.target,
            true,
            args.json.json,
            args.remote.with_socket(socket),
        ),
        Some(Command::Unlock(args)) => commands::org::run_lock(
            &args.target,
            false,
            args.json.json,
            args.remote.with_socket(socket),
        ),
        Some(Command::Snapshot(args)) => commands::snapshot::run(args, socket),
        Some(Command::SendKeys(args)) => commands::send_keys::run(args, socket),
        Some(Command::Upload(args)) => commands::upload::run(args, socket),
        Some(Command::Download(args)) => commands::download::run(args, socket),
        Some(Command::Git(args)) => commands::git_checkout::run(args, socket),
        Some(Command::Forward(args)) => commands::forward::run(args, socket),
        Some(Command::Paste(args)) => commands::paste::run(args, socket),
        Some(Command::Wait(args)) => commands::wait::run(args, socket),
        Some(Command::Watch(args)) => commands::watch::run(args, socket),
        Some(Command::On(args)) => commands::watch::on::run(&args, socket),
        Some(Command::Resource(args)) => commands::resource::run(args, socket),
        Some(Command::Terminal(args)) => commands::terminal::run(args, socket),
        Some(Command::Api(args)) => commands::public_api::run(args, socket),
        Some(Command::Rec(args)) => commands::rec::run(args, socket),
        Some(Command::Play(args)) => commands::play::run(args, socket),
        Some(Command::Ask(args)) => commands::ask::run(args, socket),
        Some(Command::Pick(args)) => commands::pick::run(args),
        Some(Command::Agent(args)) => commands::agent::run(args, socket),
        Some(Command::Run(args)) => commands::run::run(args, socket),
        Some(Command::Config(args)) => commands::config::run_config(&args.action, socket),
        Some(Command::Migrate(args)) => commands::migrate::run(args, socket),
        Some(Command::Plugin(args)) => commands::plugin::run(args),
        Some(Command::Theme(args)) => commands::theme::run(args, socket),
        Some(Command::Omarchy(args)) => commands::omarchy::run_omarchy(&args.action, socket),
        Some(Command::Archive(args)) => commands::archive::run_archive(&args.action, socket),
        Some(Command::Tag(args)) => commands::tag::run(args, socket),
        Some(Command::StdioBridge(_)) => commands::stdio_bridge::run_stdio_bridge(socket),
        Some(Command::Bootstrap(args)) => commands::bootstrap::run(args, socket),
        Some(Command::Relay(args)) => commands::relay::run(args),
        Some(Command::Pair(args)) => commands::pair::run(args, socket),
        Some(Command::Workload(args)) => commands::workload::run(args),
        Some(Command::Completion(args)) => commands::completion::run(args),
        Some(Command::Man(_)) => commands::man::run(),
        // Returned above, before process-global setup.
        Some(Command::Mcp(..)) => ExitCode::FAILURE,
        Some(Command::Skill(args)) => skill::dispatch(args.action, args.scope, socket),
        Some(Command::Mission(args)) => commands::mission::run(args, socket),
        Some(Command::Mail(args)) => commands::coord::mail(args),
        Some(Command::Chat(args)) => commands::chat::run(args, socket),
        Some(Command::Claim(args)) => commands::coord::claim(args),
        Some(Command::Worktree(args)) => commands::worktree::run(args, socket),
        Some(Command::Project(args)) => commands::project::run_project(&args.action, socket),
        Some(Command::Idea(args)) => commands::idea::run(args),
        Some(Command::Doctor(args)) => commands::doctor::run(args, socket),
        Some(Command::Logs(args)) => commands::logs::run(args),
        Some(Command::Audit(args)) => commands::audit::run(args),
        Some(Command::Report(args)) => commands::report::run(args),
        Some(Command::Host(args)) => commands::host::run(args),
        Some(Command::Service(args)) => commands::service::run(args, socket),
        Some(Command::GenReferenceDocs(args)) => commands::gen_reference_docs::run(args),
        None => commands::attach_cli::run_naked_invocation(root_rec, root_remote, socket),
    }
}
