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
//! Two asymmetries are structural and asserted, not normalised away: the
//! encodings accept different ratio domains (core open interval, L3 closed,
//! blob unchecked), and none imposes a depth bound of its own. If a later
//! change unifies them, update the map in those tests rather than deleting
//! the assertion.
//! The depth map found a real bug: `serde_json`'s recursion limit made a
//! 63-pane window's upgrade blob writable but unreadable.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_client::layout::{ArrangementDoc, LayoutDecodeError, LayoutNode as L3Node, leaves};
use phux_core::arrangement::{LayoutError, SplitDir as CoreDir};
use phux_server::upgrade::blob::{LayoutBlob, SplitDirBlob, StateBlob};

mod corpus;
mod encodings;

use corpus::{Axis, DEEP_SPINE_PANES, Shape, corpus, registry_spine};
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

/// Splitting pane 0 once per new pane, as an older image's registry did for
/// a window, builds [`registry_spine`]: the shape the deep-spine corpus
/// entry and the blob depth analysis rest on.
#[test]
fn core_split_builds_the_registry_spine() {
    const PANES: u32 = 6;
    let mut tree = CoreNode::Leaf(0);
    for pane in 1..PANES {
        tree.split(&0, pane, CoreDir::Horizontal, 0.5)
            .expect("pane 0 is in the tree");
    }
    assert_eq!(
        project_core(&tree),
        registry_spine(PANES as usize),
        "LayoutNode::split no longer nests a new pane down the left spine; \
         revisit DEEP_SPINE_PANES and the blob depth analysis"
    );
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

/// The three ratio domains, asserted as a map. If they are ever unified this
/// fails by construction: update the map, do not weaken it.
#[test]
fn ratio_domains_diverge_by_design_and_here_is_the_map() {
    // --- phux_core: the open interval (0.0, 1.0), per ADR-0012. ------------
    let mut tree = CoreNode::Leaf(0);
    let mut next = 1_u32;
    let mut attempt = |ratio: f32| -> Result<(), LayoutError<u32>> {
        next += 1;
        tree.split(&0, next, CoreDir::Horizontal, ratio)
    };
    for rejected in [0.0, 1.0, -0.5, 1.5, f32::NAN, f32::INFINITY] {
        assert!(
            matches!(attempt(rejected), Err(LayoutError::InvalidRatio(_))),
            "phux_core must reject ratio {rejected} (ADR-0012: open interval)"
        );
    }
    for accepted in [f32::EPSILON, 0.5, 1.0 - f32::EPSILON] {
        assert!(
            attempt(accepted).is_ok(),
            "phux_core must accept ratio {accepted}"
        );
    }

    // --- L3 envelope: closed [0.0, 1.0], finite only; wider than core because
    // the TUI banks unapplied `resize-pane` ratios (ADR-0048).
    for accepted in [0.0, 1.0, f32::EPSILON, 0.5] {
        let node = build_l3(&Shape::split(
            Axis::SideBySide,
            accepted,
            Shape::Pane(0),
            Shape::Pane(1),
        ));
        assert!(
            round_trip_layout_through_the_envelope(&node).is_ok(),
            "the envelope must carry ratio {accepted}"
        );
    }
    for rejected in [-0.5, 1.5, f32::NAN, f32::INFINITY] {
        let node = build_l3(&Shape::split(
            Axis::SideBySide,
            rejected,
            Shape::Pane(0),
            Shape::Pane(1),
        ));
        assert!(
            matches!(
                round_trip_layout_through_the_envelope(&node),
                Err(LayoutDecodeError::MalformedRatio(_))
            ),
            "the envelope must reject ratio {rejected}"
        );
    }

    // --- upgrade blob: no validation; out-of-domain finite ratios survive...
    for unchecked in [-0.5, 1.5, 0.0, 1.0] {
        let blob = LayoutBlob::Split {
            dir: SplitDirBlob::Horizontal,
            ratio: unchecked,
            left: Box::new(LayoutBlob::Leaf(1)),
            right: Box::new(LayoutBlob::Leaf(2)),
        };
        let json = serde_json::to_vec(&blob).expect("serializes");
        let back: LayoutBlob = serde_json::from_slice(&json).expect("deserializes");
        assert_eq!(back, blob, "the blob must carry ratio {unchecked} verbatim");
    }
    // ...but a non-finite ratio becomes JSON `null` and then fails to parse,
    // taking the whole blob with it.
    for lossy in [f32::NAN, f32::INFINITY] {
        let blob = LayoutBlob::Split {
            dir: SplitDirBlob::Horizontal,
            ratio: lossy,
            left: Box::new(LayoutBlob::Leaf(1)),
            right: Box::new(LayoutBlob::Leaf(2)),
        };
        let json = serde_json::to_vec(&blob).expect("serializes");
        assert!(
            String::from_utf8_lossy(&json).contains("null"),
            "a non-finite blob ratio serializes as JSON null"
        );
        assert!(
            serde_json::from_slice::<LayoutBlob>(&json).is_err(),
            "and does not parse back"
        );
    }
}

/// None of the three encodings imposes a depth bound of its own — see the
/// module doc's asymmetry 2.
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

/// A window deep enough to pass `serde_json`'s default recursion limit (two
/// JSON levels per split: 62 panes fit, 63 did not) in an older image's
/// upgrade blob must stay readable; before the fix the blob was written but
/// unreadable and the whole server state was lost on resume.
#[test]
fn a_window_deep_enough_to_reach_the_json_recursion_limit_survives_an_upgrade() {
    let panes = DEEP_SPINE_PANES;
    assert!(
        panes > 62,
        "the corpus spine must be past the 62-pane serde_json ceiling or this \
         test proves nothing"
    );
    let spine = registry_spine(panes);
    let bytes = one_window_blob_bytes(&build_blob(&spine), u32::try_from(panes).expect("small"));
    let restored = StateBlob::from_bytes(&bytes).unwrap_or_else(|e| {
        panic!(
            "a {panes}-pane window made the whole upgrade blob unreadable \
             ({} bytes): {e}",
            bytes.len()
        )
    });
    assert_eq!(
        project_blob(
            restored.windows[0]
                .layout
                .as_ref()
                .expect("the restored window has a layout"),
            &blob_index_map(&spine),
        ),
        spine,
        "the deep layout changed crossing the upgrade blob"
    );
}
