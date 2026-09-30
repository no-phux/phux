//! Launch executor resolution (ADR-0042): resolve an agent integration
//! template shipped by an enabled plugin under `integrations/` into a
//! spawnable argv, which the CLI spawns through `SPAWN_RESOURCE`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use phux_config::integration::{
    self, IntegrationAgentIdentity, IntegrationError, IntegrationLaunch,
    IntegrationSessionIdentity, IntegrationTemplate, LaunchWorkingDirectory, SessionResumeError,
};
use phux_config::loader as config_loader;

/// Where a plugin ships its integration templates, relative to its root.
const INTEGRATIONS_DIR: &str = "integrations";

/// A fully resolved launch: the argv to spawn, where, and from which plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLaunch {
    /// Owning plugin id.
    pub plugin_id: String,
    /// Integration id that was resolved.
    pub integration_id: String,
    /// Integration display name, when declared.
    pub display_name: Option<String>,
    /// The template command, plugin root expanded, extra args appended.
    pub argv: Vec<String>,
    /// Working directory the program runs in.
    pub cwd: PathBuf,
    /// How `cwd` was chosen.
    pub working_directory: LaunchWorkingDirectory,
    /// Owning plugin's root directory.
    pub plugin_root: PathBuf,
    /// Provider-native session policy declared by the integration.
    pub session_identity: Option<IntegrationSessionIdentity>,
    /// The launched agent's self-declared identity, when declared.
    pub agent_identity: Option<IntegrationAgentIdentity>,
}

impl ResolvedLaunch {
    /// This launch argv as a provider-native resume invocation.
    ///
    /// # Errors
    ///
    /// When native resume is unsupported or the identity is invalid.
    pub fn resume_argv(&self, native_id: &str) -> Result<Vec<String>, SessionResumeError> {
        self.session_identity
            .as_ref()
            .ok_or(SessionResumeError::Unsupported)?
            .resume_argv(&self.argv, native_id)
    }

    /// This launch argv with a caller-supplied fresh-session identity.
    ///
    /// # Errors
    ///
    /// When fresh identities are unsupported or the identity is invalid.
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
    /// Package category (`terminal-agent`), when declared.
    pub kind: Option<String>,
    /// The launched agent's self-declared identity (its detection kind).
    pub agent_identity: Option<IntegrationAgentIdentity>,
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
        /// Ids of the launchable integrations that are available.
        available: Vec<String>,
    },
    /// The integration exists but declares no `[launch]` command.
    #[error("integration {name:?} declares no `[launch]` command to launch")]
    NoLaunchCommand {
        /// Requested integration id.
        name: String,
    },
    /// The integration is valid but one or more external programs it needs
    /// are not executable on the launcher's `PATH`.
    #[error(
        "integration {name:?} is unavailable because these executables are not on PATH: {}",
        missing.join(", ")
    )]
    MissingExecutables {
        /// Requested integration id.
        name: String,
        /// Required executable names that were not found, in template order.
        missing: Vec<String>,
    },
}

struct EnabledPlugin {
    plugin_id: String,
    plugin_root: PathBuf,
}

/// Resolve `integration_id` across every enabled plugin into a spawnable
/// argv. `extra_args` are appended verbatim; `workspace_cwd` is where a
/// `working_directory = "workspace"` template runs.
///
/// An id claimed by two enabled templates is refused. A template that fails
/// to parse is skipped unless its filename stem is the requested id.
///
/// # Errors
///
/// Any [`LaunchError`].
pub fn resolve_launch(
    config_path: &Path,
    integration_id: &str,
    extra_args: &[String],
    workspace_cwd: &Path,
) -> Result<ResolvedLaunch, LaunchError> {
    resolve_loaded(
        load_templates(config_path)?,
        integration_id,
        extra_args,
        workspace_cwd,
    )
}

/// [`resolve_launch`] over an already-walked plugin tree.
fn resolve_loaded(
    loaded: Vec<LoadedTemplate>,
    integration_id: &str,
    extra_args: &[String],
    workspace_cwd: &Path,
) -> Result<ResolvedLaunch, LaunchError> {
    let mut available: Vec<String> = Vec::new();
    // The plugin owning the requested id, and the launch its template
    // resolves to (or why it cannot).
    let mut matched: Option<(String, Result<ResolvedLaunch, LaunchError>)> = None;
    for entry in loaded {
        let template = match entry.template {
            Ok(template) => template,
            Err(source) if template_file_names(&entry.path, integration_id) => {
                return Err(LaunchError::Template {
                    path: entry.path,
                    source,
                });
            }
            // A broken sibling must not block a healthy launch.
            Err(_) => continue,
        };
        let missing = template
            .launch
            .as_ref()
            .map(missing_executables)
            .unwrap_or_default();
        if template.launch.is_some() && missing.is_empty() {
            available.push(template.id.clone());
        }
        if template.id != integration_id {
            continue;
        }
        if let Some((first, _)) = matched {
            return Err(LaunchError::DuplicateIntegrationId {
                id: integration_id.to_owned(),
                first,
                second: entry.plugin_id,
            });
        }
        let name = integration_id.to_owned();
        let outcome = match (&template.launch, missing.is_empty()) {
            (Some(launch), true) => Ok(build_resolved(
                &entry.plugin_id,
                &entry.plugin_root,
                &template,
                launch,
                extra_args,
                workspace_cwd,
            )),
            (_, false) => Err(LaunchError::MissingExecutables { name, missing }),
            (None, true) => Err(LaunchError::NoLaunchCommand { name }),
        };
        matched = Some((entry.plugin_id, outcome));
    }
    if let Some((_, outcome)) = matched {
        return outcome;
    }
    available.sort();
    available.dedup();
    Err(LaunchError::NotFound {
        name: integration_id.to_owned(),
        available,
    })
}

/// Whether a template file's stem is `integration_id`, so its parse error
/// belongs to the requested launch.
fn template_file_names(path: &Path, integration_id: &str) -> bool {
    path.file_stem().and_then(|s| s.to_str()) == Some(integration_id)
}

/// Resolve the integration a `--kind` starts, in one walk of the plugin tree.
///
/// Without an explicit id it is the unique enabled integration whose
/// `[agent_identity] kind` claims `kind` (`--kind claude` finds
/// `claude-code`), else the id spelled like the kind.
///
/// # Errors
///
/// [`KindLaunchError::Ambiguous`] when several integrations claim `kind`;
/// [`KindLaunchError::Resolve`] for any [`resolve_launch`] failure.
pub fn resolve_launch_for_kind(
    config_path: &Path,
    explicit_id: Option<&str>,
    kind: &str,
    extra_args: &[String],
    workspace_cwd: &Path,
) -> Result<ResolvedLaunch, KindLaunchError> {
    let loaded = match load_templates(config_path) {
        Ok(loaded) => loaded,
        Err(source) => {
            return Err(KindLaunchError::Resolve {
                integration_id: explicit_id.unwrap_or(kind).to_owned(),
                source,
            });
        }
    };
    let integration_id = match explicit_id {
        Some(explicit) => explicit.to_owned(),
        None => match integration_for_kind(kind, &launchable(&loaded)) {
            KindClaim::Unique(id) => id,
            KindClaim::Unclaimed => kind.to_owned(),
            KindClaim::Ambiguous(claimants) => {
                return Err(KindLaunchError::Ambiguous {
                    kind: kind.to_owned(),
                    claimants,
                });
            }
        },
    };
    resolve_loaded(loaded, &integration_id, extra_args, workspace_cwd).map_err(|source| {
        KindLaunchError::Resolve {
            integration_id,
            source,
        }
    })
}

/// Failure resolving a launch from a detection kind. Exhaustive on purpose:
/// its consumer maps every variant to a refusal.
#[derive(Debug, thiserror::Error)]
pub enum KindLaunchError {
    /// More than one enabled integration claims the kind — a default this
    /// refuses to guess between.
    #[error("kind {kind:?} is claimed by more than one enabled integration: {}", claimants.join(", "))]
    Ambiguous {
        /// The requested detection kind.
        kind: String,
        /// Sorted, deduped ids of every claimant.
        claimants: Vec<String>,
    },
    /// The integration id was decided, and resolving it failed.
    #[error("could not resolve integration {integration_id:?}: {source}")]
    Resolve {
        /// The id that was resolved (explicit, claimed, or the kind itself).
        integration_id: String,
        /// The underlying resolution failure.
        source: LaunchError,
    },
}

/// How the enabled launchable integrations map onto one requested kind.
#[derive(Debug, PartialEq, Eq)]
pub enum KindClaim {
    /// Exactly one enabled integration's `[agent_identity] kind` matches.
    Unique(String),
    /// No enabled integration claims the kind.
    Unclaimed,
    /// More than one enabled integration claims it (ids sorted, deduped).
    Ambiguous(Vec<String>),
}

/// Do two kind slugs name the same kind (ignoring surrounding space and
/// ASCII case)? Every kind comparison goes through here.
#[must_use]
pub fn kind_matches(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// Match a detection kind against each integration's `[agent_identity] kind`.
///
/// A category never matches. Identical ids collapse: one id shipped twice
/// is [`LaunchError::DuplicateIntegrationId`], not an ambiguity.
#[must_use]
pub fn integration_for_kind(kind: &str, launchable: &[LaunchableIntegration]) -> KindClaim {
    let mut claims: Vec<String> = launchable
        .iter()
        .filter(|item| {
            item.agent_identity
                .as_ref()
                .and_then(|identity| identity.kind.as_deref())
                .is_some_and(|claimed| kind_matches(claimed, kind))
        })
        .map(|item| item.integration_id.clone())
        .collect();
    claims.sort_unstable();
    claims.dedup();
    match claims.len() {
        0 => KindClaim::Unclaimed,
        1 => KindClaim::Unique(claims.remove(0)),
        _ => KindClaim::Ambiguous(claims),
    }
}

/// Every launchable integration of the enabled plugins, in config then
/// filename order; unparseable templates are skipped.
///
/// # Errors
///
/// When the config, a manifest, or an `integrations/` directory fails.
pub fn list_launchable(config_path: &Path) -> Result<Vec<LaunchableIntegration>, LaunchError> {
    Ok(launchable(&load_templates(config_path)?))
}

/// [`list_launchable`] over an already-walked plugin tree.
fn launchable(loaded: &[LoadedTemplate]) -> Vec<LaunchableIntegration> {
    loaded
        .iter()
        .filter_map(|entry| {
            let template = entry.template.as_ref().ok()?;
            let launch = template.launch.as_ref()?;
            if !missing_executables(launch).is_empty() {
                return None;
            }
            Some(LaunchableIntegration {
                plugin_id: entry.plugin_id.clone(),
                integration_id: template.id.clone(),
                display_name: template.display_name.clone(),
                kind: template.kind.clone(),
                agent_identity: template.agent_identity.clone(),
            })
        })
        .collect()
}

fn missing_executables(launch: &IntegrationLaunch) -> Vec<String> {
    let path = std::env::var_os("PATH");
    launch
        .required_executables
        .iter()
        .filter(|name| !executable_on_path(name, path.as_deref()))
        .cloned()
        .collect()
}

fn executable_on_path(name: &str, path: Option<&OsStr>) -> bool {
    path.is_some_and(|value| {
        std::env::split_paths(value)
            .map(|dir| dir.join(name))
            .any(|candidate| is_executable(&candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn build_resolved(
    plugin_id: &str,
    plugin_root: &Path,
    template: &IntegrationTemplate,
    launch: &IntegrationLaunch,
    extra_args: &[String],
    workspace_cwd: &Path,
) -> ResolvedLaunch {
    let argv = integration::expand_launch_argv(&launch.command, plugin_root, extra_args);
    let cwd = match launch.working_directory {
        LaunchWorkingDirectory::PluginRoot => plugin_root.to_path_buf(),
        LaunchWorkingDirectory::Workspace => workspace_cwd.to_path_buf(),
    };
    ResolvedLaunch {
        plugin_id: plugin_id.to_owned(),
        integration_id: template.id.clone(),
        display_name: template.display_name.clone(),
        argv,
        cwd,
        working_directory: launch.working_directory,
        plugin_root: plugin_root.to_path_buf(),
        session_identity: template.session_identity.clone(),
        agent_identity: template.agent_identity.clone(),
    }
}

/// One integration template as it was found on disk.
struct LoadedTemplate {
    plugin_id: String,
    plugin_root: PathBuf,
    path: PathBuf,
    /// Kept as a `Result`: listing skips a broken template, resolution may
    /// surface its error.
    template: Result<IntegrationTemplate, IntegrationError>,
}

/// Walk every enabled plugin's templates once, in config then filename
/// order.
fn load_templates(config_path: &Path) -> Result<Vec<LoadedTemplate>, LaunchError> {
    let mut out = Vec::new();
    for plugin in enabled_plugins(config_path)? {
        for path in template_paths(&plugin.plugin_root)? {
            let template = integration::load_integration_template(&path);
            out.push(LoadedTemplate {
                plugin_id: plugin.plugin_id.clone(),
                plugin_root: plugin.plugin_root.clone(),
                path,
                template,
            });
        }
    }
    Ok(out)
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

/// A plugin's `integrations/*.toml`, sorted; none when the directory is
/// missing.
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

#[cfg(test)]
mod tests {
    use super::{KindClaim, LaunchableIntegration, integration_for_kind, kind_matches};

    /// A launchable integration with category `terminal-agent` and an
    /// optional `[agent_identity] kind`.
    fn launchable(id: &str, agent_kind: Option<&str>) -> LaunchableIntegration {
        LaunchableIntegration {
            plugin_id: "example.agent-tools".to_owned(),
            integration_id: id.to_owned(),
            display_name: None,
            kind: Some("terminal-agent".to_owned()),
            agent_identity: agent_kind.map(|kind| {
                phux_config::integration::IntegrationAgentIdentity {
                    name: None,
                    kind: Some(kind.to_owned()),
                }
            }),
        }
    }

    #[test]
    fn kind_claims_resolve_uniquely_ambiguously_or_not_at_all() {
        assert!(kind_matches(" CLAUDE ", "claude") && kind_matches("claude", "\tClaude\n"));
        assert!(!kind_matches("claude", "claude-code") && !kind_matches("", "claude"));

        let unique = [
            launchable("claude-code", Some("claude")),
            launchable("codex", Some("codex")),
            launchable("bare", None),
        ];
        let two = [
            launchable("claude-fork", Some("claude")),
            launchable("claude-code", Some("claude")),
        ];
        let duplicated = [
            launchable("claude-code", Some("claude")),
            launchable("claude-code", Some("claude")),
        ];
        let claude = || KindClaim::Unique("claude-code".to_owned());
        assert_eq!(integration_for_kind(" CLAUDE ", &unique), claude());
        // The category `kind` is never a claim.
        assert_eq!(
            integration_for_kind("terminal-agent", &unique),
            KindClaim::Unclaimed
        );
        assert_eq!(integration_for_kind("bare", &unique), KindClaim::Unclaimed);
        assert_eq!(integration_for_kind("claude", &[]), KindClaim::Unclaimed);
        assert_eq!(
            integration_for_kind("claude", &two),
            KindClaim::Ambiguous(vec!["claude-code".to_owned(), "claude-fork".to_owned()])
        );
        assert_eq!(integration_for_kind("claude", &duplicated), claude());
    }
}
