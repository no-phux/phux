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
    mode: LaunchMode,
) -> Result<ResolvedLaunch, LaunchError> {
    resolve_loaded(
        load_templates(config_path)?,
        integration_id,
        extra_args,
        workspace_cwd,
        mode,
    )
}

/// [`resolve_launch`] over an already-walked plugin tree.
fn resolve_loaded(
    loaded: Vec<LoadedTemplate>,
    integration_id: &str,
    extra_args: &[String],
    workspace_cwd: &Path,
    mode: LaunchMode,
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
        let launch = selected_launch(&template, mode);
        let missing = launch.map(missing_executables).unwrap_or_default();
        if launch.is_some() && missing.is_empty() {
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
        let outcome = match (launch, missing.is_empty()) {
            (Some(launch), true) => Ok(build_resolved(
                &entry.plugin_id,
                &entry.plugin_root,
                &template,
                launch,
                extra_args,
                workspace_cwd,
                mode,
            )),
            (_, false) => Err(LaunchError::MissingExecutables { name, missing }),
            (None, true) if mode == LaunchMode::Native => Err(LaunchError::NoNativeLaunch { name }),
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

fn selected_launch(template: &IntegrationTemplate, mode: LaunchMode) -> Option<&IntegrationLaunch> {
    match mode {
        LaunchMode::Terminal => template.launch.as_ref(),
        LaunchMode::Native => template.native.as_ref().map(|native| &native.launch),
    }
}

fn native_execution(template: &IntegrationTemplate) -> Option<NativeExecution> {
    template.native.as_ref().map(|native| NativeExecution {
        backend: native.backend,
        provider: native.provider.clone(),
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
    mode: LaunchMode,
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
        None => match integration_for_kind(kind, &launchable(&loaded, mode)) {
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
    resolve_loaded(loaded, &integration_id, extra_args, workspace_cwd, mode).map_err(|source| {
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
pub fn list_launchable(
    config_path: &Path,
    mode: LaunchMode,
) -> Result<Vec<LaunchableIntegration>, LaunchError> {
    Ok(launchable(&load_templates(config_path)?, mode))
}

/// [`list_launchable`] over an already-walked plugin tree.
fn launchable(loaded: &[LoadedTemplate], mode: LaunchMode) -> Vec<LaunchableIntegration> {
    loaded
        .iter()
        .filter_map(|entry| {
            let template = entry.template.as_ref().ok()?;
            let launch = selected_launch(template, mode)?;
            let missing_executables = missing_executables(launch);
            if mode == LaunchMode::Terminal && !missing_executables.is_empty() {
                return None;
            }
            Some(LaunchableIntegration {
                plugin_id: entry.plugin_id.clone(),
                integration_id: template.id.clone(),
                display_name: template.display_name.clone(),
                kind: template.kind.clone(),
                agent_identity: template.agent_identity.clone(),
                native: native_execution(template),
                missing_executables,
            })
        })
        .collect()
}

fn missing_executables(launch: &IntegrationLaunch) -> Vec<String> {
    let path = std::env::var_os("PATH");
    launch
        .required_executables
        .iter()
        .filter(|name| executable_at_path(name, path.as_deref()).is_none())
        .cloned()
        .collect()
}

/// Resolve the executable that will actually run on this host.
#[must_use]
pub fn executable_path(name: &str) -> Option<PathBuf> {
    executable_at_path(name, std::env::var_os("PATH").as_deref())
}

fn executable_at_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return is_executable(direct)
            .then(|| direct.canonicalize().ok())
            .flatten();
    }
    std::env::split_paths(path?)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .and_then(|candidate| candidate.canonicalize().ok())
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
    mode: LaunchMode,
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
        native: if mode == LaunchMode::Native {
            native_execution(template)
        } else {
            None
        },
    }
}
