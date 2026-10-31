//! Importing the carried tree into a fresh organization.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// Merge a blob's carried records with `current`: records the store already
/// holds win; carried records it lacks are added. With nothing loaded from
/// a store (an in-memory store), the carried authority is adopted whole.
/// Returns the merged organization, the blob's keys by wire id, and `false`
/// (nothing was imported from a pre-record tree).
pub(in crate::state) fn merge_carried(
    current: &Org,
    loaded: bool,
    carried: &OrgBlob,
    blob: &StateBlob,
    instance: phux_core::InstanceId,
) -> Result<(Org, Keyed, bool), RebuildError> {
    let org = if loaded {
        let mut sessions: HashMap<SessionKey, Session> =
            current.sessions().map(|s| (s.key, s.clone())).collect();
        let mut resources: HashMap<ResourceKey, Resource> =
            current.resources().map(|r| (r.key, r.clone())).collect();
        let mut arrangements: HashMap<ArrangementKey, Arrangement> =
            current.arrangements().map(|a| (a.key, a.clone())).collect();
        for s in &carried.sessions {
            if let std::collections::hash_map::Entry::Vacant(slot) = sessions.entry(s.key) {
                slot.insert(s.clone());
                for r in carried.resources.iter().filter(|r| r.session == s.key) {
                    resources.entry(r.key).or_insert_with(|| r.clone());
                }
                if let Some(a) = s
                    .arrangement
                    .and_then(|k| carried.arrangements.iter().find(|a| a.key == k))
                {
                    arrangements.entry(a.key).or_insert_with(|| a.clone());
                }
            }
        }
        Org::restore(
            *current.authority(),
            instance,
            sessions.into_values(),
            resources.into_values(),
            arrangements.into_values(),
            current.receipts().cloned(),
        )?
    } else {
        Org::restore(
            carried.authority,
            instance,
            carried.sessions.iter().cloned(),
            carried.resources.iter().cloned(),
            carried.arrangements.iter().cloned(),
            std::iter::empty(),
        )?
    };
    let mut keyed = Keyed {
        sessions: HashMap::new(),
        panes: HashMap::new(),
        agent_sessions: HashMap::new(),
    };
    for s in &blob.sessions {
        let key = s
            .key
            .or_else(|| org.session_by_name(&s.name).map(|r| r.key))
            .ok_or(RebuildError::DanglingRef {
                kind: "session key",
                id: s.wire_id,
            })?;
        keyed.sessions.insert(s.wire_id, key);
    }
    for p in &blob.panes {
        let key = p.key.ok_or(RebuildError::DanglingRef {
            kind: "pane key",
            id: p.wire_id,
        })?;
        keyed.panes.insert(p.wire_id, key);
    }
    for a in &blob.agent_sessions {
        if let Some(key) = a.key {
            keyed.agent_sessions.insert(a.wire_id, key);
        }
    }
    Ok((org, keyed, false))
}

/// Import a tree from an image that predates the records (MIGRATION §07):
/// each session becomes a record awaiting organization, persistent when it
/// was keep-empty; each window becomes a group of its arrangement; each pane
/// becomes a Terminal bound to its running process as activation 1; each
/// carried agent session becomes a conversation under its pane. A name the
/// current organization already uses is suffixed. Returns the merged
/// organization, keys by wire id, and `true`.
#[allow(clippy::too_many_lines, reason = "one pass per entity kind, in order")]
pub(in crate::state) fn import_tree(
    current: &Org,
    blob: &StateBlob,
    instance: phux_core::InstanceId,
) -> Result<(Org, Keyed, bool), RebuildError> {
    let mut keys = crate::state::OsKeys;
    let now = crate::state::now_stamp();
    let actor = phux_core::Actor::system();
    let operation = OperationId::from_bytes(keys.fresh());
    let mut sessions: Vec<Session> = current.sessions().cloned().collect();
    let mut resources: Vec<Resource> = current.resources().cloned().collect();
    let mut arrangements: Vec<Arrangement> = current.arrangements().cloned().collect();
    let mut keyed = Keyed {
        sessions: HashMap::new(),
        panes: HashMap::new(),
        agent_sessions: HashMap::new(),
    };
    let window_session: HashMap<u32, u32> = blob
        .windows
        .iter()
        .map(|w| (w.wire_id, w.session_wire_id))
        .collect();
    let mut session_keys: HashMap<u32, SessionKey> = HashMap::new();
    for s in &blob.sessions {
        let key = SessionKey::from_bytes(keys.fresh());
        let mut name = s.name.clone();
        while sessions.iter().any(|existing| existing.name == name) {
            name.push_str("-imported");
        }
        sessions.push(Session {
            key,
            name,
            lifetime: if s.keep_empty || s.window_wire_ids.is_empty() {
                SessionLifetime::Persistent
            } else {
                SessionLifetime::Ephemeral
            },
            awaiting_organization: true,
            members: Vec::new(),
            arrangement: None,
            attrs: phux_core::Attrs::default(),
            created: Stamp(
                u64::try_from(s.created_at_unix_nanos / 1_000_000).unwrap_or(now.as_millis()),
            ),
            revision: Revision::FIRST,
            recent: None,
        });
        session_keys.insert(s.wire_id, key);
        keyed.sessions.insert(s.wire_id, key);
    }
    let boot = crate::reconcile::boot_id();
    for p in &blob.panes {
        let session_wire =
            window_session
                .get(&p.window_wire_id)
                .ok_or(RebuildError::DanglingRef {
                    kind: "window",
                    id: p.window_wire_id,
                })?;
        let session = *session_keys
            .get(session_wire)
            .ok_or(RebuildError::DanglingRef {
                kind: "session",
                id: *session_wire,
            })?;
        let key = ResourceKey::from_bytes(keys.fresh());
        let host_proc = p
            .child_pid
            .and_then(crate::reconcile::proc_identity)
            .filter(|id| Some(id.boot_id) == boot);
        let state = if p.retained_exit.is_some() {
            ExecState::Completed
        } else {
            ExecState::Running
        };
        let mut activation = Activation::starting(ActivationId::FIRST, instance, now);
        activation.state = state;
        activation.host_proc = host_proc;
        resources.push(Resource {
            key,
            session,
            name: None,
            kind: ResourceKind::Terminal,
            provenance: Provenance {
                created_by: actor.clone(),
                operation,
                parent: None,
            },
            launch: Some(LaunchSpec::Terminal(TerminalLaunch {
                argv: Vec::new(),
                cwd: Some(p.cwd.clone()),
                env: Vec::new(),
                restore: None,
            })),
            on_exit: if p.retain_secs.is_some() || p.retained_exit.is_some() {
                ExitPolicy::Retain
            } else {
                ExitPolicy::Remove
            },
            durable_result: false,
            activation: Some(activation),
            revision: Revision::FIRST,
        });
        if let Some(record) = sessions.iter_mut().find(|s| s.key == session) {
            record.members.push(key);
        }
        keyed.panes.insert(p.wire_id, key);
    }
    for a in &blob.agent_sessions {
        let (Some(parent), Some(session)) = (
            keyed.panes.get(&a.parent_wire_id).copied(),
            resources
                .iter()
                .find(|r| keyed.panes.get(&a.parent_wire_id) == Some(&r.key))
                .map(|r| r.session),
        ) else {
            continue;
        };
        let key = ResourceKey::from_bytes(keys.fresh());
        let mut activation = Activation::starting(ActivationId::FIRST, instance, now);
        activation.state = ExecState::Running;
        resources.push(Resource {
            key,
            session,
            name: None,
            kind: ResourceKind::AgentSession,
            provenance: Provenance {
                created_by: actor.clone(),
                operation,
                parent: Some(parent),
            },
            launch: Some(LaunchSpec::Agent(AgentLaunch {
                provider: a.provider.clone(),
                native_id: a.native_id.clone(),
            })),
            on_exit: ExitPolicy::Remove,
            durable_result: false,
            activation: Some(activation),
            revision: Revision::FIRST,
        });
        if let Some(record) = sessions.iter_mut().find(|s| s.key == session) {
            record.members.push(key);
        }
        keyed.agent_sessions.insert(a.wire_id, key);
    }
    // Each window becomes a group, in the session's window order.
    for s in &blob.sessions {
        let session = session_keys[&s.wire_id];
        let mut groups = Vec::new();
        for window in s
            .window_wire_ids
            .iter()
            .filter_map(|id| blob.windows.iter().find(|w| w.wire_id == *id))
        {
            let tree = match &window.layout {
                Some(layout) => group_tree(layout, &keyed.panes, &mut keys)?,
                None => chain(&window.pane_wire_ids, &keyed.panes, &mut keys),
            };
            if let Some(tree) = tree {
                groups.push(Group {
                    id: keys.fresh(),
                    name: None,
                    tree,
                });
            }
        }
        if groups.is_empty() {
            continue;
        }
        let key = ArrangementKey::from_bytes(keys.fresh());
        arrangements.push(Arrangement {
            key,
            owner: ArrangementOwner::Session(session),
            groups,
            revision: Revision::FIRST,
        });
        if let Some(record) = sessions.iter_mut().find(|r| r.key == session) {
            record.arrangement = Some(key);
        }
    }
    // An old session can list windows whose panes did not survive; with no
    // members it cannot be ephemeral, so it stays as a persistent record.
    for session in &mut sessions {
        if session.members.is_empty() {
            session.lifetime = SessionLifetime::Persistent;
        }
    }
    let mut org = Org::restore(
        *current.authority(),
        instance,
        sessions,
        resources,
        arrangements,
        current.receipts().cloned(),
    )?;
    crate::state::org::ensure_arrangements(&mut org);
    Ok((org, keyed, true))
}

/// A window's split tree with entry leaves, from its blob mirror.
pub(in crate::state) fn group_tree(
    node: &LayoutBlob,
    panes: &HashMap<u32, ResourceKey>,
    keys: &mut crate::state::OsKeys,
) -> Result<Option<LayoutNode<EntryRef>>, RebuildError> {
    Ok(match node {
        LayoutBlob::Leaf(wire) => {
            let key = panes.get(wire).ok_or(RebuildError::DanglingRef {
                kind: "pane",
                id: *wire,
            })?;
            Some(LayoutNode::Leaf(EntryRef::member(keys.fresh(), *key)))
        }
        LayoutBlob::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            let left = group_tree(left, panes, keys)?;
            let right = group_tree(right, panes, keys)?;
            match (left, right) {
                (Some(l), Some(r)) => Some(LayoutNode::Split {
                    dir: match dir {
                        SplitDirBlob::Horizontal => SplitDir::Horizontal,
                        SplitDirBlob::Vertical => SplitDir::Vertical,
                    },
                    ratio: crate::state::layout_translation::arrangement_ratio(*ratio),
                    left: Box::new(l),
                    right: Box::new(r),
                }),
                (only, None) | (None, only) => only,
            }
        }
    })
}

/// A window without a layout: its panes side by side, each split giving the
/// remainder an equal share.
pub(in crate::state) fn chain(
    wires: &[u32],
    panes: &HashMap<u32, ResourceKey>,
    keys: &mut crate::state::OsKeys,
) -> Option<LayoutNode<EntryRef>> {
    let leaves: Vec<ResourceKey> = wires.iter().filter_map(|w| panes.get(w).copied()).collect();
    let mut tree: Option<LayoutNode<EntryRef>> = None;
    for (index, key) in leaves.iter().enumerate().rev() {
        let leaf = LayoutNode::Leaf(EntryRef::member(keys.fresh(), *key));
        tree = Some(match tree {
            None => leaf,
            Some(rest) => {
                let remaining = u16::try_from(leaves.len() - index).unwrap_or(u16::MAX);
                LayoutNode::Split {
                    dir: SplitDir::Horizontal,
                    ratio: 1.0 / f32::from(remaining),
                    left: Box::new(leaf),
                    right: Box::new(rest),
                }
            }
        });
    }
    tree
}

/// Build a pane actor around its inherited PTY, or a PTY-less actor that
/// replays its snapshot. Only one of the two PTY ids is corruption.
pub(in crate::state) fn pane_actor_bundle(
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
) -> Result<crate::terminal_actor::TerminalActorBundle, RebuildError> {
    let seed = pane_seed(p);
    Ok(match (p.master_fd, p.child_pid) {
        (Some(master_fd), Some(child_pid)) => TerminalActor::new_with_adopted_pty(
            master_fd,
            child_pid,
            p.cols,
            p.rows,
            scrollback,
            CancellationToken::new(),
            &seed,
        )?,
        (None, None) => TerminalActor::new_with_seed(p.cols, p.rows, &seed)?,
        _ => {
            return Err(RebuildError::InvalidPtyPair { wire_id: p.wire_id });
        }
    })
}
