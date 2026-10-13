//! Conversions between `phux-core`'s organization types and their
//! `phux-protocol` encodings (R02: core does not depend on the protocol).
//!
//! Keys are the same 16 bytes on both sides; a wire key is never all zero,
//! so a core key that is (no minted key is) converts to `None`.

use phux_core::Key16 as _;
use phux_protocol::ids as wire;
use phux_protocol::wire::frame;

/// A core session key on the wire.
#[must_use]
pub fn session_key(key: phux_core::SessionKey) -> Option<wire::SessionKey> {
    wire::SessionKey::new(key.to_bytes())
}

/// A core resource key on the wire.
#[must_use]
pub fn resource_key(key: phux_core::ResourceKey) -> Option<wire::ResourceKey> {
    wire::ResourceKey::new(key.to_bytes())
}

/// A core arrangement key on the wire.
#[must_use]
pub fn arrangement_key(key: phux_core::ArrangementKey) -> Option<wire::ArrangementKey> {
    wire::ArrangementKey::new(key.to_bytes())
}

/// A wire session key in core.
#[must_use]
pub const fn core_session_key(key: &wire::SessionKey) -> phux_core::SessionKey {
    phux_core::SessionKey::from_bytes(*key.as_bytes())
}

/// A wire resource key in core.
#[must_use]
pub const fn core_resource_key(key: &wire::ResourceKey) -> phux_core::ResourceKey {
    phux_core::ResourceKey::from_bytes(*key.as_bytes())
}

/// A wire arrangement key in core.
#[must_use]
pub const fn core_arrangement_key(key: &wire::ArrangementKey) -> phux_core::ArrangementKey {
    phux_core::ArrangementKey::from_bytes(*key.as_bytes())
}

/// A wire operation id in core.
#[must_use]
pub const fn core_operation(op: &wire::OperationId) -> phux_core::OperationId {
    phux_core::OperationId::from_bytes(*op.as_bytes())
}

/// A core operation id on the wire.
#[must_use]
pub fn operation(op: phux_core::OperationId) -> Option<wire::OperationId> {
    wire::OperationId::new(op.to_bytes())
}

/// A revision on the wire.
#[must_use]
pub const fn revision(r: phux_core::Revision) -> wire::Revision {
    wire::Revision::new(r.get())
}

/// A wire revision in core.
#[must_use]
pub const fn core_revision(r: wire::Revision) -> phux_core::Revision {
    phux_core::Revision(r.get())
}

/// An activation id on the wire.
#[must_use]
pub const fn activation_id(a: phux_core::ActivationId) -> wire::ActivationId {
    wire::ActivationId::new(a.get())
}

/// A wire activation id in core.
#[must_use]
pub const fn core_activation_id(a: wire::ActivationId) -> phux_core::ActivationId {
    phux_core::ActivationId(a.get())
}

/// A lifetime on the wire.
#[must_use]
pub const fn lifetime(l: phux_core::SessionLifetime) -> frame::SessionLifetime {
    match l {
        phux_core::SessionLifetime::Persistent => frame::SessionLifetime::Persistent,
        phux_core::SessionLifetime::Ephemeral => frame::SessionLifetime::Ephemeral,
    }
}

/// A wire lifetime in core.
#[must_use]
pub const fn core_lifetime(l: frame::SessionLifetime) -> phux_core::SessionLifetime {
    match l {
        frame::SessionLifetime::Persistent => phux_core::SessionLifetime::Persistent,
        frame::SessionLifetime::Ephemeral => phux_core::SessionLifetime::Ephemeral,
    }
}

/// An exit policy on the wire.
#[must_use]
pub const fn exit_policy(p: phux_core::ExitPolicy) -> frame::ExitPolicy {
    match p {
        phux_core::ExitPolicy::Remove => frame::ExitPolicy::Remove,
        phux_core::ExitPolicy::Retain => frame::ExitPolicy::Retain,
    }
}

/// A wire exit policy in core.
#[must_use]
pub const fn core_exit_policy(p: frame::ExitPolicy) -> phux_core::ExitPolicy {
    match p {
        frame::ExitPolicy::Remove => phux_core::ExitPolicy::Remove,
        frame::ExitPolicy::Retain => phux_core::ExitPolicy::Retain,
    }
}

/// An execution state on the wire.
#[must_use]
pub const fn exec_state(s: phux_core::ExecState) -> frame::ExecState {
    match s {
        phux_core::ExecState::Starting => frame::ExecState::Starting,
        phux_core::ExecState::Running => frame::ExecState::Running,
        phux_core::ExecState::Stopping => frame::ExecState::Stopping,
        phux_core::ExecState::Completed => frame::ExecState::Completed,
        phux_core::ExecState::Failed => frame::ExecState::Failed,
        phux_core::ExecState::Lost => frame::ExecState::Lost,
        phux_core::ExecState::Abandoned => frame::ExecState::Abandoned,
    }
}

/// An outcome on the wire. An observed exit with neither a status nor a
/// signal reads as a zero status.
#[must_use]
pub const fn outcome(o: phux_core::Outcome) -> frame::Outcome {
    match o {
        phux_core::Outcome::Unobserved => frame::Outcome::Unobserved,
        phux_core::Outcome::Exit(exit) => match (exit.status, exit.signal) {
            (_, Some(signal)) => frame::Outcome::Signaled(signal),
            (Some(status), None) => frame::Outcome::Exited(status),
            (None, None) => frame::Outcome::Exited(0),
        },
    }
}

/// A terminate mode in core.
#[must_use]
pub const fn core_terminate_mode(m: frame::TerminateMode) -> phux_core::TerminateMode {
    match m {
        frame::TerminateMode::Graceful => phux_core::TerminateMode::Graceful,
        frame::TerminateMode::Force => phux_core::TerminateMode::Force,
    }
}

/// A core verb on the wire.
#[must_use]
pub const fn verb(v: phux_core::Verb) -> phux_protocol::kinds::Verb {
    use phux_protocol::kinds::Verb as W;
    match v {
        phux_core::Verb::Observe => W::Inventory,
        phux_core::Verb::Interact => W::Input,
        phux_core::Verb::Organize => W::Organize,
        phux_core::Verb::Create => W::Create,
        phux_core::Verb::Terminate => W::Terminate,
    }
}

/// A core refusal on the wire.
#[must_use]
pub fn refusal(r: &phux_core::Refusal) -> frame::Refusal {
    use phux_core::Refusal as R;
    match r {
        R::Unauthorized { verb: v } => frame::Refusal::Unauthorized { verb: verb(*v) },
        R::NotFound => frame::Refusal::NotFound,
        R::Conflict { current } => frame::Refusal::Conflict {
            current: revision(*current),
        },
        R::Stale { current } => frame::Refusal::Stale {
            current: current.map(activation_id),
        },
        R::NotEmpty { members } => frame::Refusal::NotEmpty { members: *members },
        R::Unsupported { capability } => frame::Refusal::Unsupported {
            capability: capability.clone(),
        },
        R::InvalidArgument { field, reason } => frame::Refusal::InvalidArgument {
            field: (*field).to_owned(),
            reason: reason.clone(),
        },
        R::NameTaken => frame::Refusal::NameTaken,
        R::ExecutionUncertain => frame::Refusal::ExecutionUncertain,
        R::DuplicateOperation => frame::Refusal::DuplicateOperation,
    }
}

/// A core launch spec on the wire.
#[must_use]
pub fn launch(spec: &phux_core::LaunchSpec) -> frame::LaunchSpec {
    match spec {
        phux_core::LaunchSpec::Terminal(t) => frame::LaunchSpec::new(t.argv.clone())
            .with_cwd(t.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()))
            .with_env(t.env.clone()),
        phux_core::LaunchSpec::Agent(a) => frame::LaunchSpec::new(vec![a.provider.clone()])
            .with_restore(a.native_id.as_ref().map(|id| id.as_bytes().to_vec())),
        _ => frame::LaunchSpec::default(),
    }
}

/// A wire launch spec for `kind` in core; `None` for a kind core has no
/// launch for.
#[must_use]
pub fn core_launch(
    kind: phux_core::ResourceKind,
    spec: &frame::LaunchSpec,
) -> Option<phux_core::LaunchSpec> {
    match kind {
        phux_core::ResourceKind::Terminal => {
            Some(phux_core::LaunchSpec::Terminal(phux_core::TerminalLaunch {
                argv: spec.command.clone(),
                cwd: spec.cwd.as_ref().map(std::path::PathBuf::from),
                env: spec.env.clone(),
            }))
        }
        phux_core::ResourceKind::AgentSession => {
            Some(phux_core::LaunchSpec::Agent(phux_core::AgentLaunch {
                provider: spec.command.first().cloned().unwrap_or_default(),
                native_id: spec
                    .restore
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
            }))
        }
        _ => None,
    }
}

/// Attributes on the wire, as `(name, value)` pairs.
#[must_use]
pub fn attrs(a: &phux_core::Attrs) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(project) = &a.project {
        out.push(("project".to_owned(), project.clone()));
    }
    if let Some(dir) = &a.directory {
        out.push(("directory".to_owned(), dir.to_string_lossy().into_owned()));
    }
    if let Some(host) = &a.host_label {
        out.push(("host".to_owned(), host.clone()));
    }
    out
}

/// Wire attribute pairs in core; unknown names are ignored.
#[must_use]
pub fn core_attrs(pairs: &[(String, String)]) -> phux_core::Attrs {
    let mut a = phux_core::Attrs::default();
    for (name, value) in pairs {
        match name.as_str() {
            "project" => a.project = Some(value.clone()),
            "directory" => a.directory = Some(std::path::PathBuf::from(value)),
            "host" => a.host_label = Some(value.clone()),
            _ => {}
        }
    }
    a
}

/// An arrangement owner on the wire.
#[must_use]
pub fn owner(o: phux_core::ArrangementOwner) -> Option<frame::ArrangementOwner> {
    match o {
        phux_core::ArrangementOwner::Session(k) => {
            session_key(k).map(frame::ArrangementOwner::Session)
        }
        phux_core::ArrangementOwner::Workspace(k) => {
            wire::WorkspaceKey::new(k.to_bytes()).map(frame::ArrangementOwner::Workspace)
        }
    }
}

fn node(n: &phux_core::LayoutNode<phux_core::EntryRef>) -> Option<frame::ArrangementNode> {
    Some(match n {
        phux_core::LayoutNode::Leaf(entry) => frame::ArrangementNode::Leaf(frame::EntryRef {
            entry: entry.entry,
            target: match &entry.target {
                phux_core::EntryTarget::Member(k) => frame::EntryTarget::Member(resource_key(*k)?),
                phux_core::EntryTarget::Satellite { host, id } => frame::EntryTarget::Satellite {
                    host: wire::SatelliteHost::new(host.clone()),
                    id: *id,
                },
            },
        }),
        phux_core::LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => frame::ArrangementNode::Split {
            dir: match dir {
                phux_core::SplitDir::Horizontal => frame::SplitDirection::Horizontal,
                phux_core::SplitDir::Vertical => frame::SplitDirection::Vertical,
            },
            ratio: *ratio,
            first: Box::new(node(left)?),
            second: Box::new(node(right)?),
        },
    })
}

fn core_node(n: &frame::ArrangementNode) -> Option<phux_core::LayoutNode<phux_core::EntryRef>> {
    Some(match n {
        frame::ArrangementNode::Leaf(entry) => phux_core::LayoutNode::Leaf(phux_core::EntryRef {
            entry: entry.entry,
            target: match &entry.target {
                frame::EntryTarget::Member(k) => {
                    phux_core::EntryTarget::Member(core_resource_key(k))
                }
                frame::EntryTarget::Satellite { host, id } => phux_core::EntryTarget::Satellite {
                    host: host.as_str().to_owned(),
                    id: *id,
                },
                _ => return None,
            },
        }),
        frame::ArrangementNode::Split {
            dir,
            ratio,
            first,
            second,
        } => phux_core::LayoutNode::Split {
            dir: match dir {
                frame::SplitDirection::Horizontal => phux_core::SplitDir::Horizontal,
                frame::SplitDirection::Vertical => phux_core::SplitDir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(core_node(first)?),
            right: Box::new(core_node(second)?),
        },
    })
}

/// Core groups as a wire document. Entries the wire cannot carry (a zero
/// key) drop their group.
#[must_use]
pub fn document(groups: &[phux_core::Group]) -> frame::ArrangementDoc {
    frame::ArrangementDoc {
        groups: groups
            .iter()
            .filter_map(|g| {
                Some(frame::ArrangementGroup {
                    id: g.id,
                    name: g.name.clone(),
                    tree: node(&g.tree)?,
                })
            })
            .collect(),
    }
}

/// A wire document as core groups; `None` when a target kind is unknown.
#[must_use]
pub fn core_groups(doc: &frame::ArrangementDoc) -> Option<Vec<phux_core::Group>> {
    doc.groups
        .iter()
        .map(|g| {
            Some(phux_core::Group {
                id: g.id,
                name: g.name.clone(),
                tree: core_node(&g.tree)?,
            })
        })
        .collect()
}

/// A core actor as the event stamp's `ActorRef`, with its principal.
#[must_use]
pub fn actor_ref(actor: &phux_core::Actor, client: Option<wire::ClientId>) -> frame::ActorRef {
    frame::ActorRef::new(client.unwrap_or(wire::ClientId::new(0)))
        .with_client_name(actor.client.clone())
        .with_principal(principal_id(&actor.principal))
        .with_on_behalf_of(actor.on_behalf_of.as_ref().and_then(principal_id))
}

/// A principal's stable wire id: the first 16 bytes of the SHA-256 of its
/// label (`owner`, `system`, or the credential name).
#[must_use]
pub fn principal_id(principal: &phux_core::PrincipalId) -> Option<wire::PrincipalId> {
    match principal {
        phux_core::PrincipalId::Owner => digest_id(b"owner"),
        phux_core::PrincipalId::System => digest_id(b"system"),
        phux_core::PrincipalId::Credential(name) => digest_id(name.as_bytes()),
    }
}

fn digest_id(label: &[u8]) -> Option<wire::PrincipalId> {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(label);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    wire::PrincipalId::new(bytes)
}

/// A core organization event as the journal's `AgentEvent`; `None` when a
/// key cannot be carried.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one arm per event, mirrored field for field"
)]
pub fn event(e: &phux_core::Event, org: &phux_core::Org) -> Option<frame::AgentEvent> {
    use phux_core::Event as E;
    Some(match e {
        E::SessionCreated {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::SessionCreated {
            session: session_key(*key)?,
            name: name.clone(),
            lifetime: org
                .session(key)
                .map_or(frame::SessionLifetime::Persistent, |s| lifetime(s.lifetime)),
            revision: revision(*r),
        },
        E::SessionRenamed {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::SessionRenamed {
            session: session_key(*key)?,
            name: name.clone(),
            revision: revision(*r),
        },
        E::SessionLifetimeChanged {
            key,
            lifetime: l,
            revision: r,
        } => frame::AgentEvent::SessionLifetimeChanged {
            session: session_key(*key)?,
            lifetime: lifetime(*l),
            revision: revision(*r),
        },
        E::SessionDeleted {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::SessionDeleted {
            session: session_key(*key)?,
            name: name.clone(),
            revision: revision(*r),
        },
        E::SessionIntakeChanged {
            session,
            awaiting_organization,
            revision: r,
        } => frame::AgentEvent::IntakeChanged {
            session: session_key(*session)?,
            awaiting_organization: *awaiting_organization,
            revision: revision(*r),
        },
        E::MembershipChanged {
            resource,
            from,
            to,
            from_revision,
            to_revision,
            ..
        } => frame::AgentEvent::MembershipChanged {
            resource: resource_key(*resource)?,
            from: session_key(*from)?,
            to: session_key(*to)?,
            // A source the move deleted reports revision 0.
            from_revision: revision(from_revision.unwrap_or_default()),
            to_revision: revision(*to_revision),
        },
        E::ResourceCreated { key, session, kind } => frame::AgentEvent::ResourceCreated {
            resource: resource_key(*key)?,
            session: session_key(*session)?,
            kind: crate::resource::wire_kind(*kind),
        },
        E::ResourceRenamed {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::ResourceRenamed {
            resource: resource_key(*key)?,
            name: name.clone(),
            revision: revision(*r),
        },
        E::ResourceRemoved { key, session, kind } => frame::AgentEvent::ResourceRemoved {
            resource: resource_key(*key)?,
            session: session_key(*session)?,
            kind: crate::resource::wire_kind(*kind),
        },
        E::ActivationChanged {
            resource,
            activation,
            state,
            outcome: o,
        } => frame::AgentEvent::ActivationChanged {
            resource: resource_key(*resource)?,
            activation: activation_id(*activation),
            state: exec_state(*state),
            outcome: o.map(outcome),
        },
        E::ArrangementChanged {
            key,
            owner: o,
            revision: r,
        } => frame::AgentEvent::ArrangementChanged {
            arrangement: arrangement_key(*key)?,
            owner: owner(*o)?,
            revision: r.map(revision),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_refusals_round_trip() {
        let core = phux_core::SessionKey::from_bytes([3; 16]);
        let wire = session_key(core).expect("non-zero");
        assert_eq!(core_session_key(&wire), core);
        assert_eq!(
            refusal(&phux_core::Refusal::InvalidArgument {
                field: "name",
                reason: "empty".to_owned()
            }),
            frame::Refusal::InvalidArgument {
                field: "name".to_owned(),
                reason: "empty".to_owned()
            }
        );
        assert_eq!(
            outcome(phux_core::Outcome::Exit(phux_core::ExitOutcome::signaled(
                9
            ))),
            frame::Outcome::Signaled(9)
        );
    }

    #[test]
    fn documents_round_trip() {
        let groups = vec![phux_core::Group {
            id: [1; 16],
            name: Some("main".to_owned()),
            tree: phux_core::LayoutNode::Split {
                dir: phux_core::SplitDir::Vertical,
                ratio: 0.5,
                left: Box::new(phux_core::LayoutNode::Leaf(phux_core::EntryRef::member(
                    [2; 16],
                    phux_core::ResourceKey::from_bytes([4; 16]),
                ))),
                right: Box::new(phux_core::LayoutNode::Leaf(phux_core::EntryRef {
                    entry: [3; 16],
                    target: phux_core::EntryTarget::Satellite {
                        host: "far".to_owned(),
                        id: 9,
                    },
                })),
            },
        }];
        assert_eq!(core_groups(&document(&groups)), Some(groups));
    }
}
