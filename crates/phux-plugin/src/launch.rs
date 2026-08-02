//! Launch executor resolution (phux-ark7, [ADR-0042]).
//!
//! Resolve a named agent integration template — shipped by an *enabled*
//! plugin under its `integrations/` directory — into a spawnable
//! child-process argv. This is the resolution half of the launch executor:
//! it loads the config, finds the integration, expands the
//! `${PHUX_PLUGIN_ROOT}` placeholder, and returns a [`ResolvedLaunch`] the
//! CLI spawns through the ordinary `SPAWN_TERMINAL` path (so the server's
//! `PHUX_TERMINAL_ID` injection and pane recording compose for free).
//!
//! There is no in-process host: the launched program is a child-process
//! argv, exactly like plugin actions and event hooks.
//!
//! [ADR-0042]: ../../ADR/0042-launch-executor.md

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phux_config::integration::{
    self, IntegrationError, IntegrationLaunch, IntegrationSessionIdentity, IntegrationTemplate,
    LaunchWorkingDirectory, SessionResumeError,
};
use phux_config::loader as config_loader;

/// Directory, relative to a plugin root, where a plugin ships its agent
/// integration templates. A convention, not a manifest-declared path: the
/// launch executor scans it for every enabled plugin.
const INTEGRATIONS_DIR: &str = "integrations";

/// A fully resolved launch: the argv to spawn, where to run it, and which
/// plugin/integration it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLaunch {
    /// Owning plugin id.
    pub plugin_id: String,
    /// Integration id that was resolved.
    pub integration_id: String,
    /// Integration display name, when declared.
    pub display_name: Option<String>,
    /// Spawnable argv: the template command with `${PHUX_PLUGIN_ROOT}`
    /// expanded and any caller-supplied extra args appended.
    pub argv: Vec<String>,
    /// Working directory the program runs in.
    pub cwd: PathBuf,
    /// How `cwd` was chosen.
    pub working_directory: LaunchWorkingDirectory,
    /// Owning plugin's root directory.
    pub plugin_root: PathBuf,
    /// Provider-native session policy declared by the integration.
    pub session_identity: Option<IntegrationSessionIdentity>,
}

impl ResolvedLaunch {
    /// Rebuild this launch argv as a provider-native resume invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the integration does not support native resume
    /// or the supplied identity violates the policy's bounds.
    pub fn resume_argv(&self, native_id: &str) -> Result<Vec<String>, SessionResumeError> {
        self.session_identity
            .as_ref()
            .ok_or(SessionResumeError::Unsupported)?
            .resume_argv(&self.argv, native_id)
    }

    /// Rebuild this launch argv with a caller-supplied fresh-session identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider has no documented fresh-identity
    /// argv or the supplied identity violates the policy's bounds.
    pub fn fresh_argv(&self, native_id: &str) -> Result<Vec<String>, SessionResumeError> {
        self.session_identity
            .as_ref()
            .ok_or(SessionResumeError::Unsupported)?
            .fresh_argv(&self.argv, native_id)
    }
}

/// One launchable integration surfaced by [`list_launchable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchableIntegration {
    /// Owning plugin id.
    pub plugin_id: String,
    /// Integration id (the `phux launch <id>` name).
    pub integration_id: String,
    /// Display name, when declared.
    pub display_name: Option<String>,
    /// Kind slug, when declared.
    pub kind: Option<String>,
}

/// Failure resolving a launch before a spawnable argv exists.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchError {
    /// Config load failed.
    #[error("{0}")]
    Config(#[from] phux_config::ConfigError),
    /// A configured plugin manifest failed to load.
    #[error("could not load {path}: {source}")]
    Manifest {
        /// Manifest path.
        path: PathBuf,
        /// Manifest error.
        source: phux_config::plugin::PluginManifestError,
    },
    /// Multiple enabled manifests claimed one globally unique plugin id.
    #[error(
        "enabled plugin id {id:?} is ambiguous between {} and {}",
        first.display(),
        second.display()
    )]
    DuplicatePluginId {
        /// Duplicated manifest id.
        id: String,
        /// First owning plugin root.
        first: PathBuf,
        /// Conflicting plugin root.
        second: PathBuf,
    },
    /// Multiple enabled plugins/templates claimed one integration id.
    #[error("enabled integration id {id:?} is ambiguous between plugins {first:?} and {second:?}")]
    DuplicateIntegrationId {
        /// Duplicated integration id.
        id: String,
        /// First plugin that declared the id.
        first: String,
        /// Conflicting plugin that declared the id.
        second: String,
    },
    /// The requested integration's template failed to read or validate.
    #[error("could not load integration template {path}: {source}")]
    Template {
        /// Template path.
        path: PathBuf,
        /// Template error.
        source: IntegrationError,
    },
    /// A plugin's `integrations/` directory could not be read.
    #[error("could not read integration directory {path}: {source}")]
    Dir {
        /// Directory path.
        path: PathBuf,
        /// I/O error.
        source: std::io::Error,
    },
    /// No enabled plugin ships an integration with this id.
    #[error("no launchable integration named {name:?} in any enabled plugin")]
    NotFound {
        /// Requested integration id.
        name: String,
        /// Ids of the launchable integrations that *are* available, for a
        /// caller-formatted hint.
        available: Vec<String>,
    },
    /// The integration exists but declares no `[launch]` command.
    #[error("integration {name:?} declares no `[launch]` command to launch")]
    NoLaunchCommand {
        /// Requested integration id.
        name: String,
    },
}

struct EnabledPlugin {
    plugin_id: String,
    plugin_root: PathBuf,
}

/// Resolve `integration_id` against every enabled plugin's `integrations/`
/// directory, expanding the launch command into a spawnable argv rooted at
/// the owning plugin.
///
/// `extra_args` are appended verbatim to the launched program's argv (the
/// user's `phux launch codex -- --resume`). `workspace_cwd` is the
/// directory a `working_directory = "workspace"` template runs in
/// (typically the process's current directory).
///
/// Resolution scans every enabled plugin and rejects an integration id claimed
/// by more than one enabled template. Within a plugin, templates are read in
/// sorted filename order. A template that fails to parse is skipped **unless**
/// its filename stem is the requested id, in which case its error is surfaced.
///
/// # Errors
///
/// Returns [`LaunchError`] when the config or a plugin manifest cannot be
/// loaded, a plugin's `integrations/` directory cannot be read, the
/// requested integration's template is invalid, no enabled plugin ships the
/// integration ([`LaunchError::NotFound`]), or the integration declares no
/// `[launch]` command ([`LaunchError::NoLaunchCommand`]).
pub fn resolve_launch(
    config_path: &Path,
    integration_id: &str,
    extra_args: &[String],
    workspace_cwd: &Path,
) -> Result<ResolvedLaunch, LaunchError> {
    let mut available: Vec<String> = Vec::new();
    let mut matched_owner: Option<String> = None;
    let mut resolved: Option<ResolvedLaunch> = None;
    for plugin in enabled_plugins(config_path)? {
        for path in template_paths(&plugin.plugin_root)? {
            let template = match integration::load_integration_template(&path) {
                Ok(template) => template,
                Err(source) => {
                    // Surface the error only when this is the file the
                    // caller asked for (by filename stem); a broken sibling
                    // template must not block launching a healthy one.
                    if path.file_stem().and_then(|s| s.to_str()) == Some(integration_id) {
                        return Err(LaunchError::Template { path, source });
                    }
                    continue;
                }
            };
            if template.launch.is_some() {
                available.push(template.id.clone());
            }
            if template.id != integration_id {
                continue;
            }
            if let Some(first) = &matched_owner {
                return Err(LaunchError::DuplicateIntegrationId {
                    id: integration_id.to_owned(),
                    first: first.clone(),
                    second: plugin.plugin_id.clone(),
                });
            }
            matched_owner = Some(plugin.plugin_id.clone());
            if let Some(launch) = template.launch.clone() {
                resolved = Some(build_resolved(
                    &plugin,
                    &template,
                    &launch,
                    extra_args,
                    workspace_cwd,
                ));
            }
        }
    }
    if let Some(resolved) = resolved {
        return Ok(resolved);
    }
    if matched_owner.is_some() {
        return Err(LaunchError::NoLaunchCommand {
            name: integration_id.to_owned(),
        });
    }
    available.sort();
    available.dedup();
    Err(LaunchError::NotFound {
        name: integration_id.to_owned(),
        available,
    })
}

/// Enumerate every launchable integration (one with a `[launch]` command)
/// shipped by an enabled plugin, in config order then sorted filename
/// order.
///
/// # Errors
///
/// Returns [`LaunchError`] when the config or a plugin manifest cannot be
/// loaded, or a plugin's `integrations/` directory cannot be read. An
/// individual template that fails to parse is skipped.
pub fn list_launchable(config_path: &Path) -> Result<Vec<LaunchableIntegration>, LaunchError> {
    let mut out = Vec::new();
    for plugin in enabled_plugins(config_path)? {
        for path in template_paths(&plugin.plugin_root)? {
            let Ok(template) = integration::load_integration_template(&path) else {
                continue;
            };
            if template.launch.is_none() {
                continue;
            }
            out.push(LaunchableIntegration {
                plugin_id: plugin.plugin_id.clone(),
                integration_id: template.id,
                display_name: template.display_name,
                kind: template.kind,
            });
        }
    }
    Ok(out)
}

fn build_resolved(
    plugin: &EnabledPlugin,
    template: &IntegrationTemplate,
    launch: &IntegrationLaunch,
    extra_args: &[String],
    workspace_cwd: &Path,
) -> ResolvedLaunch {
    let argv = integration::expand_launch_argv(&launch.command, &plugin.plugin_root, extra_args);
    let cwd = match launch.working_directory {
        LaunchWorkingDirectory::PluginRoot => plugin.plugin_root.clone(),
        LaunchWorkingDirectory::Workspace => workspace_cwd.to_path_buf(),
    };
    ResolvedLaunch {
        plugin_id: plugin.plugin_id.clone(),
        integration_id: template.id.clone(),
        display_name: template.display_name.clone(),
        argv,
        cwd,
        working_directory: launch.working_directory,
        plugin_root: plugin.plugin_root.clone(),
        session_identity: template.session_identity.clone(),
    }
}

fn enabled_plugins(config_path: &Path) -> Result<Vec<EnabledPlugin>, LaunchError> {
    let cfg = config_loader::load_from(config_path)?;
    let mut owners = BTreeMap::<String, PathBuf>::new();
    let mut out = Vec::new();
    for entry in cfg.plugins {
        if !entry.enabled {
            continue;
        }
        let manifest_path =
            phux_config::plugin::resolve_manifest_path(&entry.manifest, config_path);
        let manifest =
            phux_config::plugin::load_plugin_manifest(&manifest_path).map_err(|source| {
                LaunchError::Manifest {
                    path: manifest_path.clone(),
                    source,
                }
            })?;
        if let Some(first) = owners.insert(manifest.id.clone(), manifest.plugin_root.clone()) {
            return Err(LaunchError::DuplicatePluginId {
                id: manifest.id,
                first,
                second: manifest.plugin_root,
            });
        }
        out.push(EnabledPlugin {
            plugin_id: manifest.id,
            plugin_root: manifest.plugin_root,
        });
    }
    Ok(out)
}

/// Collect a plugin's integration template paths (`integrations/*.toml`) in
/// sorted order. A missing `integrations/` directory yields an empty list
/// (not every plugin ships integrations).
fn template_paths(plugin_root: &Path) -> Result<Vec<PathBuf>, LaunchError> {
    let dir = plugin_root.join(INTEGRATIONS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(LaunchError::Dir { path: dir, source }),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    Ok(paths)
}
