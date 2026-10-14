//! R17: the TUI layout key `phux.tui.layout/v1/<session-id>` (Group 1,
//! `docs/spec/L3.md` §3.2) translated to and from the session's
//! arrangement, for clients that still write and read it.
//!
//! A write commits `ARRANGEMENT_REPLACE` attributed to the writer (last
//! write wins, as the key always did): local leaves become member entries
//! by resource key, satellite leaves stay satellite entries, and a leaf
//! naming no member of the session is dropped. An arrangement replaced
//! through the organization command is published back under the key, so
//! older readers see it. Named projections and third-party layout keys
//! stay opaque metadata. Isolated here so it can be deleted with the
//! translation layer.

use phux_client_core::layout::{ArrangementDoc, Group, LayoutNode, LayoutState, SplitDir};
use phux_core::ids::SessionId;
use phux_core::{Actor, ArrangementOwner, EntryRef, EntryTarget, Op};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::Scope;

use super::ServerState;

/// Split ratios the arrangement accepts: clients bank `0.0` and `1.0`
/// (ADR-0048), the arrangement holds the open interval.
const RATIO_FLOOR: f32 = 0.01;

/// A foreign split ratio as one the arrangement accepts: clamped into the
/// open interval, and an even split for NaN.
pub(super) fn arrangement_ratio(ratio: f32) -> f32 {
    if ratio.is_nan() {
        0.5
    } else {
        ratio.clamp(RATIO_FLOOR, 1.0 - RATIO_FLOOR)
    }
}

impl ServerState {
    /// The session a Group-1 `phux.tui.layout/v1/<id>` key names, if live.
    #[must_use]
    pub(crate) fn tui_layout_session(&self, scope: &Scope, key: &str) -> Option<SessionId> {
        if !matches!(scope, Scope::Group(gid) if *gid == super::DEFAULT_GROUP_ID) {
            return None;
        }
        let wire = phux_protocol::wire::info::tui_layout_key_session(key)?;
        self.idspace.resolve_session(wire)
    }

    /// Translate a TUI layout write into the session's arrangement,
    /// attributed to `actor`. A value that does not decode, or a refused
    /// replace, changes nothing and is reported.
    ///
    /// # Errors
    ///
    /// Why the write was not translated.
    pub(crate) fn translate_layout_write(
        &mut self,
        session: SessionId,
        value: &[u8],
        actor: &Actor,
    ) -> Result<(), String> {
        let doc = ArrangementDoc::decode_cbor(value).map_err(|e| e.to_string())?;
        let record = self
            .registry()
            .session(session)
            .ok_or_else(|| "session gone".to_owned())?
            .clone();
        let expected = record
            .arrangement
            .and_then(|k| self.org().arrangement(&k))
            .map_or(phux_core::Revision::ZERO, |a| a.revision);
        let mut fresh = super::OsKeys;
        let groups: Vec<phux_core::Group> = doc
            .groups
            .iter()
            .filter_map(|group| {
                let tree = group.state.tree.as_ref()?;
                let tree = self.core_tree(tree, &record.members, &mut fresh)?;
                Some(phux_core::Group {
                    id: group.id,
                    name: (!group.name.is_empty()).then(|| group.name.clone()),
                    tree,
                })
            })
            .collect();
        self.commit(
            Op::ArrangementReplace {
                owner: ArrangementOwner::Session(record.key),
                expected,
                groups,
            },
            actor,
        )
        .map(|_| ())
        .map_err(|refusal| refusal.to_string())
    }

    /// A client layout tree as entry leaves; leaves naming no member are
    /// dropped (the tree collapses around them).
    fn core_tree(
        &self,
        node: &LayoutNode,
        members: &[phux_core::ResourceKey],
        fresh: &mut super::OsKeys,
    ) -> Option<phux_core::LayoutNode<EntryRef>> {
        use phux_core::FreshKeys as _;
        Some(match node {
            LayoutNode::Leaf(id) => {
                let target = match id {
                    WireResourceId::Local { .. } => {
                        let core = self.terminal_from_wire(id)?;
                        let key = self.registry().resource_key(core)?;
                        members.contains(&key).then_some(EntryTarget::Member(key))?
                    }
                    WireResourceId::Satellite { host, id } => EntryTarget::Satellite {
                        host: host.as_str().to_owned(),
                        id: *id,
                    },
                };
                phux_core::LayoutNode::Leaf(EntryRef {
                    entry: fresh.fresh(),
                    target,
                })
            }
            LayoutNode::Split {
                dir,
                ratio,
                left,
                right,
            } => {
                let left = self.core_tree(left, members, fresh);
                let right = self.core_tree(right, members, fresh);
                match (left, right) {
                    (Some(left), Some(right)) => phux_core::LayoutNode::Split {
                        dir: match dir {
                            SplitDir::Horizontal => phux_core::SplitDir::Horizontal,
                            SplitDir::Vertical => phux_core::SplitDir::Vertical,
                        },
                        ratio: arrangement_ratio(*ratio),
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    (Some(only), None) | (None, Some(only)) => only,
                    (None, None) => return None,
                }
            }
        })
    }

    /// Publish `session`'s arrangement under its TUI layout key, so older
    /// readers see an arrangement replaced through the organization
    /// command. Member entries without a live handle are dropped; a group
    /// left empty is skipped.
    pub(crate) fn publish_arrangement_as_layout(&mut self, session: SessionId) {
        let Some(arrangement) = self
            .registry()
            .session(session)
            .and_then(|s| s.arrangement)
            .and_then(|k| self.org().arrangement(&k))
            .cloned()
        else {
            return;
        };
        let groups: Vec<Group> = arrangement
            .groups
            .iter()
            .filter_map(|group| {
                let tree = self.client_tree(&group.tree)?;
                let focus = phux_client_core::layout::leaves(&tree).into_iter().next()?;
                Some(Group::with_id(
                    group.id,
                    group.name.clone().unwrap_or_default(),
                    LayoutState {
                        tree: Some(tree),
                        focus: Some(focus),
                    },
                ))
            })
            .collect();
        let doc = ArrangementDoc { groups, active: 0 };
        let Ok(bytes) = doc.encode_topology_cbor() else {
            return;
        };
        let wire = self.idspace.intern_session(session);
        let key = phux_protocol::wire::info::tui_layout_key(wire);
        let scope = Scope::Group(super::DEFAULT_GROUP_ID);
        let _ = self.metadata_set_by(&scope, &key, bytes, None);
    }

    /// An entry tree as a client layout tree over live wire ids.
    fn client_tree(&mut self, node: &phux_core::LayoutNode<EntryRef>) -> Option<LayoutNode> {
        Some(match node {
            phux_core::LayoutNode::Leaf(entry) => LayoutNode::Leaf(match &entry.target {
                EntryTarget::Member(key) => {
                    let id = self.registry().resource_id(key)?;
                    self.resource_handle(id)?;
                    self.intern_terminal_wire(id)
                }
                EntryTarget::Satellite { host, id } => {
                    WireResourceId::satellite(host.as_str(), *id)
                }
            }),
            phux_core::LayoutNode::Split {
                dir,
                ratio,
                left,
                right,
            } => {
                let left = self.client_tree(left);
                let right = self.client_tree(right);
                match (left, right) {
                    (Some(left), Some(right)) => LayoutNode::Split {
                        dir: match dir {
                            phux_core::SplitDir::Horizontal => SplitDir::Horizontal,
                            phux_core::SplitDir::Vertical => SplitDir::Vertical,
                        },
                        ratio: *ratio,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    (Some(only), None) | (None, Some(only)) => only,
                    (None, None) => return None,
                }
            }
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn handle_for(state: &mut ServerState, pane: phux_core::ids::ResourceId) {
        let _ = state.register_resource_handle(
            pane,
            super::super::tests::mk_handle(),
            tokio_util::sync::CancellationToken::new(),
        );
    }

    #[test]
    fn a_layout_write_becomes_the_arrangement_and_publishes_back() {
        let mut state = ServerState::new();
        let (session, a) = state.seed_session("work");
        let b = state.add_pane_to_session(session).unwrap();
        handle_for(&mut state, a);
        handle_for(&mut state, b);
        let (wa, wb) = (state.intern_terminal_wire(a), state.intern_terminal_wire(b));
        let stranger = WireResourceId::local(999);
        let doc = ArrangementDoc {
            groups: vec![Group::with_id(
                [7; 16],
                "main".to_owned(),
                LayoutState {
                    tree: Some(LayoutNode::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 1.0,
                        left: Box::new(LayoutNode::Leaf(wa.clone())),
                        right: Box::new(LayoutNode::Split {
                            dir: SplitDir::Vertical,
                            ratio: 0.5,
                            left: Box::new(LayoutNode::Leaf(wb.clone())),
                            right: Box::new(LayoutNode::Leaf(stranger)),
                        }),
                    }),
                    focus: Some(wa.clone()),
                },
            )],
            active: 0,
        };
        state
            .translate_layout_write(session, &doc.encode_cbor().unwrap(), &Actor::owner())
            .unwrap();
        let key = state
            .registry()
            .session(session)
            .unwrap()
            .arrangement
            .unwrap();
        let arrangement = state.org().arrangement(&key).unwrap().clone();
        assert_eq!(arrangement.groups.len(), 1);
        assert_eq!(arrangement.groups[0].id, [7; 16]);
        assert_eq!(arrangement.groups[0].name.as_deref(), Some("main"));
        assert_eq!(
            arrangement.resources(),
            vec![
                state.registry().resource_key(a).unwrap(),
                state.registry().resource_key(b).unwrap()
            ],
            "the stranger leaf is dropped"
        );

        state.publish_arrangement_as_layout(session);
        let wire = state.idspace.session_wire(session).unwrap();
        let bytes = state
            .metadata()
            .get(
                &Scope::Group(crate::state::DEFAULT_GROUP_ID),
                &phux_protocol::wire::info::tui_layout_key(wire),
            )
            .expect("published");
        let back = ArrangementDoc::decode_cbor(&bytes).unwrap();
        assert_eq!(
            phux_client_core::layout::leaves(back.groups[0].state.tree.as_ref().unwrap()),
            vec![wa, wb]
        );
    }
}
