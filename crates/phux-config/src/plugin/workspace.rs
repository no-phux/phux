use std::collections::BTreeSet;

use serde::Deserialize;

use super::validate::{non_empty, normalize_id, reject_duplicate_ids, trim_optional};
use super::{
    PluginManifestAction, PluginManifestAgent, PluginManifestError, PluginManifestEvent,
    PluginManifestPane, PluginManifestWorkspace, PluginWorkspacePane,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPluginManifestWorkspace {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    contexts: Vec<String>,
    #[serde(default)]
    agents: Vec<String>,
    #[serde(default)]
    actions: Vec<String>,
    #[serde(default)]
    events: Vec<String>,
    #[serde(default)]
    panes: Vec<RawPluginWorkspacePane>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginWorkspacePane {
    id: String,
    pane: String,
    role: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceSourceSlices<'a> {
    pub(super) agents: &'a [PluginManifestAgent],
    pub(super) actions: &'a [PluginManifestAction],
    pub(super) events: &'a [PluginManifestEvent],
    pub(super) panes: &'a [PluginManifestPane],
}

pub(super) fn normalize_workspaces(
    raw: Vec<RawPluginManifestWorkspace>,
    sources: WorkspaceSourceSlices<'_>,
) -> Result<Vec<PluginManifestWorkspace>, PluginManifestError> {
    let known = WorkspaceSourceIds::from_sources(&sources);
    let workspaces = raw
        .into_iter()
        .map(|workspace| normalize_workspace(workspace, &known))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_ids(
        workspaces.iter().map(|workspace| workspace.id.as_str()),
        "plugin workspace",
    )?;
    Ok(workspaces)
}

struct WorkspaceSourceIds<'a> {
    agents: BTreeSet<&'a str>,
    actions: BTreeSet<&'a str>,
    events: BTreeSet<&'a str>,
    panes: BTreeSet<&'a str>,
}

impl<'a> WorkspaceSourceIds<'a> {
    fn from_sources(sources: &WorkspaceSourceSlices<'a>) -> Self {
        Self {
            agents: sources
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect(),
            actions: sources
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect(),
            events: sources
                .events
                .iter()
                .map(|event| event.id.as_str())
                .collect(),
            panes: sources.panes.iter().map(|pane| pane.id.as_str()).collect(),
        }
    }
}

fn normalize_workspace(
    raw: RawPluginManifestWorkspace,
    known: &WorkspaceSourceIds<'_>,
) -> Result<PluginManifestWorkspace, PluginManifestError> {
    let id = normalize_id(&raw.id, false, "plugin workspace id")?;
    let contexts = normalize_non_empty_strings(raw.contexts, "plugin workspace context")?;
    let agents = normalize_workspace_refs(raw.agents, &known.agents, &id, "agent")?;
    let actions = normalize_workspace_refs(raw.actions, &known.actions, &id, "action")?;
    let events = normalize_workspace_refs(raw.events, &known.events, &id, "event")?;
    let panes = raw
        .panes
        .into_iter()
        .map(|pane| normalize_workspace_pane(&pane, &id, &known.panes))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_ids(
        panes.iter().map(|pane| pane.id.as_str()),
        "plugin workspace pane",
    )?;

    Ok(PluginManifestWorkspace {
        id,
        title: non_empty(&raw.title, "plugin workspace title")?,
        description: raw.description.as_deref().and_then(trim_optional),
        contexts,
        agents,
        actions,
        events,
        panes,
    })
}

fn normalize_workspace_pane(
    raw: &RawPluginWorkspacePane,
    workspace_id: &str,
    known_panes: &BTreeSet<&str>,
) -> Result<PluginWorkspacePane, PluginManifestError> {
    let pane = normalize_id(&raw.pane, false, "plugin workspace pane reference")?;
    if !known_panes.contains(pane.as_str()) {
        return Err(unknown_ref(workspace_id, "pane", &pane));
    }
    Ok(PluginWorkspacePane {
        id: normalize_id(&raw.id, false, "plugin workspace pane id")?,
        pane,
        role: normalize_id(&raw.role, false, "plugin workspace pane role")?,
        description: raw.description.as_deref().and_then(trim_optional),
    })
}

fn normalize_non_empty_strings(
    raw: Vec<String>,
    label: &str,
) -> Result<Vec<String>, PluginManifestError> {
    raw.into_iter()
        .map(|value| non_empty(&value, label))
        .collect()
}

fn normalize_workspace_refs(
    raw: Vec<String>,
    known: &BTreeSet<&str>,
    workspace_id: &str,
    label: &str,
) -> Result<Vec<String>, PluginManifestError> {
    let refs = raw
        .into_iter()
        .map(|value| normalize_id(&value, false, "plugin workspace reference"))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_ids(
        refs.iter().map(String::as_str),
        "plugin workspace reference",
    )?;
    for reference in &refs {
        if !known.contains(reference.as_str()) {
            return Err(unknown_ref(workspace_id, label, reference));
        }
    }
    Ok(refs)
}

fn unknown_ref(workspace_id: &str, label: &str, reference: &str) -> PluginManifestError {
    PluginManifestError::Invalid(format!(
        "workspace {workspace_id} references unknown {label} '{reference}'"
    ))
}
