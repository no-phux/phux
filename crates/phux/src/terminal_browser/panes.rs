use std::collections::HashMap;
use std::path::Path;

use phux_client::attach::connection::Connection;
use phux_client::layout::{self, ArrangementDoc};
use phux_client::layout_ops::LayoutOps;
use phux_core::process::TerminalProcessState;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use serde::Serialize;
use serde_json::Value;

use super::Caller;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Pane {
    pub id: String,
    pub tab: String,
}

#[derive(Serialize)]
pub(super) struct PaneDetails {
    #[serde(flatten)]
    pub pane: Pane,
    pub tty: Option<String>,
    pub command: Option<String>,
    #[serde(skip)]
    pub child: Option<i32>,
}

pub(super) struct Inventory {
    pub panes: Vec<PaneDetails>,
    pub layouts: Vec<ArrangementDoc>,
}

impl Inventory {
    pub(super) fn pane(&self, id: &str) -> Option<&PaneDetails> {
        self.panes.iter().find(|pane| pane.pane.id == id)
    }

    pub(super) fn current(
        &self,
        tty: Option<&str>,
        caller: &Caller,
        processes: &Processes,
    ) -> Option<Pane> {
        if let Some(tty) = tty
            && let Some(pane) = self
                .panes
                .iter()
                .find(|pane| pane.tty.as_deref() == Some(tty))
        {
            return Some(pane.pane.clone());
        }
        let mut pid = i32::try_from(caller.pid).ok()?;
        // Bound a malformed or concurrently changing ancestor chain.
        for _ in 0..128 {
            if let Some(pane) = self.panes.iter().find(|pane| pane.child == Some(pid)) {
                return Some(pane.pane.clone());
            }
            let parent = processes.0.get(&pid)?.parent;
            if parent <= 1 || parent == pid {
                return None;
            }
            pid = parent;
        }
        None
    }

    pub(super) fn neighbor(&self, id: &str, direction: layout::Direction) -> Option<Pane> {
        let resource = resource_id(id).ok()?;
        for document in &self.layouts {
            let Some(group) = document.group_of(&resource) else {
                continue;
            };
            let tree = document.groups[group].state.tree.as_ref()?;
            let representative = document
                .slot_of(&resource)
                .map_or(&resource, |slot| slot.showing());
            let neighbor = layout::focus_direction(tree, representative, direction)?;
            return self
                .pane(&format!("@{}", neighbor.local_id()?))
                .map(|p| p.pane.clone());
        }
        None
    }

    pub(super) fn representative(&self, id: &str) -> Result<ResourceId, String> {
        let resource = resource_id(id)?;
        let document = self
            .layouts
            .iter()
            .find(|doc| doc.group_of(&resource).is_some())
            .ok_or("pane is no longer in an arrangement")?;
        Ok(document
            .slot_of(&resource)
            .map_or_else(|| resource.clone(), |slot| slot.showing().clone()))
    }
}

pub(super) fn resource_id(id: &str) -> Result<ResourceId, String> {
    let number = id.strip_prefix('@').ok_or("pane id must be @N")?;
    let id: u32 = number.parse().map_err(|_| "pane id must be @N")?;
    if id == 0 || number != id.to_string() {
        return Err("pane id must be canonical @N".into());
    }
    Ok(ResourceId::local(id))
}

pub(super) async fn read(server: &Path, processes: &Processes) -> Result<Inventory, String> {
    let mut conn = Connection::connect(server)
        .await
        .map_err(|e| e.to_string())?;
    let view = phux_client::state::get_state_on(&mut conn)
        .await
        .map_err(|e| e.to_string())?;
    // Only local panes are exported; a missing satellite cannot change this inventory.
    let snapshot = view.snapshot();
    let mut inventory = Inventory {
        panes: Vec::new(),
        layouts: Vec::new(),
    };
    for session in &snapshot.sessions {
        if !phux_client::resource::terminals_in(snapshot, session.id).any(|info| info.id.is_local())
        {
            continue;
        }
        let document = LayoutOps::new(&mut conn, session.id, 100)
            .read()
            .await
            .map_err(|e| e.to_string())?;
        inventory
            .panes
            .extend(session_panes(&mut conn, snapshot, session, &document, processes).await?);
        inventory.layouts.push(document);
    }
    Ok(inventory)
}

async fn session_panes(
    conn: &mut Connection,
    snapshot: &SessionSnapshot,
    session: &SessionInfo,
    document: &ArrangementDoc,
    processes: &Processes,
) -> Result<Vec<PaneDetails>, String> {
    let session_id = session
        .key
        .map_or_else(|| session.id.get().to_string(), |key| key.to_string());
    let mut panes = Vec::new();
    for info in
        phux_client::resource::terminals_in(snapshot, session.id).filter(|info| info.id.is_local())
    {
        let Some(group) = document.group_of(&info.id) else {
            continue;
        };
        let Some(process) = process(conn, &info.id).await? else {
            continue;
        };
        if process.exit.is_some() {
            continue;
        }
        let child = process.child.map(|child| child.pid);
        panes.push(PaneDetails {
            pane: Pane {
                id: format!("@{}", info.id.local_id().ok_or("not a local pane")?),
                tab: format!(
                    "phux-{session_id}-{}",
                    phux_protocol::api::hex16(&document.groups[group].id)
                ),
            },
            tty: child
                .and_then(|pid| processes.0.get(&pid))
                .and_then(|process| process.tty.clone()),
            command: process.foreground.and_then(|foreground| foreground.name),
            child,
        });
    }
    Ok(panes)
}

async fn process(
    conn: &mut Connection,
    id: &ResourceId,
) -> Result<Option<TerminalProcessState>, String> {
    let result = conn
        .request(
            1,
            Command::GetTerminalState {
                terminal_id: id.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
        )
        .await
        .map_err(|e| e.to_string())?
        .into_result_ignoring_interleaved();
    decode_process(result)
}

fn decode_process(result: CommandResult) -> Result<Option<TerminalProcessState>, String> {
    match result {
        CommandResult::OkWith(CommandValue::Json(text)) => {
            let mut json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            serde_json::from_value(json["process"].take()).map_err(|e| e.to_string())
        }
        CommandResult::Error {
            code: ErrorCode::TerminalNotFound | ErrorCode::NotFound,
            ..
        } => Ok(None),
        CommandResult::Error { message, .. } => Err(message),
        _ => Err("unexpected terminal process response".into()),
    }
}

pub(super) struct Process {
    pub parent: i32,
    pub tty: Option<String>,
}

pub(super) struct Processes(pub HashMap<i32, Process>);

impl Processes {
    pub(super) async fn read() -> Result<Self, String> {
        let output = tokio::process::Command::new("ps")
            .args([
                "-U",
                &rustix::process::geteuid().as_raw().to_string(),
                "-o",
                "pid=,ppid=,tty=",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err("could not read the local process table".into());
        }
        Ok(Self::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    pub(super) fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    let pid = fields.next()?.parse().ok()?;
                    let parent = fields.next()?.parse().ok()?;
                    let tty = fields.next().filter(|tty| !matches!(*tty, "??" | "?"));
                    let tty = tty.map(|tty| format!("/dev/{tty}"));
                    Some((pid, Process { parent, tty }))
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_processes_are_not_inventory_failures() {
        assert_eq!(
            decode_process(CommandResult::OkWith(CommandValue::Json(
                r#"{"process":null}"#.into()
            )))
            .unwrap(),
            None
        );
        assert_eq!(
            decode_process(CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "gone".into()
            })
            .unwrap(),
            None
        );
        assert!(
            decode_process(CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: "denied".into()
            })
            .is_err()
        );
    }
}
