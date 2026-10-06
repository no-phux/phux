use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::validate::{
    non_empty, normalize_command, normalize_id, reject_duplicate_ids, trim_optional,
};
use super::workspace::{RawPluginManifestWorkspace, WorkspaceSourceSlices, normalize_workspaces};
use super::{
    PluginAgentAttention, PluginAgentState, PluginManifest, PluginManifestAction,
    PluginManifestAgent, PluginManifestBuild, PluginManifestError, PluginManifestEvent,
    PluginManifestLinkHandler, PluginManifestPane, PluginManifestSidebar, PluginManifestWidget,
    PluginPanePlacement, PluginPlatform, PluginWidgetSlot, SIDEBAR_SECTION_DEFAULT_ROWS,
    SIDEBAR_SECTION_MAX_ROWS,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifest {
    id: String,
    name: String,
    version: String,
    min_phux_version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<PluginPlatform>>,
    #[serde(default)]
    build: Vec<RawPluginManifestBuild>,
    #[serde(default)]
    agents: Vec<RawPluginManifestAgent>,
    #[serde(default)]
    actions: Vec<RawPluginManifestAction>,
    #[serde(default)]
    events: Vec<RawPluginManifestEvent>,
    #[serde(default)]
    panes: Vec<RawPluginManifestPane>,
    #[serde(default)]
    links: Vec<RawPluginManifestLinkHandler>,
    #[serde(default)]
    workspaces: Vec<RawPluginManifestWorkspace>,
    #[serde(default)]
    widgets: Vec<RawPluginManifestWidget>,
    #[serde(default)]
    sidebar: Vec<RawPluginManifestSidebar>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifestSidebar {
    id: String,
    title: String,
    format: String,
    #[serde(default)]
    rows: Option<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifestBuild {
    #[serde(default)]
    platforms: Option<Vec<PluginPlatform>>,
    command: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifestAgent {
    id: String,
    label: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    state: PluginAgentState,
    #[serde(default)]
    attention: PluginAgentAttention,
    #[serde(default)]
    contexts: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifestAction {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    contexts: Vec<String>,
    #[serde(default)]
    platforms: Option<Vec<PluginPlatform>>,
    command: Vec<String>,
    #[serde(default)]
    keys: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifestEvent {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    on: String,
    #[serde(default)]
    platforms: Option<Vec<PluginPlatform>>,
    command: Vec<String>,
}

/// Raw `[[widgets]]` entry. No `deny_unknown_fields`: every key besides
/// `id` / `slot` / `kind` is a kind-specific widget option captured by the
/// flattened `opts` map (the same open shape `[status]` widget tables use).
#[derive(Debug, Deserialize)]
struct RawPluginManifestWidget {
    id: String,
    #[serde(default)]
    slot: PluginWidgetSlot,
    kind: String,
    #[serde(flatten)]
    opts: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifestPane {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<PluginPlatform>>,
    #[serde(default)]
    placement: PluginPanePlacement,
    command: Vec<String>,
}

/// Load and validate a `phux-plugin.toml` manifest.
///
/// # Errors
///
/// Returns an error if the file cannot be read, cannot be parsed as TOML,
/// or violates the plugin manifest schema.
pub fn load_plugin_manifest(path: &Path) -> Result<PluginManifest, PluginManifestError> {
    let source = load_manifest_source(path)?;
    let manifest_path = source.canonical_path;
    let plugin_root = manifest_path
        .parent()
        .ok_or_else(|| PluginManifestError::Invalid("manifest path has no parent".to_owned()))?
        .to_path_buf();
    let raw: RawPluginManifest =
        toml::from_str(&source.input).map_err(|err| PluginManifestError::Parse {
            path: source.display_path,
            message: err.message().to_owned(),
        })?;

    let id = normalize_id(&raw.id, true, "plugin id")?;
    let name = non_empty(&raw.name, "plugin name")?;
    let version = non_empty(&raw.version, "plugin version")?;
    let min_phux_version = non_empty(&raw.min_phux_version, "plugin min_phux_version")?;
    super::version::enforce_min_phux_version(
        &id,
        &min_phux_version,
        super::version::CURRENT_PHUX_VERSION,
    )?;

    // Workspaces resolve references into the sections before them.
    let build = raw
        .build
        .into_iter()
        .map(normalize_build)
        .collect::<Result<Vec<_>, _>>()?;
    let agents =
        normalize_unique_section(raw.agents, normalize_agent, id_of_agent, "plugin agent")?;
    let actions =
        normalize_unique_section(raw.actions, normalize_action, id_of_action, "plugin action")?;
    let events =
        normalize_unique_section(raw.events, normalize_event, id_of_event, "plugin event")?;
    let panes = normalize_unique_section(raw.panes, normalize_pane, id_of_pane, "plugin pane")?;
    let links = normalize_unique_section(
        raw.links,
        normalize_link_handler,
        id_of_link,
        "plugin link handler",
    )?;
    let workspaces = normalize_workspaces(
        raw.workspaces,
        WorkspaceSourceSlices {
            agents: &agents,
            actions: &actions,
            events: &events,
            panes: &panes,
        },
    )?;
    let widgets =
        normalize_unique_section(raw.widgets, normalize_widget, id_of_widget, "plugin widget")?;
    let sidebar = normalize_unique_section(
        raw.sidebar,
        normalize_sidebar,
        id_of_sidebar,
        "plugin sidebar section",
    )?;

    Ok(PluginManifest {
        id,
        name,
        version,
        min_phux_version,
        description: raw.description.as_deref().and_then(trim_optional),
        manifest_path,
        plugin_root,
        platforms: raw.platforms,
        build,
        agents,
        actions,
        events,
        panes,
        links,
        workspaces,
        widgets,
        sidebar,
    })
}

/// Normalize one repeated section entry by entry, then reject duplicate
/// ids within it. `label` names the section in both error paths.
fn normalize_unique_section<R, T>(
    raw: Vec<R>,
    normalize: fn(R) -> Result<T, PluginManifestError>,
    id_of: fn(&T) -> &str,
    label: &str,
) -> Result<Vec<T>, PluginManifestError> {
    let entries = raw
        .into_iter()
        .map(normalize)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_ids(entries.iter().map(id_of), label)?;
    Ok(entries)
}

const fn id_of_agent(agent: &PluginManifestAgent) -> &str {
    agent.id.as_str()
}

const fn id_of_action(action: &PluginManifestAction) -> &str {
    action.id.as_str()
}

const fn id_of_event(event: &PluginManifestEvent) -> &str {
    event.id.as_str()
}

const fn id_of_pane(pane: &PluginManifestPane) -> &str {
    pane.id.as_str()
}

const fn id_of_link(link: &PluginManifestLinkHandler) -> &str {
    link.id.as_str()
}

const fn id_of_widget(widget: &PluginManifestWidget) -> &str {
    widget.id.as_str()
}

const fn id_of_sidebar(section: &PluginManifestSidebar) -> &str {
    section.id.as_str()
}

/// Validate one `[[sidebar]]` section: every `{token}` its `format` names is
/// in [`crate::vocab::SIDEBAR_TOKENS`] (with a did-you-mean on a typo), and
/// `rows` is within `1..=SIDEBAR_SECTION_MAX_ROWS`.
fn normalize_sidebar(
    raw: RawPluginManifestSidebar,
) -> Result<PluginManifestSidebar, PluginManifestError> {
    let RawPluginManifestSidebar {
        id,
        title,
        format,
        rows,
    } = raw;
    let id = normalize_id(&id, false, "plugin sidebar section id")?;
    let format = non_empty(&format, "plugin sidebar section format")?;
    if let Some((token, suggestion)) = crate::vocab::unknown_sidebar_token(&format) {
        let hint = suggestion.map_or_else(
            || format!(" (known: {})", crate::vocab::SIDEBAR_TOKENS.join(", ")),
            |near| format!(" (did you mean `{{{near}}}`?)"),
        );
        return Err(PluginManifestError::Invalid(format!(
            "plugin sidebar section '{id}': unknown format token `{{{token}}}`{hint}"
        )));
    }
    let rows = rows.unwrap_or(SIDEBAR_SECTION_DEFAULT_ROWS);
    if !(1..=SIDEBAR_SECTION_MAX_ROWS).contains(&rows) {
        return Err(PluginManifestError::Invalid(format!(
            "plugin sidebar section '{id}': rows must be 1..={SIDEBAR_SECTION_MAX_ROWS}, got {rows}"
        )));
    }
    Ok(PluginManifestSidebar {
        title: non_empty(&title, "plugin sidebar section title")?,
        id,
        format,
        rows,
    })
}

fn normalize_widget(
    raw: RawPluginManifestWidget,
) -> Result<PluginManifestWidget, PluginManifestError> {
    Ok(PluginManifestWidget {
        id: normalize_id(&raw.id, false, "plugin widget id")?,
        slot: raw.slot,
        kind: non_empty(&raw.kind, "plugin widget kind")?,
        opts: raw.opts,
    })
}

fn normalize_build(
    raw: RawPluginManifestBuild,
) -> Result<PluginManifestBuild, PluginManifestError> {
    let command = normalize_command(&raw.command)?;

    Ok(PluginManifestBuild {
        platforms: raw.platforms,
        command,
    })
}

fn normalize_agent(
    raw: RawPluginManifestAgent,
) -> Result<PluginManifestAgent, PluginManifestError> {
    let contexts = raw
        .contexts
        .into_iter()
        .map(|context| non_empty(&context, "plugin agent context"))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(PluginManifestAgent {
        id: normalize_id(&raw.id, false, "plugin agent id")?,
        label: non_empty(&raw.label, "plugin agent label")?,
        description: raw.description.as_deref().and_then(trim_optional),
        state: raw.state,
        attention: raw.attention,
        contexts,
    })
}

fn normalize_action(
    raw: RawPluginManifestAction,
) -> Result<PluginManifestAction, PluginManifestError> {
    let contexts = raw
        .contexts
        .iter()
        .map(|context| non_empty(context, "plugin action context"))
        .collect::<Result<Vec<_>, _>>()?;
    let command = normalize_command(&raw.command)?;

    Ok(PluginManifestAction {
        id: normalize_id(&raw.id, false, "plugin action id")?,
        title: non_empty(&raw.title, "plugin action title")?,
        description: raw.description.as_deref().and_then(trim_optional),
        contexts,
        platforms: raw.platforms,
        command,
        keys: raw.keys.as_deref().and_then(trim_optional),
    })
}

fn normalize_event(
    raw: RawPluginManifestEvent,
) -> Result<PluginManifestEvent, PluginManifestError> {
    let command = normalize_command(&raw.command)?;

    Ok(PluginManifestEvent {
        id: normalize_id(&raw.id, false, "plugin event id")?,
        title: non_empty(&raw.title, "plugin event title")?,
        description: raw.description.as_deref().and_then(trim_optional),
        on: non_empty(&raw.on, "plugin event name")?,
        platforms: raw.platforms,
        command,
    })
}

fn normalize_pane(raw: RawPluginManifestPane) -> Result<PluginManifestPane, PluginManifestError> {
    let command = normalize_command(&raw.command)?;

    Ok(PluginManifestPane {
        id: normalize_id(&raw.id, false, "plugin pane id")?,
        title: non_empty(&raw.title, "plugin pane title")?,
        description: raw.description.as_deref().and_then(trim_optional),
        platforms: raw.platforms,
        placement: raw.placement,
        command,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginManifestLinkHandler {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    contexts: Vec<String>,
    #[serde(default)]
    schemes: Vec<String>,
    #[serde(default)]
    patterns: Vec<String>,
    #[serde(default)]
    platforms: Option<Vec<PluginPlatform>>,
    command: Vec<String>,
}

fn normalize_link_handler(
    raw: RawPluginManifestLinkHandler,
) -> Result<PluginManifestLinkHandler, PluginManifestError> {
    let contexts = raw
        .contexts
        .iter()
        .map(|context| non_empty(context, "plugin link handler context"))
        .collect::<Result<Vec<_>, _>>()?;
    let schemes = raw
        .schemes
        .iter()
        .map(|scheme| non_empty(scheme, "plugin link handler scheme"))
        .collect::<Result<Vec<_>, _>>()?;
    let patterns = raw
        .patterns
        .iter()
        .map(|pattern| non_empty(pattern, "plugin link handler pattern"))
        .collect::<Result<Vec<_>, _>>()?;
    if schemes.is_empty() && patterns.is_empty() {
        return Err(PluginManifestError::Invalid(
            "plugin link handler requires at least one scheme or pattern".to_owned(),
        ));
    }
    let command = normalize_command(&raw.command)?;

    Ok(PluginManifestLinkHandler {
        id: normalize_id(&raw.id, false, "plugin link handler id")?,
        title: non_empty(&raw.title, "plugin link handler title")?,
        description: raw.description.as_deref().and_then(trim_optional),
        contexts,
        schemes,
        patterns,
        platforms: raw.platforms,
        command,
    })
}

const MANIFEST_MAX_BYTES: u64 = 1024 * 1024;

struct ManifestSource {
    display_path: PathBuf,
    canonical_path: PathBuf,
    input: String,
}

fn load_manifest_source(path: &Path) -> Result<ManifestSource, PluginManifestError> {
    let display_path = if path.is_dir() {
        path.join("phux-plugin.toml")
    } else {
        path.to_path_buf()
    };
    let metadata = std::fs::metadata(&display_path)?;
    if !metadata.is_file() {
        return Err(PluginManifestError::Invalid(format!(
            "{} is not a regular file",
            display_path.display()
        )));
    }
    reject_oversized(metadata.len())?;
    let input = read_manifest_string(&display_path)?;
    Ok(ManifestSource {
        canonical_path: display_path.canonicalize()?,
        display_path,
        input,
    })
}

fn read_manifest_string(path: &Path) -> Result<String, PluginManifestError> {
    let file = std::fs::File::open(path)?;
    let mut reader = file.take(MANIFEST_MAX_BYTES + 1);
    let mut input = String::new();
    reader.read_to_string(&mut input)?;
    let len = u64::try_from(input.len()).map_err(|_| oversized_error())?;
    reject_oversized(len)?;
    Ok(input)
}

fn reject_oversized(len: u64) -> Result<(), PluginManifestError> {
    if len > MANIFEST_MAX_BYTES {
        return Err(oversized_error());
    }
    Ok(())
}

fn oversized_error() -> PluginManifestError {
    PluginManifestError::Invalid(format!(
        "plugin manifest exceeds {MANIFEST_MAX_BYTES} byte limit"
    ))
}
