//! R17: the TUI layout key `phux.tui.layout/v1/<session-id>` (Group 1,
//! `docs/organization.md#records-handles-and-arrangements`) translated to and from the session's
//! arrangement, for clients that still write and read it.
//!
//! A write commits `ARRANGEMENT_REPLACE` attributed to the writer (last
//! write wins, as the key always did): local leaves become member entries
//! by resource key, satellite leaves stay satellite entries, and a leaf
//! naming no member of the session is dropped. A refused write changes
//! nothing; the writer's bytes are never stored. A `GET` encodes the
//! current arrangement, and every arrangement change (and every handle
//! that brings a member's leaf live) republishes it under the key, so
//! older readers follow. Named projections and third-party layout keys
//! stay opaque metadata. Isolated here so it can be deleted with the
//! translation layer.

use phux_core::ids::SessionId;
use phux_core::{Actor, ArrangementOwner, EntryRef, EntryTarget, Op};
use phux_layout_codec::{Group, LayoutNode};
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
        let doc = phux_layout_codec::decode(value).map_err(|e| e.to_string())?;
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
                let tree = self.core_tree(&group.tree, &record.members, &mut fresh)?;
                Some(phux_core::Group {
                    locked: false,
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
                slot_tabs: false,
            },
            actor,
        )
        .map(|_| ())
        .map_err(|refusal| refusal.to_string())
    }

    /// A client layout tree as entry leaves; leaves naming no member are
    /// dropped (the tree collapses around them) and ratios are moved into
    /// the open interval the arrangement holds.
    fn core_tree(
        &self,
        node: &LayoutNode,
        members: &[phux_core::ResourceKey],
        fresh: &mut super::OsKeys,
    ) -> Option<phux_core::LayoutNode<EntryRef>> {
        use phux_core::FreshKeys as _;
        let entries = node.map(&mut |id| {
            let target = self.entry_target(id, members)?;
            Some(EntryRef::new(fresh.fresh(), target))
        });
        let mut tree = present_leaves(entries)?;
        tree.for_each_ratio_mut(&mut |ratio| *ratio = arrangement_ratio(*ratio));
        Some(tree)
    }

    /// What a client leaf shows in the arrangement: a member by resource
    /// key, or a satellite as itself. `None` for a non-member.
    fn entry_target(
        &self,
        id: &WireResourceId,
        members: &[phux_core::ResourceKey],
    ) -> Option<EntryTarget> {
        match id {
            WireResourceId::Local { .. } => {
                let core = self.idspace.resource_from_wire(id)?;
                let key = self.registry().resource_key(core)?;
                members.contains(&key).then_some(EntryTarget::Member(key))
            }
            WireResourceId::Satellite { host, id } => Some(EntryTarget::Satellite {
                host: host.as_str().to_owned(),
                id: *id,
            }),
        }
    }

    /// `session`'s arrangement as the legacy layout envelope: what a `GET`
    /// of its TUI layout key answers.
    ///
    /// Member entries without a live handle are dropped and a group left
    /// empty is skipped. Focus is no part of an arrangement, so each group
    /// focuses its first leaf; readers keep their own. `None` when nothing
    /// live is arranged, so an older reader bootstraps as it always did.
    #[must_use]
    pub(crate) fn arrangement_as_layout(&self, session: SessionId) -> Option<Vec<u8>> {
        let arrangement = self
            .registry()
            .session(session)
            .and_then(|s| s.arrangement)
            .and_then(|k| self.org().arrangement(&k))?;
        let groups: Vec<Group> = arrangement
            .groups
            .iter()
            .filter_map(|group| {
                let tree = self.client_tree(&group.tree)?;
                let focus = tree.leaves().into_iter().next().cloned()?;
                Some(Group {
                    id: group.id,
                    name: group.name.clone().unwrap_or_default(),
                    tree,
                    focus,
                    locked: false,
                })
            })
            .collect();
        if groups.is_empty() {
            return None;
        }
        phux_layout_codec::encode(groups.iter().map(Group::borrow), 0, &[]).ok()
    }

    /// Store `session`'s arrangement under its TUI layout key and broadcast
    /// it, so legacy subscribers see every arrangement change whatever its
    /// source; with nothing live arranged, tombstone it. Equal bytes, or an
    /// absent key, broadcast nothing. A session no wire id names yet has a
    /// key nobody can read, and gets none.
    pub(crate) fn publish_arrangement_as_layout(&mut self, session: SessionId) {
        let Some(wire) = self.idspace.session_wire(session) else {
            return;
        };
        let key = phux_protocol::wire::info::tui_layout_key(wire);
        let scope = Scope::Group(super::DEFAULT_GROUP_ID);
        let _ = match self.arrangement_as_layout(session) {
            Some(bytes) => self.metadata_set_by(&scope, &key, bytes, None),
            None => self.metadata_delete_by(&scope, &key, None),
        };
    }

    /// An entry tree as a client layout tree over live wire ids; member
    /// entries without a live handle are dropped.
    fn client_tree(&self, node: &phux_core::LayoutNode<EntryRef>) -> Option<LayoutNode> {
        present_leaves(node.map(&mut |entry| self.client_leaf(entry)))
    }

    /// The live wire id an entry shows, if its resource has a handle (a
    /// registered handle always has one).
    fn client_leaf(&self, entry: &EntryRef) -> Option<WireResourceId> {
        match &entry.target {
            EntryTarget::Member(key) => {
                let id = self.registry().resource_id(key)?;
                self.resources().handle(id)?;
                self.idspace.resource_wire(id).cloned()
            }
            EntryTarget::Satellite { host, id } => {
                Some(WireResourceId::satellite(host.as_str(), *id))
            }
        }
    }
}

/// `tree` without its `None` leaves, splits collapsing around them; `None`
/// when no leaf is left.
fn present_leaves<L: Clone>(
    tree: phux_core::LayoutNode<Option<L>>,
) -> Option<phux_core::LayoutNode<L>> {
    let kept = phux_core::arrangement::collapse(tree, &Option::is_none)?;
    kept.try_map(&mut |leaf| leaf.clone().ok_or(())).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_client_core::layout::{ArrangementDoc, Group, LayoutState, SplitDir};

    fn handle_for(state: &mut ServerState, pane: phux_core::ids::ResourceId) {
        state
            .register_resource_handle(
                pane,
                super::super::tests::mk_handle(),
                tokio_util::sync::CancellationToken::new(),
            )
            .unwrap();
    }

    #[test]
    fn a_layout_write_becomes_the_arrangement_and_publishes_back() {
        let mut state = ServerState::new();
        let (session, a) = state.seed_session("work");
        let b = state.add_pane_to_session(session).unwrap();
        handle_for(&mut state, a);
        handle_for(&mut state, b);
        let (wa, wb) = (
            state.idspace.intern_resource(a),
            state.idspace.intern_resource(b),
        );
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
            slots: Vec::new(),
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

        let wire = state.idspace.intern_session(session);
        state.publish_arrangement_as_layout(session);
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
    /// L3 §3.2: the key follows every arrangement change, not only a
    /// replace: a leaf goes when its member is reaped, and a refused write
    /// leaves the key as it was.
    #[test]
    fn the_published_layout_follows_every_arrangement_change() {
        let mut state = ServerState::new();
        let (session, a) = state.seed_session("work");
        let b = state.add_pane_to_session(session).unwrap();
        let key = phux_protocol::wire::info::tui_layout_key(state.idspace.intern_session(session));
        handle_for(&mut state, a);
        handle_for(&mut state, b);
        let (wa, wb) = (
            state.idspace.intern_resource(a),
            state.idspace.intern_resource(b),
        );
        let scope = Scope::Group(crate::state::DEFAULT_GROUP_ID);
        let leaves = |state: &ServerState| {
            let bytes = state.metadata().get(&scope, &key).expect("published");
            assert_eq!(Some(&bytes), state.arrangement_as_layout(session).as_ref());
            let doc = ArrangementDoc::decode_cbor(&bytes).unwrap();
            doc.groups
                .iter()
                .filter_map(|g| g.state.tree.as_ref())
                .flat_map(phux_client_core::layout::leaves)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            leaves(&state),
            vec![wa.clone()],
            "published at registration"
        );

        let tree = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(wa.clone())),
            right: Box::new(LayoutNode::Leaf(wb.clone())),
        };
        let doc = ArrangementDoc {
            groups: vec![Group::with_id(
                [7; 16],
                "main".to_owned(),
                LayoutState {
                    tree: Some(tree),
                    focus: Some(wb.clone()),
                },
            )],
            active: 0,
            slots: Vec::new(),
        };
        let write = doc.encode_cbor().unwrap();
        state
            .translate_layout_write(session, &write, &Actor::owner())
            .unwrap();
        assert_eq!(leaves(&state), vec![wa.clone(), wb]);
        assert_ne!(
            state.metadata().get(&scope, &key),
            Some(write),
            "encoded, not stored"
        );

        let _ = state.reap_resource(b);
        assert_eq!(leaves(&state), vec![wa.clone()], "a prune republishes");

        assert!(
            state
                .translate_layout_write(session, b"not-cbor", &Actor::owner())
                .is_err()
        );
        assert_eq!(leaves(&state), vec![wa]);
    }
}
