//! Random command sequences: every organization invariant holds after every
//! step, every refusal leaves state unchanged (both checked by the
//! harness), and registry handles track the records exactly.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use phux_core::{
    ActivationId, Actor, ArrangementOwner, Attrs, Command, EntryRef, ExecState, ExitOutcome, Group,
    LayoutNode, Op, Outcome, ResourceKey, Revision, SessionKey, SessionLifetime, SourcePin,
    SplitDir, TerminateMode,
};
use proptest::prelude::*;

use crate::support::{Harness, job, shell};

#[derive(Debug, Clone)]
enum Step {
    CreateSession {
        ephemeral: bool,
        name: u8,
    },
    Rename {
        s: usize,
        name: u8,
    },
    SetProject {
        s: usize,
        project: Option<u8>,
    },
    SetLifetime {
        s: usize,
        ephemeral: bool,
    },
    Delete {
        s: usize,
    },
    CreateResource {
        s: usize,
        retain: bool,
        inbox: bool,
    },
    Move {
        r: usize,
        s: usize,
        stale: bool,
    },
    MoveMany {
        rs: Vec<usize>,
        s: usize,
    },
    Resolve {
        s: usize,
    },
    Undo {
        op: usize,
    },
    Retry {
        op: usize,
    },
    Terminate {
        r: usize,
    },
    Remove {
        r: usize,
    },
    Relaunch {
        r: usize,
    },
    Arrange {
        s: usize,
        take: usize,
    },
    Started {
        r: usize,
    },
    Ended {
        r: usize,
        observed: bool,
        failed: bool,
    },
    Lost {
        r: usize,
    },
}

fn step() -> impl Strategy<Value = Step> {
    let i = any::<usize>;
    prop_oneof![
        (any::<bool>(), 0..6_u8)
            .prop_map(|(ephemeral, name)| Step::CreateSession { ephemeral, name }),
        (i(), 0..6_u8).prop_map(|(s, name)| Step::Rename { s, name }),
        (i(), proptest::option::of(0..3_u8))
            .prop_map(|(s, project)| Step::SetProject { s, project }),
        (i(), any::<bool>()).prop_map(|(s, ephemeral)| Step::SetLifetime { s, ephemeral }),
        i().prop_map(|s| Step::Delete { s }),
        (i(), any::<bool>(), any::<bool>()).prop_map(|(s, retain, inbox)| Step::CreateResource {
            s,
            retain,
            inbox
        }),
        (i(), i(), any::<bool>()).prop_map(|(r, s, stale)| Step::Move { r, s, stale }),
        (proptest::collection::vec(i(), 1..4), i()).prop_map(|(rs, s)| Step::MoveMany { rs, s }),
        i().prop_map(|s| Step::Resolve { s }),
        i().prop_map(|op| Step::Undo { op }),
        i().prop_map(|op| Step::Retry { op }),
        i().prop_map(|r| Step::Terminate { r }),
        i().prop_map(|r| Step::Remove { r }),
        i().prop_map(|r| Step::Relaunch { r }),
        (i(), 0..4_usize).prop_map(|(s, take)| Step::Arrange { s, take }),
        i().prop_map(|r| Step::Started { r }),
        (i(), any::<bool>(), any::<bool>()).prop_map(|(r, observed, failed)| Step::Ended {
            r,
            observed,
            failed
        }),
        i().prop_map(|r| Step::Lost { r }),
    ]
}

fn pick<T: Copy>(items: &[T], i: usize) -> Option<T> {
    (!items.is_empty()).then(|| items[i % items.len()])
}

struct World {
    h: Harness,
    history: Vec<Command>,
    entries: u8,
}

impl World {
    fn sessions(&self) -> Vec<SessionKey> {
        self.h.reg.org().sessions().map(|s| s.key).collect()
    }

    fn resources(&self) -> Vec<ResourceKey> {
        self.h.reg.org().resources().map(|r| r.key).collect()
    }

    fn srev(&self, s: SessionKey) -> Revision {
        self.h.session(s).revision
    }

    fn rrev(&self, r: ResourceKey) -> Revision {
        self.h.resource(r).revision
    }

    fn activation(&self, r: ResourceKey) -> ActivationId {
        self.h
            .resource(r)
            .activation
            .map_or(ActivationId::FIRST, |a| a.id)
    }

    #[expect(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        reason = "one flat arm per step kind"
    )]
    fn op_for(&mut self, step: &Step) -> Option<(Actor, Op)> {
        let sessions = self.sessions();
        let resources = self.resources();
        let owner = Actor::owner();
        let system = Actor::system();
        let op = match step {
            Step::CreateSession { ephemeral, name } => (
                owner,
                Op::SessionCreate {
                    name: format!("s{name}"),
                    lifetime: if *ephemeral {
                        SessionLifetime::Ephemeral
                    } else {
                        SessionLifetime::Persistent
                    },
                    attrs: Attrs::default(),
                    resource: ephemeral.then(shell),
                },
            ),
            Step::Rename { s, name } => {
                let s = pick(&sessions, *s)?;
                (
                    owner,
                    Op::SessionRename {
                        session: s,
                        name: format!("s{name}"),
                    },
                )
            }
            Step::SetProject { s, project } => {
                let s = pick(&sessions, *s)?;
                (
                    owner,
                    Op::SessionSetProject {
                        session: s,
                        project: project.map(|p| format!("p{p}")),
                    },
                )
            }
            Step::SetLifetime { s, ephemeral } => {
                let s = pick(&sessions, *s)?;
                let lifetime = if *ephemeral {
                    SessionLifetime::Ephemeral
                } else {
                    SessionLifetime::Persistent
                };
                (
                    owner,
                    Op::SessionSetLifetime {
                        session: s,
                        lifetime,
                    },
                )
            }
            Step::Delete { s } => {
                let s = pick(&sessions, *s)?;
                (owner, Op::SessionDelete { session: s })
            }
            Step::CreateResource { s, retain, inbox } => (
                owner,
                Op::ResourceCreate {
                    session: if *inbox {
                        None
                    } else {
                        Some(pick(&sessions, *s)?)
                    },
                    resource: if *retain { job() } else { shell() },
                },
            ),
            Step::Move { r, s, stale } => {
                let r = pick(&resources, *r)?;
                let to = pick(&sessions, *s)?;
                let from = self.h.resource(r).session;
                let to_rev = self.srev(to);
                (
                    owner,
                    Op::ResourceMove {
                        resource: r,
                        from,
                        from_expected: self.srev(from),
                        to,
                        to_expected: if *stale { to_rev.next() } else { to_rev },
                    },
                )
            }
            Step::MoveMany { rs, s } => {
                let picked: Vec<ResourceKey> =
                    rs.iter().filter_map(|i| pick(&resources, *i)).collect();
                let mut unique = Vec::new();
                for r in picked {
                    if !unique.contains(&r) {
                        unique.push(r);
                    }
                }
                if unique.is_empty() {
                    return None;
                }
                let to = pick(&sessions, *s)?;
                let mut sources: Vec<SourcePin> = Vec::new();
                for r in &unique {
                    let session = self.h.resource(*r).session;
                    if !sources.iter().any(|p| p.session == session) {
                        sources.push(SourcePin {
                            session,
                            expected: self.srev(session),
                        });
                    }
                }
                (
                    owner,
                    Op::ResourceMoveMany {
                        resources: unique,
                        sources,
                        to,
                        to_expected: self.srev(to),
                    },
                )
            }
            Step::Resolve { s } => {
                let s = pick(&sessions, *s)?;
                (
                    owner,
                    Op::InboxResolve {
                        session: s,
                        expected: self.srev(s),
                    },
                )
            }
            Step::Undo { op } => {
                let original = pick_cmd(&self.history, *op)?.operation;
                (
                    owner,
                    Op::OrganizationUndo {
                        original,
                        expected: vec![],
                    },
                )
            }
            Step::Retry { .. } => return None,
            Step::Terminate { r } => {
                let r = pick(&resources, *r)?;
                (
                    owner,
                    Op::ResourceTerminate {
                        resource: r,
                        activation: self.activation(r),
                        mode: TerminateMode::Graceful,
                    },
                )
            }
            Step::Remove { r } => {
                let r = pick(&resources, *r)?;
                (owner, Op::ResourceRemove { resource: r })
            }
            Step::Relaunch { r } => {
                let r = pick(&resources, *r)?;
                (
                    owner,
                    Op::ResourceRelaunch {
                        resource: r,
                        expected: self.rrev(r),
                        launch: None,
                    },
                )
            }
            Step::Arrange { s, take } => {
                let s = pick(&sessions, *s)?;
                let session = self.h.session(s);
                let members: Vec<ResourceKey> =
                    session.members.iter().take(*take).copied().collect();
                let expected = session
                    .arrangement
                    .and_then(|k| self.h.reg.org().arrangement(&k))
                    .map_or(Revision::ZERO, |a| a.revision);
                let mut tree: Option<LayoutNode<EntryRef>> = None;
                for resource in members {
                    self.entries = self.entries.wrapping_add(1);
                    let leaf = LayoutNode::Leaf(EntryRef::member([self.entries; 16], resource));
                    tree = Some(match tree {
                        None => leaf,
                        Some(t) => LayoutNode::Split {
                            dir: SplitDir::Vertical,
                            ratio: 0.5,
                            left: Box::new(t),
                            right: Box::new(leaf),
                        },
                    });
                }
                let groups = tree
                    .map(|tree| {
                        vec![Group {
                            id: [self.entries; 16],
                            name: None,
                            tree,
                        }]
                    })
                    .unwrap_or_default();
                (
                    owner,
                    Op::ArrangementReplace {
                        owner: ArrangementOwner::Session(s),
                        expected,
                        groups,
                    },
                )
            }
            Step::Started { r } => {
                let r = pick(&resources, *r)?;
                (
                    system,
                    Op::ActivationStarted {
                        resource: r,
                        activation: self.activation(r),
                        proc: None,
                    },
                )
            }
            Step::Ended {
                r,
                observed,
                failed,
            } => {
                let r = pick(&resources, *r)?;
                (
                    system,
                    Op::ActivationEnded {
                        resource: r,
                        activation: self.activation(r),
                        state: if *failed {
                            ExecState::Failed
                        } else {
                            ExecState::Completed
                        },
                        outcome: if *observed {
                            Outcome::Exit(ExitOutcome::exited(0))
                        } else {
                            Outcome::Unobserved
                        },
                    },
                )
            }
            Step::Lost { r } => {
                let r = pick(&resources, *r)?;
                (
                    system,
                    Op::ActivationLost {
                        resource: r,
                        activation: self.activation(r),
                    },
                )
            }
        };
        Some(op)
    }

    fn run(&mut self, step: &Step) {
        if let Step::Retry { op } = step {
            if let Some(cmd) = pick_cmd(&self.history, *op) {
                let actor = if cmd.op.is_report() {
                    Actor::system()
                } else {
                    Actor::owner()
                };
                let replay = self.h.run_cmd(&actor, &cmd).expect("a retry replays");
                assert!(replay.replayed && replay.transition.is_empty());
            }
            return;
        }
        let Some((actor, op)) = self.op_for(step) else {
            return;
        };
        let operation = self.h.next_op();
        let cmd = Command { operation, op };
        if self.h.run_cmd(&actor, &cmd).is_ok() && !cmd.op.is_report() {
            self.history.push(cmd);
        }
    }
}

fn pick_cmd(history: &[Command], i: usize) -> Option<Command> {
    (!history.is_empty()).then(|| history[i % history.len()].clone())
}

/// Every record has exactly one live handle and every handle resolves.
fn check_handles(h: &Harness) -> Result<(), TestCaseError> {
    let org = h.reg.org();
    prop_assert_eq!(h.reg.session_count(), org.sessions().count());
    prop_assert_eq!(h.reg.resources().count(), org.resources().count());
    for s in org.sessions() {
        let id = h.reg.session_id(&s.key);
        prop_assert!(id.is_some());
        let id = id.unwrap();
        prop_assert_eq!(h.reg.session_key(id), Some(s.key));
        prop_assert_eq!(h.reg.members(id).len(), s.members.len());
    }
    for r in org.resources() {
        let id = h.reg.resource_id(&r.key);
        prop_assert!(id.is_some());
        let id = id.unwrap();
        prop_assert_eq!(h.reg.session_of(id), h.reg.session_id(&r.session));
        let desc = h.reg.resource(id).unwrap();
        prop_assert_eq!(desc.kind, r.kind);
        let parent = r.provenance.parent.and_then(|p| h.reg.resource_id(&p));
        prop_assert_eq!(desc.parent, parent);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn invariants_hold_after_every_step(steps in proptest::collection::vec(step(), 0..80)) {
        let mut world = World { h: Harness::new(), history: Vec::new(), entries: 0 };
        for step in &steps {
            world.run(step);
            let org = world.h.reg.org();
            prop_assert!(org.check_invariants().is_ok(), "{:?}", org.check_invariants());
            for r in org.resources() {
                if let Some(a) = &r.activation {
                    prop_assert!(a.id >= ActivationId::FIRST);
                    prop_assert_eq!(a.ended.is_some(), !(a.state.is_current() || a.state == ExecState::Lost));
                }
            }
            check_handles(&world.h)?;
        }
    }
}
