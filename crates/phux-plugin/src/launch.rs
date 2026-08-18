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
    self, IntegrationAgentIdentity, IntegrationError, IntegrationLaunch,
    IntegrationSessionIdentity, IntegrationTemplate, LaunchWorkingDirectory, SessionResumeError,
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
    /// The launched agent's self-declared identity, when the template
    /// carries an `[agent_identity]` section. Its `kind` is the
    /// detection-manifest slug, not the template's category `kind`.
    pub agent_identity: Option<IntegrationAgentIdentity>,
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
    /// Kind slug, when declared. A package *category* (`terminal-agent`),
    /// not the detection slug — that lives in `agent_identity`.
    pub kind: Option<String>,
    /// The launched agent's self-declared identity, when the template
    /// carries an `[agent_identity]` section.
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
    let mut matched_owner: Option<String> = None;
    let mut resolved: Option<ResolvedLaunch> = None;
    for entry in loaded {
        let template = match entry.template {
            Ok(template) => template,
            Err(source) => {
                // Surface the error only when this is the file the
                // caller asked for (by filename stem); a broken sibling
                // template must not block launching a healthy one.
                if entry.path.file_stem().and_then(|s| s.to_str()) == Some(integration_id) {
                    return Err(LaunchError::Template {
                        path: entry.path,
                        source,
                    });
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
                second: entry.plugin_id,
            });
        }
        matched_owner = Some(entry.plugin_id.clone());
        if let Some(launch) = template.launch.clone() {
            resolved = Some(build_resolved(
                &entry.plugin_id,
                &entry.plugin_root,
                &template,
                &launch,
                extra_args,
                workspace_cwd,
            ));
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

/// Resolve the integration a `--kind` starts, and build its argv, from one
/// walk of the enabled plugin tree.
///
/// The integration id and the detection kind are different namespaces: a
/// template's own `kind` is a category (`terminal-agent`); the detection slug
/// lives in its `[agent_identity]` block. With no explicit id, the
/// integration is therefore the unique enabled one whose `[agent_identity]
/// kind` claims `kind` (`--kind claude` resolves `claude-code` with no second
/// flag); two claimants are refused by name rather than picked between, and
/// no claimant falls back to the id spelled like the kind, which is the
/// pre-`agent_identity` default.
///
/// One entry point rather than [`list_launchable`] followed by
/// [`resolve_launch`], because that pair walked the whole tree twice — config
/// read and parse, every enabled plugin's manifest, every `integrations/`
/// directory, and every template file, twice — on exactly the default path
/// this resolution exists to serve.
///
/// # Errors
///
/// Returns [`KindLaunchError::Ambiguous`] when more than one enabled
/// integration claims `kind`, and [`KindLaunchError::Resolve`] for every
/// failure [`resolve_launch`] reports, naming the id that was resolved.
pub fn resolve_launch_for_kind(
    config_path: &Path,
    explicit_id: Option<&str>,
    kind: &str,
    extra_args: &[String],
    workspace_cwd: &Path,
) -> Result<ResolvedLaunch, KindLaunchError> {
    // A tree that cannot be walked is reported against the id the caller
    // asked for, or the one the kind would have fallen back to — the same
    // diagnosis the pre-single-walk code produced one step later.
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

/// Failure resolving a launch from a detection kind.
///
/// Exhaustive on purpose, unlike [`LaunchError`]: its one consumer maps every
/// variant onto a refusal with its own code and remedy, so a variant added
/// without a mapping should be a compile error there rather than a silent
/// fall-through to a generic message.
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

/// Do two kind slugs name the same kind?
///
/// The whole tolerance, in one place: surrounding whitespace and ASCII case
/// are insignificant. Every comparison of a requested kind against a declared
/// one goes through here — the `[agent_identity]` claim match below and the
/// CLI's readiness verdict — so "these two stay in step" is a fact the
/// compiler keeps rather than a comment two functions promise each other.
#[must_use]
pub fn kind_matches(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// Match a detection kind against each launchable integration's
/// `[agent_identity] kind`.
///
/// A template's top-level `kind` (a category such as `terminal-agent`)
/// deliberately never matches. Identical ids are collapsed before counting:
/// one id shipped by two plugins is [`resolve_launch`]'s
/// [`LaunchError::DuplicateIntegrationId`] failure, not an ambiguity between
/// two genuine choices a refusal could name.
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
    Ok(launchable(&load_templates(config_path)?))
}

/// [`list_launchable`] over an already-walked plugin tree.
fn launchable(loaded: &[LoadedTemplate]) -> Vec<LaunchableIntegration> {
    loaded
        .iter()
        .filter_map(|entry| {
            let template = entry.template.as_ref().ok()?;
            template.launch.as_ref()?;
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
    /// The parse outcome, kept rather than discarded at the walk: listing
    /// skips a broken template, while resolution surfaces its error when it
    /// is the file the caller named. One walk has to serve both.
    template: Result<IntegrationTemplate, IntegrationError>,
}

/// Walk every enabled plugin's `integrations/` directory once, parsing each
/// template exactly once — config order, then sorted filename order.
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

#[cfg(test)]
mod tests {
    use super::{KindClaim, LaunchableIntegration, integration_for_kind, kind_matches};

    /// A launchable integration as [`super::list_launchable`] surfaces it:
    /// the category `kind` is always `terminal-agent`, and `agent_kind` (when
    /// given) rides the `[agent_identity]` block.
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

    /// The one tolerance rule, shared with the CLI's readiness verdict so the
    /// two can never disagree about whether a kind matches.
    #[test]
    fn kind_matching_ignores_surrounding_space_and_ascii_case() {
        assert!(kind_matches("claude", "claude"));
        assert!(kind_matches(" CLAUDE ", "claude"));
        assert!(kind_matches("claude", "\tClaude\n"));
        assert!(!kind_matches("claude", "claude-code"));
        assert!(!kind_matches("", "claude"));
    }

    /// The map this resolution exists for: `--kind claude` finds
    /// `claude-code` through its `[agent_identity] kind`. The category `kind`
    /// (`terminal-agent`, on every template) never matches.
    #[test]
    fn a_unique_agent_identity_claim_resolves_the_integration() {
        let launchables = [
            launchable("claude-code", Some("claude")),
            launchable("codex", Some("codex")),
            launchable("generic-shell-agent", Some("generic")),
        ];
        assert_eq!(
            integration_for_kind("claude", &launchables),
            KindClaim::Unique("claude-code".to_owned())
        );
        assert_eq!(
            integration_for_kind(" CLAUDE ", &launchables),
            KindClaim::Unique("claude-code".to_owned())
        );
        // `terminal-agent` is every template's category, never a claim.
        assert_eq!(
            integration_for_kind("terminal-agent", &launchables),
            KindClaim::Unclaimed
        );
    }

    /// Two enabled integrations claiming one kind is an ambiguity naming
    /// both, rather than a pick — the claimants ride the answer so the
    /// caller's refusal can print them.
    #[test]
    fn an_ambiguous_kind_claim_names_every_claimant() {
        let launchables = [
            launchable("claude-fork", Some("claude")),
            launchable("claude-code", Some("claude")),
            launchable("codex", Some("codex")),
        ];
        assert_eq!(
            integration_for_kind("claude", &launchables),
            KindClaim::Ambiguous(vec!["claude-code".to_owned(), "claude-fork".to_owned()])
        );
        // One id shipped twice is `resolve_launch`'s DuplicateIntegrationId
        // failure, not an ambiguity between two genuine choices.
        let duplicated = [
            launchable("claude-code", Some("claude")),
            launchable("claude-code", Some("claude")),
        ];
        assert_eq!(
            integration_for_kind("claude", &duplicated),
            KindClaim::Unique("claude-code".to_owned())
        );
    }

    /// A template with no `[agent_identity]` block claims nothing, and an
    /// empty listing claims nothing — both leave the caller on the
    /// id-spelled-like-the-kind fallback.
    #[test]
    fn a_kind_no_template_claims_is_unclaimed() {
        let launchables = [
            launchable("claude-code", None),
            launchable("codex", Some("codex")),
        ];
        assert_eq!(
            integration_for_kind("claude", &launchables),
            KindClaim::Unclaimed
        );
        assert_eq!(integration_for_kind("claude", &[]), KindClaim::Unclaimed);
    }
}
