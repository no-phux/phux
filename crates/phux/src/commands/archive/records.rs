/// What [`create_imported_session`] made besides the session.
struct ImportOutcome {
    /// Panes imported as a plain shell instead of their archived agent.
    warnings: Vec<String>,
}

enum ImportFailure {
    NameTaken,
    Other(String),
}

impl From<String> for ImportFailure {
    fn from(reason: String) -> Self {
        Self::Other(reason)
    }
}

impl ImportFailure {
    fn message(self) -> String {
        match self {
            Self::NameTaken => format!(
                "the server refused it: {}",
                phux_client::org::refusal_sentence(&phux_protocol::wire::frame::Refusal::NameTaken,)
            ),
            Self::Other(reason) => reason,
        }
    }
}

/// Import every archived pane as inert records. Archived host placement is
/// descriptive data, not authorization to execute locally or on a satellite.
async fn create_imported_session(
    socket_path: &Path,
    create: &model::CreateRequest,
    kind: ImportKind<'_>,
    extra: &[(String, String)],
) -> Result<ImportOutcome, ImportFailure> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| format!("could not reach the server: {err}"))?;
    let mut notices = Vec::new();
    let mut attrs = session_attrs(create, kind);
    attrs.extend(extra.iter().cloned());
    let outcome = phux_client::session::create_empty_session_with_attrs(
        &mut conn,
        &create.name,
        &attrs,
        &mut notices,
    )
    .await
    .map_err(|err| format!("could not create the session: {err}"))?;
    for notice in notices {
        warn!("partial results — {notice}");
    }
    match outcome {
        phux_client::session::CreateEmptyOutcome::Created => {}
        phux_client::session::CreateEmptyOutcome::Refused(
            phux_protocol::wire::frame::Refusal::NameTaken,
        ) => return Err(ImportFailure::NameTaken),
        phux_client::session::CreateEmptyOutcome::Refused(refusal) => {
            return Err(ImportFailure::Other(format!(
                "the server refused it: {}",
                phux_client::org::refusal_sentence(&refusal)
            )));
        }
        other => {
            return Err(ImportFailure::Other(format!(
                "the server did not create it: {other:?}"
            )));
        }
    }
    if matches!(kind, ImportKind::Archive) {
        tag_session(socket_path, create).await?;
    }
    let warnings = import_dormant_panes(&mut conn, create).await;
    drop(conn);
    let warnings = warnings?;
    Ok(ImportOutcome {
        warnings,
    })
}

/// What one archived pane runs: its command, directory, environment, and
/// native agent session. The seed pane's come from the session's plan.
struct PaneSource<'a> {
    command: Option<&'a Vec<String>>,
    cwd: Option<&'a str>,
    env: &'a std::collections::BTreeMap<String, String>,
    agent_session: Option<&'a model::WorkspaceAgentSession>,
    native_launch: Option<&'a phux_protocol::native_agent::SpawnNativeAgent>,
}

impl<'a> PaneSource<'a> {
    fn seed(create: &'a model::CreateRequest) -> Self {
        Self {
            command: create.command.as_ref(),
            cwd: create.cwd.as_deref(),
            env: &create.env,
            agent_session: create.agent_session.as_ref(),
            native_launch: create.native_launch.as_ref(),
        }
    }

    fn pane(pane: &'a model::WorkspacePane) -> Self {
        Self {
            command: pane.command.as_ref(),
            cwd: pane.cwd.as_deref(),
            env: &pane.env,
            agent_session: pane.agent_session.as_ref(),
            native_launch: pane.native_launch.as_ref(),
        }
    }
}

/// Preserve complete archived intent as inert data. Current templates are
/// resolved only by the executing host at admission, never during import.
fn pane_launch(
    source: &PaneSource<'_>,
    session: &str,
    _warnings: &mut Vec<String>,
) -> Result<LaunchSpec, String> {
    if let Some(native) = source.native_launch {
        let mut spec = native.clone();
        session.clone_into(&mut spec.session);
        spec.guardian = None;
        spec.guardian_key = None;
        spec.guardian_expected = None;
        let marker = serde_json::to_string(&spec)
            .map_err(|error| format!("could not encode native launch: {error}"))?;
        return Ok(LaunchSpec::new(Vec::new())
            .with_cwd(source.cwd.map(str::to_owned))
            .with_env(vec![("PHUX_NATIVE_HOST_SPEC".to_owned(), marker)]));
    }
    let restore = source.agent_session.map(|record| {
        phux_client::agent_session_record::AgentSessionRecord::new(
            &record.plugin_id, &record.integration_id, &record.native_id,
        )?.encode()
    }).transpose()?;
    Ok(LaunchSpec::new(source.command.cloned().unwrap_or_default())
        .with_cwd(source.cwd.map(str::to_owned))
        .with_env(source.env.clone().into_iter().collect())
        .with_restore(restore))
}

/// Create every archived pane of the imported session `create` as a dormant
/// resource record and place them in its arrangement as the archive laid
/// them out. A failure removes the records made and deletes the session, so
/// a failed import leaves nothing behind. Returns the agent fallback
/// warnings.
async fn import_dormant_panes(
    conn: &mut Connection,
    create: &model::CreateRequest,
) -> Result<Vec<String>, String> {
    let session = phux_client::org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(|err| format!("could not read the session records: {err}"))?
        .session_named(&create.name)
        .map(|record| record.key)
        .ok_or_else(|| format!("session {:?} is gone", create.name))?;
    let mut warnings = Vec::new();
    let mut made = Vec::new();
    let result = match create_dormant_records(conn, session, create, &mut made, &mut warnings).await
    {
        Ok(keys) if keys.iter().all(Vec::is_empty) => Ok(()),
        Ok(keys) => {
            let document = imported_arrangement(&create.windows, &keys);
            phux_client::org::replace_session_arrangement(conn, session, &document)
                .await
                .map(drop)
                .map_err(|err| format!("could not arrange the imported panes: {err}"))
        }
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        for key in made {
            let _ = phux_client::org::remove_resource(conn, key).await;
        }
        let _ = phux_client::org::delete_session(conn, session).await;
        return Err(err);
    }
    Ok(warnings)
}

/// Create the dormant records, one row of keys per archived window. A
/// session with no windows but a seed command or agent gets one seed pane.
async fn create_dormant_records(
    conn: &mut Connection,
    session: SessionKey,
    create: &model::CreateRequest,
    made: &mut Vec<ResourceKey>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Vec<ResourceKey>>, String> {
    let mut rows: Vec<Vec<PaneSource<'_>>> = create
        .windows
        .iter()
        .enumerate()
        .map(|(window_index, window)| {
            window
                .panes
                .iter()
                .enumerate()
                .map(|(pane_index, pane)| {
                    if create.seed_position == Some((window_index, pane_index)) {
                        PaneSource::seed(create)
                    } else {
                        PaneSource::pane(pane)
                    }
                })
                .collect()
        })
        .collect();
    if create.windows.is_empty()
        && (create.command.is_some()
            || create.agent_session.is_some()
            || create.native_launch.is_some())
    {
        rows.push(vec![PaneSource::seed(create)]);
    }
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows {
        let mut row_keys = Vec::with_capacity(row.len());
        for source in row {
            let launch = pane_launch(&source, &create.name, warnings)?;
            let committed = phux_client::org::create_resource(
                conn,
                session,
                ResourceKind::Terminal,
                Some(launch),
                LaunchMode::Deferred,
            )
            .await
            .map_err(|err| format!("could not import an archived pane: {err}"))?;
            let key = committed
                .resource
                .ok_or_else(|| "the server created no pane record".to_owned())?;
            made.push(key);
            row_keys.push(key);
        }
        keys.push(row_keys);
    }
    Ok(keys)
}

/// The imported session's arrangement: one group per archived window with
/// panes, its archived split tree mapped onto that window's `keys` (a linear
/// chain without a captured layout, or for a corrupt one).
fn imported_arrangement(windows: &[WorkspaceWindow], keys: &[Vec<ResourceKey>]) -> WireDoc {
    let groups = keys
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let window = windows.get(index);
            let tree = window
                .and_then(|window| window.layout.as_ref())
                .and_then(|node| wire_node_from_archive(node, row))
                .or_else(|| wire_chain(row))?;
            let name = window.map(|window| window.name.clone());
            let name = name.filter(|name| !name.is_empty());
            Some(ArrangementGroup::new(
                phux_client::layout::fresh_id(),
                name,
                tree,
            ))
        })
        .collect();
    WireDoc { groups }
}

fn wire_leaf(key: ResourceKey) -> ArrangementNode {
    ArrangementNode::Leaf(EntryRef::new(
        phux_client::layout::fresh_id(),
        EntryTarget::Member(key),
    ))
}

/// Map an archived split tree onto dormant resource keys.
fn wire_node_from_archive(
    node: &model::WorkspaceLayoutNode,
    keys: &[ResourceKey],
) -> Option<ArrangementNode> {
    match node {
        model::WorkspaceLayoutNode::Pane { pane } => keys.get(*pane).copied().map(wire_leaf),
        model::WorkspaceLayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Some(ArrangementNode::Split {
            dir: match dir {
                model::WorkspaceSplitDir::Horizontal => SplitDirection::Horizontal,
                model::WorkspaceSplitDir::Vertical => SplitDirection::Vertical,
            },
            ratio: ratio.clamp(0.05, 0.95),
            first: Box::new(wire_node_from_archive(left, keys)?),
            second: Box::new(wire_node_from_archive(right, keys)?),
        }),
    }
}

/// [`linear_chain`] in key space; `None` for no keys.
fn wire_chain(keys: &[ResourceKey]) -> Option<ArrangementNode> {
    let mut rest = keys.iter().rev();
    let mut tree = wire_leaf(*rest.next()?);
    for key in rest {
        tree = ArrangementNode::Split {
            dir: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(wire_leaf(*key)),
            second: Box::new(tree),
        };
    }
    Some(tree)
}

/// Relaunch the dormant panes of the imported session `name` in place.
async fn relaunch_dormant_session(
    socket_path: &Path,
    name: &str,
) -> Result<SessionOutcome, String> {
    let results = phux_client::org::restore_session_on(socket_path, name)
        .await
        .map_err(|err| format!("could not relaunch its panes: {err}"))?;
    let refused: Vec<String> = results
        .into_iter()
        .filter_map(|(key, result)| result.err().map(|err| format!("{key}: {err}")))
        .collect();
    if refused.is_empty() {
        Ok(SessionOutcome {
            warnings: Vec::new(),
        })
    } else {
        Err(format!(
            "could not relaunch its panes: {}",
            refused.join("; ")
        ))
    }
}

/// Attributes belong to the session record, not the legacy singleton tags.
fn session_attrs(create: &model::CreateRequest, kind: ImportKind<'_>) -> Vec<(String, String)> {
    let project_root = match kind {
        ImportKind::Archive => None,
        ImportKind::Project { directory } => Some(directory.to_string_lossy().into_owned()),
    };
    [
        ("host", create.host.as_deref()),
        ("project", create.project.as_deref()),
        (
            phux_core::project_identity::attr::LINK,
            create.project_link.as_deref(),
        ),
        (
            "directory",
            project_root.as_deref().or(create.directory.as_deref()),
        ),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        value
            .filter(|value| !value.is_empty())
            .map(|value| (key.to_owned(), value.to_owned()))
    })
    .collect()
}

/// Write a recreated session's archived host, project, and directory.
async fn tag_session(socket_path: &Path, create: &model::CreateRequest) -> Result<(), String> {
    if let Some(host) = create.host.as_deref().filter(|host| !host.is_empty()) {
        phux_client::workspace_place::set_session_host(socket_path, &create.name, host)
            .await
            .map_err(|err| err.to_string())?;
    }
    if let Some(project) = create
        .project
        .as_deref()
        .filter(|project| !project.is_empty())
    {
        phux_client::workspace_place::set_session_project(socket_path, &create.name, project)
            .await
            .map_err(|err| err.to_string())?;
    }
    if let Some(directory) = create.directory.as_deref().filter(|cwd| !cwd.is_empty()) {
        phux_client::workspace_place::set_session_directory(socket_path, &create.name, directory)
            .await
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}