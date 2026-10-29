//! The verb table: one arm per subcommand, each handing its parsed
//! arguments to the verb's entry point.

use std::process::ExitCode;

use crate::commands::{self, Command};
use crate::skill;

use self::attach::run_naked_invocation;
use self::machines::run_service;

pub(crate) mod attach;
mod machines;
mod panes;
mod recording;
mod sessions;

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
        Some(Command::Attach(args)) => attach::attach(args, socket),
        Some(Command::Server(args)) => machines::server(args, socket),
        Some(Command::Ls(args)) => sessions::ls(args, socket),
        Some(Command::Whoami(commands::WhoamiArgs { json, remote })) => {
            commands::whoami::run_whoami(json.json, remote.with_socket(socket))
        }
        Some(Command::Status(commands::StatusArgs { json })) => {
            commands::status::run_status(json.json, socket)
        }
        Some(Command::RuntimeInfo(commands::RuntimeInfoArgs { json })) => {
            commands::runtime_info::run(json.json)
        }
        Some(Command::Perf(args)) => machines::perf(args, socket),
        Some(Command::New(args)) => sessions::new(args, socket),
        Some(Command::Spawn(args)) => panes::spawn(args, socket),
        Some(Command::Launch(args)) => panes::launch(args, socket),
        Some(Command::Kill(args)) => sessions::kill(args, socket),
        Some(Command::Detach(commands::DetachArgs {
            session,
            yes,
            json,
            remote,
        })) => commands::detach::run_detach(session, yes, json.json, remote.with_socket(socket)),
        Some(Command::InsertPane(args)) => panes::insert_pane(args, socket),
        Some(Command::Mv(args)) => panes::mv(args, socket),
        Some(Command::Rm(commands::RmArgs { target, yes, json })) => {
            commands::org::run_rm(&target, yes, json, socket)
        }
        Some(Command::Relaunch(commands::RelaunchArgs { target, json })) => {
            commands::org::run_relaunch(&target, json, socket)
        }
        Some(Command::Restore(commands::RestoreArgs {
            session,
            verbose,
            json,
        })) => commands::org::run_restore(session.as_deref(), verbose, json, socket),
        Some(Command::Organize(args)) => sessions::organize(args, socket),
        Some(Command::SwapPane(args)) => panes::swap_pane(args, socket),
        Some(Command::Resize(commands::ResizeArgs {
            target,
            geometry,
            json,
        })) => commands::resize::run_resize(&target, geometry, json.json, socket),
        Some(Command::Take(commands::TakeArgs { target, ttl, json })) => {
            commands::supervise::run_take(&target, ttl, json.json, socket)
        }
        Some(Command::Give(commands::GiveArgs { target, json })) => {
            commands::supervise::run_give(&target, json.json, socket)
        }
        Some(Command::Signal(args)) => panes::signal(args, socket),
        Some(Command::Approvals(commands::ApprovalsArgs { json })) => {
            commands::approvals::run_approvals(json.json, socket)
        }
        Some(Command::Approve(commands::ApproveArgs { id, yes, json })) => {
            commands::approvals::run_approve(&id, yes, json.json, socket)
        }
        Some(Command::Deny(commands::DenyArgs { id, json })) => {
            commands::approvals::run_deny(&id, json.json, socket)
        }
        Some(Command::Update(commands::UpdateArgs { opts })) => {
            commands::update::run_update(&opts, socket)
        }
        Some(Command::Channel(commands::ChannelArgs { channel, json })) => {
            commands::channel::run(channel, json.json, socket)
        }
        Some(Command::Upgrade(commands::UpgradeArgs {
            close_agent_sessions,
        })) => commands::upgrade::run_upgrade(socket, close_agent_sessions),
        Some(Command::Rename(commands::RenameArgs {
            target,
            new_name,
            json,
            remote,
        })) => {
            commands::rename::run_rename(&target, &new_name, json.json, remote.with_socket(socket))
        }
        Some(Command::Snapshot(args)) => panes::snapshot(args, socket),
        Some(Command::SendKeys(commands::SendKeysArgs { target, keys, json })) => {
            commands::send_keys::run_send_keys(&target, &keys, json.json, socket)
        }
        Some(Command::Upload(args)) => panes::upload(args, socket),
        Some(Command::Download(args)) => machines::download(args, socket),
        Some(Command::Paste(commands::PasteArgs {
            target,
            text,
            untrusted,
            json,
        })) => commands::paste::run_paste(&target, text, untrusted, json.json, socket),
        Some(Command::Wait(args)) => panes::wait(args, socket),
        Some(Command::Watch(args)) => panes::watch(args, socket),
        Some(Command::Resource(commands::ResourceArgs { action })) => {
            commands::resource::run_resource(&action, socket)
        }
        Some(Command::Terminal(commands::TerminalArgs { action })) => {
            commands::terminal::run(&action, socket)
        }
        Some(Command::Rec(args)) => recording::rec(args, socket),
        Some(Command::Play {
            file,
            target,
            speed,
            idle_limit,
            loops,
            split,
            ratio,
            no_fit,
            close,
            json,
            pty_writer,
        }) => commands::play::run_play(&commands::play::PlayArgs {
            file: &file,
            target: target.as_deref(),
            speed: speed.0,
            idle_limit,
            // The CLI spells "repeat forever" as `--loop` with no value,
            // which clap fills in as 0; `passes` carries that as `None` so
            // the player's loop condition is a plain "count remaining".
            passes: match loops {
                None => Some(1),
                Some(0) => None,
                Some(n) => Some(n),
            },
            split,
            ratio,
            no_fit,
            close,
            json: json.json,
            socket,
            pty_writer,
        }),
        Some(Command::Ask(args)) => panes::ask(args, socket),
        Some(Command::Agent(commands::AgentArgs { action })) => {
            commands::agent::run_agent(&action, socket)
        }
        Some(Command::Run(args)) => panes::run(args, socket),
        Some(Command::Config(commands::ConfigArgs { action })) => {
            commands::config::run_config(&action, socket)
        }
        Some(Command::Plugin(commands::PluginArgs { action })) => {
            commands::plugin::run_plugin(&action)
        }
        Some(Command::Theme(commands::ThemeArgs { action })) => {
            commands::theme::run_theme(&action, socket)
        }
        Some(Command::Omarchy(commands::OmarchyArgs { action })) => {
            commands::omarchy::run_omarchy(&action, socket)
        }
        Some(Command::Archive(commands::ArchiveArgs { action })) => {
            commands::archive::run_archive(&action, socket)
        }
        Some(Command::Tag(commands::TagArgs { action })) => commands::tag::run_tag(&action, socket),
        Some(Command::StdioBridge {}) => commands::stdio_bridge::run_stdio_bridge(socket),
        Some(Command::Bootstrap(args)) => machines::bootstrap(args, socket),
        Some(Command::Relay(commands::RelayArgs { action })) => commands::relay::run_relay(action),
        Some(Command::Pair(args)) => machines::pair(args, socket),
        Some(Command::Workload(commands::WorkloadArgs { action, json })) => {
            commands::workload::run(action, json)
        }
        Some(Command::Completion(commands::CompletionArgs { shell })) => {
            commands::completion::run_completion(shell.into())
        }
        // Returned above, before process-global setup.
        Some(Command::Mcp(..)) => ExitCode::FAILURE,
        Some(Command::Skill(commands::SkillArgs { scope })) => skill::run(scope),
        Some(Command::Mission(commands::MissionArgs { action })) => {
            commands::mission::run(action, socket)
        }
        Some(Command::Mail(commands::MailArgs { action })) => commands::coord::run_mail(&action),
        Some(Command::Claim(commands::ClaimArgs { action })) => commands::coord::run_claim(&action),
        Some(Command::Worktree(commands::WorktreeArgs { action })) => {
            commands::worktree::run_worktree(&action, socket)
        }
        Some(Command::Project(commands::ProjectArgs { action })) => {
            commands::project::run_project(&action, socket)
        }
        Some(Command::Idea {
            private,
            flush,
            text,
        }) => commands::idea::run(private, flush, text.as_deref()),
        Some(Command::Doctor(commands::DoctorArgs { json })) => {
            commands::doctor::run_doctor(json, socket)
        }
        Some(Command::Logs(commands::LogsArgs {
            server,
            client,
            pid,
            follow,
            lines,
            json,
        })) => commands::logs::run_logs(server, client, pid, follow, lines, json),
        Some(Command::Report(commands::ReportArgs { action, json })) => {
            commands::report::run_report(action, json.json)
        }
        Some(Command::Host(commands::HostArgs { action })) => commands::host::run_host(&action),
        Some(Command::Service(commands::ServiceArgs { action })) => run_service(action, socket),
        Some(Command::GenReferenceDocs(commands::GenReferenceDocsArgs { out })) => {
            commands::gen_reference_docs::run_gen_reference_docs(out)
        }
        None => run_naked_invocation(root_rec, root_remote, socket),
    }
}
