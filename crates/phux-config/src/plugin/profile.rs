use std::collections::BTreeSet;

use serde::Deserialize;

use super::validate::{non_empty, normalize_id, reject_duplicate_ids, trim_optional};
use super::{
    PluginManifestAction, PluginManifestAgent, PluginManifestError, PluginManifestEvent,
    PluginManifestPane, PluginManifestProfile, PluginProfilePane,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPluginManifestProfile {
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
    panes: Vec<RawPluginProfilePane>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginProfilePane {
    id: String,
    pane: String,
    role: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct ProfileSourceSlices<'a> {
    pub(super) agents: &'a [PluginManifestAgent],
    pub(super) actions: &'a [PluginManifestAction],
    pub(super) events: &'a [PluginManifestEvent],
    pub(super) panes: &'a [PluginManifestPane],
}

pub(super) fn normalize_profiles(
    raw: Vec<RawPluginManifestProfile>,
    sources: ProfileSourceSlices<'_>,
) -> Result<Vec<PluginManifestProfile>, PluginManifestError> {
    let known = ProfileSourceIds::from_sources(&sources);
    let profiles = raw
        .into_iter()
        .map(|profile| normalize_profile(profile, &known))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_ids(
        profiles.iter().map(|profile| profile.id.as_str()),
        "plugin profile",
    )?;
    Ok(profiles)
}

struct ProfileSourceIds<'a> {
    agents: BTreeSet<&'a str>,
    actions: BTreeSet<&'a str>,
    events: BTreeSet<&'a str>,
    panes: BTreeSet<&'a str>,
}

impl<'a> ProfileSourceIds<'a> {
    fn from_sources(sources: &ProfileSourceSlices<'a>) -> Self {
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

fn normalize_profile(
    raw: RawPluginManifestProfile,
    known: &ProfileSourceIds<'_>,
) -> Result<PluginManifestProfile, PluginManifestError> {
    let id = normalize_id(&raw.id, false, "plugin profile id")?;
    let contexts = normalize_non_empty_strings(raw.contexts, "plugin profile context")?;
    let agents = normalize_profile_refs(raw.agents, &known.agents, &id, "agent")?;
    let actions = normalize_profile_refs(raw.actions, &known.actions, &id, "action")?;
    let events = normalize_profile_refs(raw.events, &known.events, &id, "event")?;
    let panes = raw
        .panes
        .into_iter()
        .map(|pane| normalize_profile_pane(&pane, &id, &known.panes))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_ids(
        panes.iter().map(|pane| pane.id.as_str()),
        "plugin profile pane",
    )?;

    Ok(PluginManifestProfile {
        id,
        title: non_empty(&raw.title, "plugin profile title")?,
        description: raw.description.as_deref().and_then(trim_optional),
        contexts,
        agents,
        actions,
        events,
        panes,
    })
}

fn normalize_profile_pane(
    raw: &RawPluginProfilePane,
    profile_id: &str,
    known_panes: &BTreeSet<&str>,
) -> Result<PluginProfilePane, PluginManifestError> {
    let pane = normalize_id(&raw.pane, false, "plugin profile pane reference")?;
    if !known_panes.contains(pane.as_str()) {
        return Err(unknown_ref(profile_id, "pane", &pane));
    }
    Ok(PluginProfilePane {
        id: normalize_id(&raw.id, false, "plugin profile pane id")?,
        pane,
        role: normalize_id(&raw.role, false, "plugin profile pane role")?,
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

fn normalize_profile_refs(
    raw: Vec<String>,
    known: &BTreeSet<&str>,
    profile_id: &str,
    label: &str,
) -> Result<Vec<String>, PluginManifestError> {
    let refs = raw
        .into_iter()
        .map(|value| normalize_id(&value, false, "plugin profile reference"))
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_ids(refs.iter().map(String::as_str), "plugin profile reference")?;
    for reference in &refs {
        if !known.contains(reference.as_str()) {
            return Err(unknown_ref(profile_id, label, reference));
        }
    }
    Ok(refs)
}

fn unknown_ref(profile_id: &str, label: &str, reference: &str) -> PluginManifestError {
    PluginManifestError::Invalid(format!(
        "profile {profile_id} references unknown {label} '{reference}'"
    ))
}
