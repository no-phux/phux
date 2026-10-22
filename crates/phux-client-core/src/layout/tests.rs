//! Layout tests: shared tree and arrangement fixtures.

mod codec_and_tree;
mod groups;
mod properties;

use std::collections::HashSet;

use proptest::prelude::*;

use super::*;
use crate::layout::legacy::LAYOUT_ENVELOPE_VERSION;
use crate::layout::legacy::serialize::{CborLayoutNode, CborResourceId, CborWorkspaceEnvelope};
use serde::Serialize;

fn t(id: u32) -> ResourceId {
    ResourceId::local(id)
}

fn leaf(id: u32) -> LayoutNode {
    LayoutNode::Leaf(t(id))
}

fn encode_state(state: &LayoutState) -> Vec<u8> {
    ArrangementDoc {
        groups: vec![Group::new("1".into(), state.clone())],
        active: 0,
    }
    .encode_cbor()
    .unwrap()
}

fn ws_split(a: u32, b: u32, focus: u32) -> ArrangementDoc {
    let tree = split_at(&leaf(a), &t(a), &t(b), SplitDir::Horizontal, 0.5).unwrap();
    ArrangementDoc {
        groups: vec![Group::new(
            "1".to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(t(focus)),
            },
        )],
        active: 0,
    }
}
