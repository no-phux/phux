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
    // A declared OpenCode profile always wins, including broken templates
    // named opencode.toml. Never hide an explicit error behind the fallback.
    if !out.iter().any(|entry| {
        template_file_names(&entry.path, "opencode")
            || entry
                .template
                .as_ref()
                .is_ok_and(|template| template.id == "opencode")
    }) {
        out.push(builtin_opencode());
    }
    Ok(out)
}

fn builtin_opencode() -> LoadedTemplate {
    let executable = opencode_executable(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .map_or_else(
        || "opencode".to_owned(),
        |path| path.to_string_lossy().into_owned(),
    );
    LoadedTemplate {
        plugin_id: "phux.builtin".into(),
        plugin_root: PathBuf::new(),
        path: PathBuf::from("opencode.toml"),
        template: Ok(IntegrationTemplate {
            id: "opencode".into(),
            display_name: Some("OpenCode".into()),
            kind: Some("terminal-agent".into()),
            launch: None,
            native: Some(integration::IntegrationNativeLaunch {
                backend: NativeBackend::Acp,
                provider: "opencode".into(),
                launch: IntegrationLaunch {
                    command: vec![executable.clone(), "acp".into()],
                    required_executables: vec![executable],
                    working_directory: LaunchWorkingDirectory::Workspace,
                },
            }),
            template_path: PathBuf::new(),
            session_identity: None,
            agent_identity: Some(IntegrationAgentIdentity {
                name: Some("opencode".into()),
                kind: Some("opencode".into()),
            }),
        }),
    }
}

fn opencode_executable(path: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    executable_at_path("opencode", path).or_else(|| {
        let installed = Path::new(home?).join(".opencode/bin/opencode");
        is_executable(&installed)
            .then(|| installed.canonicalize().ok())
            .flatten()
    })
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
