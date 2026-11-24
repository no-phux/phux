//! What a session's rows are: its tabs, then the resources no tab holds.

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;

use crate::addressable::Addressable;
use crate::desk::Tab;

/// One sidebar row's worth of work: a tab and the panes it holds, or a
/// resource no tab holds, alone.
pub(super) struct Item<'a> {
    pub(super) tab: Option<&'a Tab>,
    pub(super) members: Vec<&'a Addressable>,
}

impl<'a> Item<'a> {
    /// The member the row is named after: the first an agent runs in, else
    /// the first. Never the loudest, so a row's name holds still.
    pub(super) fn lead(&self, agent: impl Fn(&ResourceId) -> bool) -> &'a Addressable {
        (self.members.iter().find(|m| agent(&m.id)))
            .or(self.members.first())
            .copied()
            .expect("an item holds a member")
    }
}

/// A session's rows: its tabs, in their order, each holding the listed
/// resources it places; then each listed resource no tab holds, as the
/// server lists them. Without an arrangement, every resource is a row.
pub(super) fn items<'a>(tabs: &'a [Tab], rows: &[&'a Addressable]) -> Vec<Item<'a>> {
    let mut placed = std::collections::HashSet::new();
    let mut out: Vec<Item> = tabs
        .iter()
        .filter_map(|tab| {
            let members: Vec<&Addressable> = (tab.resources.iter())
                .filter_map(|id| rows.iter().find(|r| r.id == *id).copied())
                .filter(|r| placed.insert(r.id.clone()))
                .collect();
            (!members.is_empty()).then_some(Item {
                tab: Some(tab),
                members,
            })
        })
        .collect();
    out.extend(
        (rows.iter())
            .filter(|r| !placed.contains(&r.id))
            .map(|r| Item {
                tab: None,
                members: vec![*r],
            }),
    );
    out
}

/// The fold: each root item, in list order, with the items started from it.
/// An item folds under the item holding a member's parent (`phux.parent/v1`),
/// and that one's, up to a root, so a grandchild sits with the children. A
/// parent that closed, sits in another session, or closes a cycle leaves the
/// item a root.
pub(super) fn fold(items: &[Item]) -> Vec<(usize, Vec<usize>)> {
    let owner: HashMap<&ResourceId, usize> = (items.iter().enumerate())
        .flat_map(|(i, item)| item.members.iter().map(move |m| (&m.id, i)))
        .collect();
    let up = |i: usize| {
        (items[i].members.iter())
            .find_map(|m| m.parent.as_ref().and_then(|p| owner.get(p)).copied())
            .filter(|&j| j != i)
    };
    let root = |i: usize| {
        let mut at = i;
        for _ in 0..items.len() {
            match up(at) {
                None => return at,
                Some(j) if j == i => return i,
                Some(j) => at = j,
            }
        }
        i
    };
    let roots: Vec<usize> = (0..items.len()).map(root).collect();
    let mut children = vec![Vec::new(); items.len()];
    for (i, &r) in roots.iter().enumerate() {
        if r != i {
            children[r].push(i);
        }
    }
    (0..items.len())
        .filter(|&i| roots[i] == i)
        .map(|i| (i, std::mem::take(&mut children[i])))
        .collect()
}

/// A tab's name when a person gave it one: the server's defaults are
/// just numbers.
pub(super) fn given_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.parse::<u32>().is_err()).then(|| name.to_owned())
}
