//! `phux ls --all`: probing every registered host and rendering the
//! machine-grouped listing.

use std::collections::HashMap;
use std::process::ExitCode;

use phux_client::agent_meta::AgentRecord;
use phux_client::glance::Sources;
use phux_client::resource;
use phux_client::state::StateView;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::SessionSnapshot;

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::partial;
use crate::commands::server_target::ServerTarget;
use phux_core::host_list::{HostJson, HostKind, HostListJson};

use super::glance_rows::{RowProject, collapse_by_project, glance_lines, glance_of_row};
use super::lines::print_sessions_json;
use super::session_view::print_one_session;
use super::{Depth, print_lines, print_sessions};

/// How long `phux ls --all` waits on any one host. A tailnet peer answers
/// in well under a second; one that has not answered by now is reported as
/// unreachable rather than holding up every other machine's rows.
pub(super) const HOST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// `phux ls --all`: this machine and every registered host, grouped by
/// machine (`phux.hosts/v1` with `json`).
///
/// Every host is queried at once and each gets [`HOST_DEADLINE`], so the
/// listing costs one slow host's deadline at worst, not the sum. A host that
/// cannot be planned, dialed, or listed keeps its row with the reason. The
/// exit status is 0 whenever the listing itself could be produced: "mini is
/// down" is a true answer about mini, not a failure of `ls`.
pub(crate) fn run_ls_all(json: bool, socket: Option<std::path::PathBuf>) -> ExitCode {
    let rt = match super::super::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let registry = match super::super::remote::load_registry() {
        Ok(entries) => entries,
        Err(err) => {
            let err = CliError::new(
                codes::REMOTE_UNRESOLVED,
                format!("could not read the host registry: {err}"),
                "fix the config (`phux config check`), then retry",
            );
            return json_err::emit(json, &err, 1);
        }
    };
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let mut probes: Vec<HostProbe> = vec![HostProbe {
        row: host_row("local", &local_hostname(), HostKind::Local, None),
        target: Ok(ServerTarget::local(&socket)),
    }];
    for entry in &registry {
        probes.push(HostProbe {
            row: host_row(
                &entry.name,
                &entry.name,
                HostKind::Remote,
                Some(entry.endpoint.clone()),
            ),
            target: super::super::server_target::plan_entry(&rt, entry, "ls")
                .map(|remote| ServerTarget::Remote(Box::new(remote)))
                .map_err(|err| err.message),
        });
    }
    // Client connections are `!Send` (current-thread runtime, ADR-0003), so
    // the probes run concurrently as local tasks rather than on a pool.
    let local = tokio::task::LocalSet::new();
    let hosts = local.block_on(&rt, async {
        let handles: Vec<_> = probes
            .into_iter()
            .map(|probe| tokio::task::spawn_local(probe.run()))
            .collect();
        let mut rows = Vec::with_capacity(handles.len());
        for handle in handles {
            if let Ok(row) = handle.await {
                rows.push(row);
            }
        }
        rows
    });
    let doc = HostListJson::new(hosts);
    if json {
        return match serde_json::to_string_pretty(&doc) {
            Ok(text) => {
                outln!("{text}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                error!("failed to serialize host list as JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    for line in host_list_lines(&doc) {
        outln!("{line}");
    }
    ExitCode::SUCCESS
}

/// One machine to list: its row, pre-filled, and how to reach it (or why it
/// cannot be reached before a dial is even tried).
pub(super) struct HostProbe {
    pub(super) row: HostJson,
    pub(super) target: Result<ServerTarget, String>,
}

impl HostProbe {
    pub(super) async fn run(self) -> HostJson {
        let Self { mut row, target } = self;
        let target = match target {
            Ok(target) => target,
            Err(err) => {
                row.error = Some(err);
                return row;
            }
        };
        let listing = async {
            let mut conn = target.connect().await?;
            phux_client::session_list::listing_view(&mut conn).await
        };
        match tokio::time::timeout(HOST_DEADLINE, listing).await {
            Ok(Ok(view)) => {
                let organization = view.organization().cloned();
                let (snapshot, _) = view.into_parts();
                let mut sessions: Vec<_> = snapshot
                    .sessions
                    .iter()
                    .map(|s| {
                        phux_client::session_list::session_json(&snapshot, organization.as_ref(), s)
                    })
                    .collect();
                sessions.sort_by(|a, b| a.name.cmp(&b.name));
                row.reachable = true;
                row.sessions = sessions;
            }
            Ok(Err(err)) => row.error = Some(unreachable_reason(&err)),
            Err(_) => {
                row.error = Some(format!(
                    "did not answer within {}s",
                    HOST_DEADLINE.as_secs()
                ));
            }
        }
        row
    }
}

/// Why a host did not list, in the words `phux ls` uses: a socket with no
/// server behind it is "no server running", not an attach-loop I/O error.
pub(super) fn unreachable_reason(err: &phux_client::attach::AttachError) -> String {
    match err {
        phux_client::attach::AttachError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            "no server running".to_owned()
        }
        other => other.to_string(),
    }
}

pub(super) fn host_row(
    name: &str,
    label: &str,
    kind: HostKind,
    endpoint: Option<String>,
) -> HostJson {
    HostJson {
        name: name.to_owned(),
        label: label.to_owned(),
        kind,
        endpoint,
        reachable: false,
        error: None,
        sessions: Vec::new(),
    }
}

/// This machine's short hostname (`mac.local` becomes `mac`), or `local`.
pub(crate) fn local_hostname() -> String {
    phux_config::instance::local_host_name()
}

/// The human `--all` listing: one header per machine, its sessions' glance
/// lines indented beneath, and an unreachable machine's reason in place of
/// its sessions.
pub(super) fn host_list_lines(doc: &HostListJson) -> Vec<String> {
    let mut lines = Vec::new();
    for host in &doc.hosts {
        let header = match host.kind {
            HostKind::Local => format!("{} (this machine)", host.label),
            HostKind::Remote => host.label.clone(),
        };
        if !host.reachable {
            let why = host.error.as_deref().unwrap_or("unreachable");
            lines.push(format!("{header}  unreachable: {why}"));
            continue;
        }
        lines.push(header);
        if host.sessions.is_empty() {
            lines.push("  (no sessions)".to_owned());
        }
        let rows = host
            .sessions
            .iter()
            .map(|row| {
                let project = RowProject {
                    identity: row.project.clone(),
                    link: row.project_link.clone(),
                };
                (glance_of_row(row), project)
            })
            .collect();
        lines.extend(
            collapse_by_project(rows)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }
    lines
}

/// Print one fetched listing in the requested shape.
pub(super) fn render_listing(
    json: bool,
    depth: &Depth,
    view: StateView,
    agents: &HashMap<ResourceId, AgentRecord>,
) -> ExitCode {
    let hosts_complete = view.host_sessions_complete();
    let organization = view.organization().cloned();
    let (snapshot, degradation) = view.into_parts();
    if json {
        // Not stderr: a `--json` consumer's channel is the document.
        return print_sessions_json(
            &snapshot,
            organization.as_ref(),
            &degradation,
            hosts_complete,
        );
    }
    let sources = Sources {
        snapshot: &snapshot,
        organization: organization.as_ref(),
        agents,
    };
    if let Some(name) = depth.session.as_deref() {
        let code = print_one_session(sources, name, depth.detail);
        partial::warn_partial_view("ls", &degradation);
        return code;
    }
    if depth.detail {
        print_sessions(&snapshot, organization.as_ref());
    } else {
        print_lines(glance_lines(sources));
    }
    if !hosts_complete && has_satellite_terminals(&snapshot) {
        warn!("{HOST_SESSIONS_UNAVAILABLE}");
    }
    partial::warn_partial_view("ls", &degradation);
    ExitCode::SUCCESS
}

/// The text-mode note for a hub that does not report its satellites'
/// sessions (no `HOST_SESSIONS`): the satellite Terminals it does list are
/// real, but which sessions they belong to cannot be known from here.
pub(super) const HOST_SESSIONS_UNAVAILABLE: &str = "ls: satellite sessions are unavailable from this \
     server (it does not report HOST_SESSIONS); its satellite terminals are listed above";

pub(super) fn has_satellite_terminals(snapshot: &SessionSnapshot) -> bool {
    resource::terminals(snapshot).any(|pane| pane.id.host().is_some())
}
