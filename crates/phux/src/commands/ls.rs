use std::collections::HashMap;
use std::process::ExitCode;

use phux_client::org::OrgInventory;
use phux_client::resource;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::SessionSnapshot;

use crate::commands::server_target::ServerSpec;

mod all_hosts;
mod glance_rows;
mod lines;
mod session_view;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_glance;

use all_hosts::render_listing;
pub(crate) use all_hosts::{local_hostname, run_ls_all};
use glance_rows::with_hosts;
pub(crate) use lines::format_session_line;
use lines::local_session_lines;

/// How deep `phux ls` reads (`docs/reference/cli.md#phux-ls`): the glance by
/// default, one session's terminals with `SESSION`, and keys, revisions, and
/// runs with `-l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Depth {
    /// List this session's terminals instead of every session.
    pub session: Option<String>,
    /// Add keys, lifetimes, windows, and runs; never collapse.
    pub detail: bool,
}

impl Depth {
    /// The depth `phux ls [SESSION] [-l]` asks for.
    pub(crate) const fn of(session: Option<String>, detail: bool) -> Self {
        Self { session, detail }
    }

    /// Whether the text needs each terminal's agent record. The detail
    /// listing of every session prints none, so it skips the reads.
    const fn reads_agents(&self) -> bool {
        self.session.is_some() || !self.detail
    }
}

/// First request id for the agent-record reads, clear of the listing's
/// `GET_STATE`, layout, and organization reads.
const AGENT_READ_FIRST_REQUEST_ID: u32 = 0x8000_0000;

/// `phux ls` — list sessions via `GET_STATE`; never auto-starts a server.
/// `--json` emits the stable [`SessionListJson`] (ADR-0022), complete at
/// every depth. A partial listing from a degraded hub still exits 0,
/// reporting the gap on stderr or in the `unreachable` list (see
/// [`partial`]).
pub(crate) fn run_ls(json: bool, depth: &Depth, server: ServerSpec) -> ExitCode {
    let (rt, target) = match server.prepare("ls", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let listing = rt.block_on(async {
        let mut conn = target.connect().await?;
        // The glance prints no window counts, so it skips their reads.
        let view = if json || depth.detail {
            phux_client::session_list::listing_view(&mut conn).await?
        } else {
            phux_client::session_list::glance_view(&mut conn).await?
        };
        let agents = if json || !depth.reads_agents() {
            HashMap::new()
        } else {
            let panes = agent_panes(view.snapshot(), depth.session.as_deref());
            // The listing already warns about a partial view; a record a
            // degraded hub could not read only leaves its agent unnamed.
            phux_client::agent_record::fetch_index_on(
                &mut conn,
                &panes,
                AGENT_READ_FIRST_REQUEST_ID,
                &mut Vec::new(),
            )
            .await
        };
        drop(conn);
        Ok::<_, phux_client::attach::AttachError>((view, agents))
    });
    match listing {
        Ok((view, agents)) => render_listing(json, depth, view, &agents),
        Err(err) => target.report_unreachable(json, &err, "ls"),
    }
}

/// The terminals whose agent records the text needs: one session's, or
/// every local one.
fn agent_panes(snapshot: &SessionSnapshot, session: Option<&str>) -> Vec<ResourceId> {
    let session = session.and_then(|name| snapshot.sessions.iter().find(|s| s.name == name));
    session.map_or_else(
        || {
            resource::terminals(snapshot)
                .filter(|pane| pane.id.is_local())
                .map(|pane| pane.id.clone())
                .collect()
        },
        |session| {
            resource::terminals_in(snapshot, session.id)
                .map(|pane| pane.id.clone())
                .collect()
        },
    )
}

/// The empty listing, in the `phux host ls` mold: say what is missing,
/// then name the exact next commands. Stdout (it answers the question asked)
/// and exit 0 (an empty enumeration is a true answer, not a failure — the
/// non-zero case is *no server*, which never reaches this).
const EMPTY_STATE: [&str; 2] = [
    "No sessions.",
    "Start one with `phux` (attaches, auto-starting a server), or `phux new NAME -- CMD`.",
];

/// Render the session list, one line per session (tmux-`ls`-ish), followed
/// by satellite Terminals that cannot be joined to hub-local sessions. An
/// empty server prints [`EMPTY_STATE`] instead of nothing.
pub(crate) fn print_sessions(snapshot: &SessionSnapshot, organization: Option<&OrgInventory>) {
    print_lines(session_lines(snapshot, organization));
}

/// Print `lines`, or [`EMPTY_STATE`] when there are none.
fn print_lines(lines: Vec<String>) {
    if lines.is_empty() {
        for line in EMPTY_STATE {
            outln!("{line}");
        }
        return;
    }
    for line in lines {
        outln!("{line}");
    }
}

/// The `phux ls -l` body: name-sorted session lines with windows, lifetime,
/// and key, each followed by its panes' agent sessions, then satellite
/// Terminals. Empty exactly when there is nothing to list ([`EMPTY_STATE`]).
/// A hub reporting a host inventory renders grouped by host
/// ([`host_grouped_lines`]). `organization` supplies each session's
/// lifetime and execution counts when the server reports them.
fn session_lines(snapshot: &SessionSnapshot, organization: Option<&OrgInventory>) -> Vec<String> {
    with_hosts(snapshot, local_session_lines(snapshot, organization))
}
