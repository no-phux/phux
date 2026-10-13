//! Conformance: the three split-tree encodings must describe the same tree.
//!
//! The ADR-0012 split tree exists as `phux_core::arrangement::LayoutNode`
//! (the generic tree a session arrangement's groups are made of,
//! ADR-0160), `phux_client::layout::LayoutNode` (the client tree every
//! consumer persists as the L3 `phux.tui.layout/v1` CBOR envelope,
//! `docs/spec/L3.md` §3.2), and `phux_server::upgrade::blob::LayoutBlob` (the
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

use std::collections::HashMap;

use phux_client::layout::{
    ArrangementDoc, LayoutDecodeError, LayoutNode as L3Node, SplitDir as L3Dir, leaves,
};
use phux_core::arrangement::{LayoutError, LayoutNode, SplitDir as CoreDir};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_server::upgrade::blob::{LayoutBlob, SplitDirBlob, StateBlob};

/// The core tree with the corpus pane index as its leaf.
type CoreNode = LayoutNode<u32>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// Two rectangles side by side, divided by a vertical bar.
    SideBySide,
    /// Two rectangles stacked, divided by a horizontal bar.
    Stacked,
}

#[derive(Debug, Clone)]
enum Shape {
    /// A single pane, identified by its corpus-local index.
    Pane(u32),
    /// An interior split. `first` is the left/top child — the one `ratio`
    /// describes.
    Divide {
        axis: Axis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
}

/// Bit-exact on `ratio`: a projection that rounds, negates a zero, or turns a
/// value into a NaN must not compare equal to the shape it came from.
impl PartialEq for Shape {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pane(a), Self::Pane(b)) => a == b,
            (
                Self::Divide {
                    axis: axis_a,
                    ratio: ratio_a,
                    first: first_a,
                    second: second_a,
                },
                Self::Divide {
                    axis: axis_b,
                    ratio: ratio_b,
                    first: first_b,
                    second: second_b,
                },
            ) => {
                axis_a == axis_b
                    && ratio_a.to_bits() == ratio_b.to_bits()
                    && first_a == first_b
                    && second_a == second_b
            }
            _ => false,
        }
    }
}

impl Shape {
    fn split(axis: Axis, ratio: f32, first: Self, second: Self) -> Self {
        Self::Divide {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Pane indices in left-to-right traversal order.
    fn panes(&self) -> Vec<u32> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes(&self, out: &mut Vec<u32>) {
        match self {
            Self::Pane(i) => out.push(*i),
            Self::Divide { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }
}

/// Panes in the deep-spine entry: each new pane nests one level down the
/// left spine, as an older image's registry built a window, so this is a
/// 64-deep tree, past `serde_json`'s default recursion limit for the upgrade
/// blob.
const DEEP_SPINE_PANES: usize = 64;

/// One shape per interesting structural or numeric case. Every entry is fed
/// through all three encodings.
fn corpus() -> Vec<(&'static str, Shape)> {
    vec![
        ("single leaf", Shape::Pane(0)),
        (
            "one side-by-side split",
            Shape::split(Axis::SideBySide, 0.5, Shape::Pane(0), Shape::Pane(1)),
        ),
        (
            "one stacked split",
            Shape::split(Axis::Stacked, 0.5, Shape::Pane(0), Shape::Pane(1)),
        ),
        (
            "nested on both axes, asymmetric",
            Shape::split(
                Axis::SideBySide,
                0.25,
                Shape::split(Axis::Stacked, 0.75, Shape::Pane(0), Shape::Pane(1)),
                Shape::split(
                    Axis::SideBySide,
                    0.5,
                    Shape::Pane(2),
                    Shape::split(Axis::Stacked, 0.125, Shape::Pane(3), Shape::Pane(4)),
                ),
            ),
        ),
        (
            "ratio at the exact boundaries",
            Shape::split(
                Axis::SideBySide,
                0.0,
                Shape::Pane(0),
                Shape::split(Axis::Stacked, 1.0, Shape::Pane(1), Shape::Pane(2)),
            ),
        ),
        (
            "ratio just inside the boundaries",
            Shape::split(
                Axis::Stacked,
                f32::EPSILON,
                Shape::Pane(0),
                Shape::split(
                    Axis::SideBySide,
                    1.0 - f32::EPSILON,
                    Shape::Pane(1),
                    Shape::Pane(2),
                ),
            ),
        ),
        (
            "deep asymmetric left spine",
            registry_spine(DEEP_SPINE_PANES),
        ),
        ("balanced tree, depth 6", balanced(6, &mut 0)),
    ]
}

/// The tree an older image's registry built for `panes` panes in one
/// window: the active pane 0 is split each time, so pane 1 is the outermost
/// right child. Repeated [`LayoutNode::split`] of pane 0 builds the same
/// shape ([`core_split_builds_the_registry_spine`]).
fn registry_spine(panes: usize) -> Shape {
    let mut node = Shape::Pane(0);
    for i in (1..u32::try_from(panes).expect("corpus is small")).rev() {
        node = Shape::split(Axis::SideBySide, 0.5, node, Shape::Pane(i));
    }
    node
}

/// A fully balanced tree of `depth` splits, alternating axis by level.
fn balanced(depth: u32, next: &mut u32) -> Shape {
    if depth == 0 {
        let leaf = Shape::Pane(*next);
        *next += 1;
        return leaf;
    }
    let axis = if depth.is_multiple_of(2) {
        Axis::SideBySide
    } else {
        Axis::Stacked
    };
    let first = balanced(depth - 1, next);
    let second = balanced(depth - 1, next);
    Shape::split(axis, 0.5, first, second)
}

fn build_core(shape: &Shape) -> CoreNode {
    match shape {
        Shape::Pane(i) => CoreNode::Leaf(*i),
        Shape::Divide {
            axis,
            ratio,
            first,
            second,
        } => CoreNode::Split {
            dir: match axis {
                Axis::SideBySide => CoreDir::Horizontal,
                Axis::Stacked => CoreDir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(build_core(first)),
            right: Box::new(build_core(second)),
        },
    }
}

fn project_core(node: &CoreNode) -> Shape {
    match node {
        CoreNode::Leaf(index) => Shape::Pane(*index),
        CoreNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Shape::Divide {
            axis: match dir {
                CoreDir::Horizontal => Axis::SideBySide,
                CoreDir::Vertical => Axis::Stacked,
            },
            ratio: *ratio,
            first: Box::new(project_core(left)),
            second: Box::new(project_core(right)),
        },
    }
}

const fn l3_pane_id(index: u32) -> WireResourceId {
    WireResourceId::new(index + 1)
}

fn build_l3(shape: &Shape) -> L3Node {
    match shape {
        Shape::Pane(i) => L3Node::Leaf(l3_pane_id(*i)),
        Shape::Divide {
            axis,
            ratio,
            first,
            second,
        } => L3Node::Split {
            dir: match axis {
                Axis::SideBySide => L3Dir::Horizontal,
                Axis::Stacked => L3Dir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(build_l3(first)),
            right: Box::new(build_l3(second)),
        },
    }
}

fn project_l3(node: &L3Node) -> Shape {
    match node {
        L3Node::Leaf(tid) => Shape::Pane(
            tid.local_id()
                .expect("corpus builds Local ids only")
                .checked_sub(1)
                .expect("L3 pane ids start at 1"),
        ),
        L3Node::Split {
            dir,
            ratio,
            left,
            right,
        } => Shape::Divide {
            axis: match dir {
                L3Dir::Horizontal => Axis::SideBySide,
                L3Dir::Vertical => Axis::Stacked,
            },
            ratio: *ratio,
            first: Box::new(project_l3(left)),
            second: Box::new(project_l3(right)),
        },
    }
}

fn build_blob(shape: &Shape) -> LayoutBlob {
    match shape {
        Shape::Pane(i) => LayoutBlob::Leaf(i + 1),
        Shape::Divide {
            axis,
            ratio,
            first,
            second,
        } => LayoutBlob::Split {
            dir: match axis {
                Axis::SideBySide => SplitDirBlob::Horizontal,
                Axis::Stacked => SplitDirBlob::Vertical,
            },
            ratio: *ratio,
            left: Box::new(build_blob(first)),
            right: Box::new(build_blob(second)),
        },
    }
}

fn project_blob(node: &LayoutBlob, index_of: &HashMap<u32, u32>) -> Shape {
    match node {
        LayoutBlob::Leaf(wire) => Shape::Pane(index_of[wire]),
        LayoutBlob::Split {
            dir,
            ratio,
            left,
            right,
        } => Shape::Divide {
            axis: match dir {
                SplitDirBlob::Horizontal => Axis::SideBySide,
                SplitDirBlob::Vertical => Axis::Stacked,
            },
            ratio: *ratio,
            first: Box::new(project_blob(left, index_of)),
            second: Box::new(project_blob(right, index_of)),
        },
    }
}

/// Identity map for the blob's `u32` keys, matching [`build_blob`].
fn blob_index_map(shape: &Shape) -> HashMap<u32, u32> {
    shape.panes().into_iter().map(|i| (i + 1, i)).collect()
}

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

/// Encode `node` as the one window of a v3 layout envelope (L3.md §3.2) and
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
