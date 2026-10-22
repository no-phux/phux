use phux_protocol::ResourceId;
use phux_protocol::ids::{ActivationId, ResourceKey, Revision};
use phux_protocol::wire::frame::{
    ActivationInfo, ArrangementDoc, ArrangementOwner, Command, CommandResult, CommandValue,
    Committed, EntryTarget, ExecState, LaunchMode, LiveTarget, Outcome, Refusal, SessionLifetime,
};

use super::answer::RecordKey;
use super::{Inverse, OrgFixture};

impl OrgFixture {
    #[allow(clippy::too_many_lines, reason = "one arm per organization command")]
    pub(super) fn mutate(
        &mut self,
        command: &Command,
    ) -> Result<(CommandResult, Option<Inverse>), Refusal> {
        let ok = |committed: Committed| {
            CommandResult::OkWith(CommandValue::Committed(Box::new(committed)))
        };
        match command {
            Command::SessionCreate {
                operation,
                name,
                lifetime,
                launch,
                ..
            } => {
                if self.sessions.iter().any(|s| s.name == *name) {
                    return Err(Refusal::NameTaken);
                }
                if *lifetime == SessionLifetime::Ephemeral && launch.is_none() {
                    return Err(Refusal::InvalidArgument {
                        field: "launch".to_owned(),
                        reason: "an ephemeral session needs a launch".to_owned(),
                    });
                }
                let session = self.session(name, *lifetime);
                let revision = self
                    .session_ref(&session)
                    .map(|record| record.revision)
                    .unwrap_or_default();
                let mut committed =
                    Committed::new(*operation, revision).with_session(Some(session));
                let mut groups = Vec::new();
                if launch.is_some() {
                    let resource = self.shell(session);
                    let record = self.resource_ref(&resource).expect("made");
                    committed = committed
                        .with_resource(Some(resource))
                        .with_target(phux_client_core::org_ops::live_target(record));
                    groups.push(phux_protocol::wire::frame::ArrangementGroup {
                        id: self.fresh(),
                        name: None,
                        tree: phux_protocol::wire::frame::ArrangementNode::Leaf(
                            phux_protocol::wire::frame::EntryRef {
                                entry: self.fresh(),
                                target: EntryTarget::Member(resource),
                            },
                        ),
                    });
                }
                // Every session has an arrangement (SPEC L2 §2.2): the seed
                // member's one group, or none.
                self.arrangement(session, ArrangementDoc { groups });
                Ok((ok(committed), None))
            }
            Command::SessionRename {
                operation,
                session,
                name,
            } => {
                self.session_ref(session).ok_or(Refusal::NotFound)?;
                if self
                    .sessions
                    .iter()
                    .any(|s| s.name == *name && s.key != *session)
                {
                    return Err(Refusal::NameTaken);
                }
                let revision = self.bump();
                let record = self.session_mut(session).ok_or(Refusal::NotFound)?;
                record.name.clone_from(name);
                record.revision = revision;
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::SessionSetProject {
                operation,
                session,
                project,
            } => {
                self.session_ref(session).ok_or(Refusal::NotFound)?;
                let revision = self.bump();
                let record = self.session_mut(session).ok_or(Refusal::NotFound)?;
                crate::project_identity::set_link_attr(&mut record.attrs, project.as_deref());
                record.revision = revision;
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::SessionSetLifetime {
                operation,
                session,
                lifetime,
            } => {
                let revision = self.bump();
                let record = self.session_mut(session).ok_or(Refusal::NotFound)?;
                record.lifetime = *lifetime;
                record.revision = revision;
                if *lifetime == SessionLifetime::Ephemeral && record.members == 0 {
                    self.delete_session(session);
                }
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::SessionDelete { operation, session } => {
                let record = self.session_ref(session).ok_or(Refusal::NotFound)?;
                if Some(*session) == self.inbox {
                    return Err(Refusal::InvalidArgument {
                        field: "session".to_owned(),
                        reason: "the designated intake session cannot be deleted".to_owned(),
                    });
                }
                if record.members > 0 {
                    return Err(Refusal::NotEmpty {
                        members: record.members,
                    });
                }
                self.delete_session(session);
                let revision = self.bump();
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::SessionKill { operation, session } => {
                if Some(*session) == self.inbox {
                    return Err(Refusal::InvalidArgument {
                        field: "session".to_owned(),
                        reason: "the designated intake session cannot be deleted".to_owned(),
                    });
                }
                let members: Vec<ResourceKey> = self
                    .resources
                    .iter()
                    .filter(|r| r.session == *session)
                    .map(|r| r.key)
                    .collect();
                if self.session_ref(session).is_none() {
                    return Err(Refusal::NotFound);
                }
                for member in &members {
                    self.drop_resource(member);
                }
                self.delete_session(session);
                let committed = Committed::new(*operation, Revision::default())
                    .with_members(Some(u32::try_from(members.len()).unwrap_or(u32::MAX)));
                Ok((ok(committed), None))
            }
            Command::ResourceCreate {
                operation,
                session,
                on_exit,
                launch_mode,
                ..
            } => {
                if self.session_ref(session).is_none() {
                    return Err(Refusal::NotFound);
                }
                let resource = match launch_mode {
                    LaunchMode::Deferred => self.member(*session, None, *on_exit, None),
                    LaunchMode::Immediate => self.shell(*session),
                };
                let record = self.resource_ref(&resource).expect("made");
                let committed = Committed::new(*operation, record.revision)
                    .with_resource(Some(resource))
                    .with_target(phux_client_core::org_ops::live_target(record))
                    .with_revisions(self.pin_session(session).into_iter().collect());
                Ok((ok(committed), None))
            }
            Command::ResourceMove {
                operation,
                resource,
                from,
                from_expected,
                to,
                to_expected,
            } => {
                let current = self
                    .resource_ref(resource)
                    .ok_or(Refusal::NotFound)?
                    .session;
                if current != *from {
                    return Err(Refusal::InvalidArgument {
                        field: "from".to_owned(),
                        reason: "the resource is not in that session".to_owned(),
                    });
                }
                self.check_session(*from, *from_expected)?;
                self.check_session(*to, *to_expected)?;
                let revision = self.move_one(resource, *to);
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&[*from, *to]));
                Ok((
                    ok(committed),
                    Some(Inverse::Moves(vec![(*resource, *from)])),
                ))
            }
            Command::ResourceMoveMany {
                operation,
                items,
                to,
                to_expected,
            } => {
                for item in items {
                    let current = self.resource_ref(&item.resource).ok_or(Refusal::NotFound)?;
                    if current.session != item.from {
                        return Err(Refusal::InvalidArgument {
                            field: "move_item".to_owned(),
                            reason: "a resource is not in its named session".to_owned(),
                        });
                    }
                    let source = self.session_ref(&item.from).ok_or(Refusal::NotFound)?;
                    if source.revision != item.from_expected {
                        return Err(Refusal::Conflict {
                            current: source.revision,
                        });
                    }
                }
                self.check_session(*to, *to_expected)?;
                let mut inverse = Vec::new();
                let mut touched = vec![*to];
                let mut revision = Revision::default();
                for item in items {
                    revision = self.move_one(&item.resource, *to);
                    inverse.push((item.resource, item.from));
                    touched.push(item.from);
                }
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&touched));
                Ok((ok(committed), Some(Inverse::Moves(inverse))))
            }
            Command::ResourceRename {
                operation,
                resource,
                name,
            } => {
                self.resource_ref(resource).ok_or(Refusal::NotFound)?;
                let revision = self.bump();
                let record = self.resource_mut(resource).ok_or(Refusal::NotFound)?;
                record.name.clone_from(name);
                record.revision = revision;
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::ResourceTerminate {
                operation, target, ..
            } => {
                let key = self.terminate_target(target)?;
                if !self.terminate_holds {
                    self.end_activation(&key, Outcome::Signaled(15));
                }
                Ok((
                    CommandResult::Accepted {
                        operation: *operation,
                    },
                    None,
                ))
            }
            Command::ResourceRemove {
                operation,
                resource,
            } => {
                self.resource_ref(resource).ok_or(Refusal::NotFound)?;
                self.require_inactive(resource)?;
                let session = self.drop_resource(resource).ok_or(Refusal::NotFound)?;
                let revision = self.bump();
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&[session]));
                Ok((ok(committed), None))
            }
            Command::ResourceRelaunch {
                operation,
                resource,
                expected,
            } => {
                self.check_resource(*resource, *expected)?;
                self.require_inactive(resource)?;
                let live = ResourceId::local(self.next_live());
                let revision = self.bump();
                let instance = self.instance;
                let record = self.resource_mut(resource).ok_or(Refusal::NotFound)?;
                let id = record
                    .activation
                    .map_or(ActivationId::new(1), |a| ActivationId::new(a.id.get() + 1));
                record.activation = Some(ActivationInfo::new(id, instance, ExecState::Running));
                record.live = Some(live.clone());
                record.revision = revision;
                let committed =
                    Committed::new(*operation, revision).with_target(Some(LiveTarget {
                        resource: live,
                        instance,
                        activation: id,
                    }));
                Ok((ok(committed), None))
            }
            Command::InboxResolve {
                operation,
                session,
                expected,
            } => {
                self.check_session(*session, *expected)?;
                let revision = self.bump();
                let record = self.session_mut(session).ok_or(Refusal::NotFound)?;
                let prior = record.awaiting_organization;
                record.awaiting_organization = false;
                record.revision = revision;
                let committed = Committed::new(*operation, revision)
                    .with_prior_awaiting(Some(prior))
                    .with_revisions(self.pins(&[*session]));
                Ok((ok(committed), Some(Inverse::Intake(*session))))
            }
            Command::OrganizationUndo {
                operation,
                undo,
                expected,
            } => {
                let receipt = self
                    .receipts
                    .iter()
                    .find(|r| r.operation == *undo)
                    .cloned()
                    .ok_or(Refusal::NotFound)?;
                let inverse = receipt.inverse.ok_or_else(|| Refusal::Unsupported {
                    capability: "undo of this operation".to_owned(),
                })?;
                for pin in expected {
                    let current = self
                        .sessions
                        .iter()
                        .find(|s| s.key.as_bytes() == &pin.key)
                        .map(|s| s.revision);
                    if let Some(current) = current
                        && current != pin.revision
                    {
                        return Err(Refusal::Conflict { current });
                    }
                }
                let (revision, touched, redo) = match inverse {
                    Inverse::Moves(moves) => {
                        let mut revision = Revision::default();
                        let mut touched = Vec::new();
                        let mut redo = Vec::new();
                        for (resource, session) in moves {
                            let from = self.resource_ref(&resource).map(|r| r.session);
                            revision = self.move_one(&resource, session);
                            touched.push(session);
                            if let Some(from) = from {
                                touched.push(from);
                                redo.push((resource, from));
                            }
                        }
                        (revision, touched, Inverse::Moves(redo))
                    }
                    Inverse::Intake(session) => {
                        let revision = self.bump();
                        let record = self.session_mut(&session).ok_or(Refusal::NotFound)?;
                        record.awaiting_organization = true;
                        record.revision = revision;
                        (revision, vec![session], Inverse::Intake(session))
                    }
                };
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&touched));
                Ok((ok(committed), Some(redo)))
            }
            Command::ArrangementReplace {
                operation,
                arrangement,
                expected,
                document,
            } => {
                let value = self.arrangement_ref(arrangement).ok_or(Refusal::NotFound)?;
                let (current, owner) = (value.revision, value.owner);
                self.conflict(RecordKey::Arrangement(*arrangement))?;
                if current != *expected {
                    return Err(Refusal::Conflict { current });
                }
                if let ArrangementOwner::Session(session) = owner {
                    for group in &document.groups {
                        for entry in phux_client_core::layout::wire::wire_leaves(&group.tree) {
                            if let EntryTarget::Member(key) = entry.target
                                && self.resource_ref(&key).is_none_or(|r| r.session != session)
                            {
                                return Err(Refusal::InvalidArgument {
                                    field: "document".to_owned(),
                                    reason: "an entry names a resource that is not a member"
                                        .to_owned(),
                                });
                            }
                        }
                    }
                }
                let revision = self.bump();
                let value = self
                    .arrangements
                    .iter_mut()
                    .find(|a| a.key == *arrangement)
                    .ok_or(Refusal::NotFound)?;
                value.document = document.clone();
                value.revision = revision;
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            _ => Err(Refusal::Unsupported {
                capability: "scripted".to_owned(),
            }),
        }
    }
}
