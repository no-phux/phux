//! Conformance: the three split-tree encodings must describe the same tree.
//!
//! The ADR-0012 split tree exists as `phux_core::arrangement::LayoutNode`
//! (the generic tree a session arrangement's groups are made of,
//! ADR-0160), `phux_client::layout::LayoutNode` (the client tree every
//! consumer persists as the L3 `phux.tui.layout/v1` CBOR envelope,
//! `docs/organization.md`), and `phux_server::upgrade::blob::LayoutBlob` (the
//! window layout an upgrade blob from an older image carries, keyed by wire
//! id, which the resuming server reads into an arrangement). Each has its own
//! round-trip test; this file builds one corpus into all three and projects
//! them back to a shared normal form so a drift in axis convention, child
//! order, or ratio domain fails on the commit that caused it.
//!
//! The encodings are compared as independently built shapes: core leaves are
//! the corpus indices themselves, L3 and blob leaves are wire ids.
//!
//! None of the encodings imposes a depth bound of its own; the depth map
//! found a real bug: `serde_json`'s recursion limit made a 63-pane window's
//! upgrade blob writable but unreadable.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_client::layout::{ArrangementDoc, LayoutDecodeError, LayoutNode as L3Node, leaves};
use phux_core::arrangement::SplitDir as CoreDir;
use phux_server::upgrade::blob::{LayoutBlob, StateBlob};

mod corpus;
mod encodings;

use corpus::{DEEP_SPINE_PANES, corpus, registry_spine};
use encodings::{
    CoreNode, blob_index_map, build_blob, build_core, build_l3, project_blob, project_core,
    project_l3,
};

/// One abstract corpus, built independently into all three encodings and
/// projected back. Any drift in axis vocabulary, child order, ratio placement,
/// or leaf ordering shows up as an inequality naming the corpus entry.
#[test]
fn all_three_encodings_describe_the_same_tree() {
    for (name, shape) in corpus() {
        let indices = shape.panes();
        let index_of_blob = blob_index_map(&shape);

        let core = build_core(&shape);
        let l3 = build_l3(&shape);
        let blob = build_blob(&shape);

        assert_eq!(
            project_core(&core),
            shape,
            "{name}: phux_core encoding does not project back to the corpus shape"
        );
        assert_eq!(
            project_l3(&l3),
            shape,
            "{name}: L3 client encoding does not project back to the corpus shape"
        );
        assert_eq!(
            project_blob(&blob, &index_of_blob),
            shape,
            "{name}: upgrade-blob encoding does not project back to the corpus shape"
        );

        // Leaf order is load-bearing on its own: `LayoutNode::leaves()` is what
        // `name:N.M` selectors and clients walk to tile panes, so a builder that
        // swapped `left` and `right` while keeping the tree isomorphic would
        // still be a bug.
        let core_leaf_order: Vec<u32> = core.leaves().into_iter().copied().collect();
        assert_eq!(
            core_leaf_order, indices,
            "{name}: phux_core leaf traversal order diverged from the corpus"
        );
    }
}

/// L3 and blob each survive their own round trip and still equal the corpus
/// shape, so a self-consistent codec bug cannot hide (core never serializes).
#[test]
fn l3_and_blob_round_trips_land_back_on_the_corpus_shape() {
    for (name, shape) in corpus() {
        // L3: through the CBOR envelope every consumer persists.
        let decoded = round_trip_layout_through_the_envelope(&build_l3(&shape))
            .unwrap_or_else(|e| panic!("{name}: layout envelope failed to round-trip: {e:?}"));
        assert_eq!(
            project_l3(&decoded),
            shape,
            "{name}: layout changed crossing the L3 envelope"
        );

        // Blob: through the JSON carrier the upgrade actually writes.
        let blob = build_blob(&shape);
        let json = serde_json::to_vec(&blob).expect("blob serializes");
        let back: LayoutBlob = serde_json::from_slice(&json)
            .unwrap_or_else(|e| panic!("{name}: blob JSON did not round-trip: {e}"));
        assert_eq!(
            project_blob(&back, &blob_index_map(&shape)),
            shape,
            "{name}: layout changed crossing the upgrade blob"
        );
    }
}

/// Encode `node` as the one window of a v3 layout envelope (docs/organization.md) and
/// decode it back, returning the tree the decoder produced.
fn round_trip_layout_through_the_envelope(node: &L3Node) -> Result<L3Node, LayoutDecodeError> {
    let first = leaves(node).into_iter().next().expect("a tree has a leaf");
    let mut workspace = ArrangementDoc::single(first);
    workspace.groups[0].state.tree = Some(node.clone());
    let bytes = workspace.encode_cbor().expect("envelope encodes");
    let decoded = ArrangementDoc::decode_cbor(&bytes)?;
    Ok(decoded.groups[0]
        .state
        .tree
        .clone()
        .expect("layout survived the round trip"))
}

/// A v1 upgrade blob holding one session and one window whose layout is
/// `layout`, as an older image writes it, in bytes [`StateBlob::from_bytes`]
/// reads. Only the fields every image writes are present, so an additive
/// field never breaks this fixture.
fn one_window_blob_bytes(layout: &LayoutBlob, panes: u32) -> Vec<u8> {
    let pane_ids: Vec<u32> = (1..=panes).collect();
    let blob = serde_json::json!({
        "version": phux_server::upgrade::blob::BLOB_VERSION,
        "listener_fd": -1,
        "counters": {
            "next_session_wire_id": 2,
            "next_terminal_wire_id": panes + 1,
            "next_window_wire_id": 2,
            "next_touch_timestamp": 1,
        },
        "sessions": [{
            "wire_id": 1,
            "name": "conformance",
            "window_wire_ids": [1],
            "active_window": 1,
        }],
        "windows": [{
            "wire_id": 1,
            "session_wire_id": 1,
            "pane_wire_ids": pane_ids,
            "active_resource": 1,
            "layout": layout,
        }],
        "panes": [],
    });
    serde_json::to_vec(&blob).expect("a deep blob still serializes")
}

/// None of the three encodings imposes a depth bound of its own, so a
/// spine past `serde_json`'s 62-pane recursion ceiling survives each.
#[test]
fn depth_bounds_diverge_by_design_and_here_is_the_map() {
    // One pane past the deep-spine corpus entry.
    let panes = DEEP_SPINE_PANES + 1;
    let too_deep = registry_spine(panes);
    let pane_count = u32::try_from(panes).expect("small");

    // --- phux_core: no cap at all. Splitting builds this happily.
    let mut core = CoreNode::Leaf(0);
    for pane in 1..pane_count {
        core.split(&0, pane, CoreDir::Horizontal, 0.5)
            .expect("pane 0 is in the tree");
    }
    assert_eq!(
        project_core(&core),
        too_deep,
        "phux_core imposes no depth bound; if it grows one, this is \
         where to record it"
    );

    // --- L3 envelope: no cap of its own; the CBOR reader's recursion
    // limit is what keeps a hostile peer's value off the stack.
    assert_eq!(
        project_l3(
            &round_trip_layout_through_the_envelope(&build_l3(&too_deep))
                .expect("the envelope carries a spine this deep")
        ),
        too_deep,
        "the envelope must carry the deep spine unchanged"
    );

    // --- upgrade blob: no cap of its own. It never crosses a trust
    // boundary — the bytes come from this binary's own predecessor image.
    let bytes = one_window_blob_bytes(&build_blob(&too_deep), pane_count);
    let restored = StateBlob::from_bytes(&bytes).expect("deserializes");
    assert_eq!(
        project_blob(
            restored.windows[0]
                .layout
                .as_ref()
                .expect("the restored window has a layout"),
            &blob_index_map(&too_deep),
        ),
        too_deep,
        "the blob must carry the deep spine unchanged"
    );
}
