//! Conformance: the three split-tree encodings must describe the same tree.
//!
//! The ADR-0012 split tree exists as `phux_core::window::LayoutNode` (the
//! server's domain model), `phux_client::layout::LayoutNode` (the client tree
//! every consumer persists as the L3 `phux.tui.layout/v1` CBOR envelope,
//! `docs/spec/L3.md` §3.2), and `phux_server::upgrade::blob::LayoutBlob` (the
//! serde carrier for graceful upgrade, keyed by wire id). The wire carries no
//! split tree (ADR-0030). Each has its own round-trip test; this file builds
//! one corpus into all three and projects them back to a shared normal form
//! so a drift in axis convention, child order, or ratio domain fails on the
//! commit that caused it.
//!
//! There is no core-to-L3 conversion (the server never writes a layout), so
//! core and L3 are compared as independently built shapes; core-to-blob
//! drives the real upgrade producer.
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
    LayoutDecodeError, LayoutNode as L3Node, SplitDir as L3Dir, Workspace, leaves,
};
use phux_core::ids::{ResourceId as CoreResourceId, WindowId as CoreWindowId};
use phux_core::registry::Registry;
use phux_core::window::{LayoutError, LayoutNode as CoreNode, SplitDir as CoreDir};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_server::state::ServerState;
use phux_server::upgrade::blob::{LayoutBlob, SplitDirBlob, StateBlob};

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

/// Panes in the deep-spine entry: the registry nests each new pane one level
/// down the left spine, so this is a 64-deep tree, past `serde_json`'s
/// default recursion limit for the upgrade blob.
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

/// The tree `Registry::new_terminal` builds for `panes` panes: the active
/// pane 0 is split each time, so pane 1 is the outermost right child (checked
/// against the real registry in
/// [`core_built_tree_matches_the_blob_the_upgrade_writes`]).
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

fn build_core(shape: &Shape, ids: &[CoreResourceId]) -> CoreNode {
    match shape {
        Shape::Pane(i) => CoreNode::Leaf(ids[*i as usize]),
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
            left: Box::new(build_core(first, ids)),
            right: Box::new(build_core(second, ids)),
        },
    }
}

fn project_core(node: &CoreNode, index_of: &HashMap<CoreResourceId, u32>) -> Shape {
    match node {
        CoreNode::Leaf(tid) => Shape::Pane(index_of[tid]),
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
            first: Box::new(project_core(left, index_of)),
            second: Box::new(project_core(right, index_of)),
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

/// A registry holding one session, one window, and `panes` terminals; returns
/// the ids in creation order so pane index `i` maps to `ids[i]`.
fn seeded_registry(panes: usize) -> (Registry, CoreWindowId, Vec<CoreResourceId>) {
    let mut reg = Registry::new();
    let sid = reg.new_session("conformance".to_owned());
    let wid = reg.new_window(sid).expect("session exists");
    let ids = (0..panes)
        .map(|_| reg.new_terminal(wid).expect("window exists"))
        .collect();
    (reg, wid, ids)
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
        let (_reg, _wid, core_ids) = seeded_registry(indices.len());
        let index_of_core: HashMap<CoreResourceId, u32> = core_ids
            .iter()
            .enumerate()
            .map(|(i, tid)| (*tid, u32::try_from(i).expect("corpus is small")))
            .collect();
        let index_of_blob = blob_index_map(&shape);

        let core = build_core(&shape, &core_ids);
        let l3 = build_l3(&shape);
        let blob = build_blob(&shape);

        assert_eq!(
            project_core(&core, &index_of_core),
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
        // the client walks to tile panes, so a builder that swapped `left` and
        // `right` while keeping the tree isomorphic would still be a bug.
        let core_leaf_order: Vec<u32> = core.leaves().iter().map(|t| index_of_core[t]).collect();
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
    let mut workspace = Workspace::single(first);
    workspace.windows[0].state.tree = Some(node.clone());
    let bytes = workspace.encode_cbor().expect("envelope encodes");
    let decoded = Workspace::decode_cbor(&bytes)?;
    Ok(decoded.windows[0]
        .state
        .tree
        .clone()
        .expect("layout survived the round trip"))
}

/// Drives the real core-to-blob conversion: build a window through the
/// registry and assert `ServerState`'s upgrade blob carries the core tree
/// (actor-less panes, so nothing blocks on a mailbox).
#[tokio::test(flavor = "current_thread")]
async fn core_built_tree_matches_the_blob_the_upgrade_writes() {
    const PANES: usize = 6;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (state, wid, index_of_core, index_of_blob) = state_with_one_window(PANES);
            let blob = state.build_upgrade_blob(7).await;

            let core_layout = state
                .registry()
                .window(wid)
                .expect("window exists")
                .layout
                .clone()
                .expect("a window with panes has a layout");
            let blob_layout = blob.windows[0]
                .layout
                .clone()
                .expect("the blob carries the window's layout");

            assert_eq!(
                project_blob(&blob_layout, &index_of_blob),
                project_core(&core_layout, &index_of_core),
                "the upgrade blob's tree is not the core tree"
            );

            // And the shape is the left spine [`registry_spine`] documents.
            // Stated here so the depth reasoning below rests on the real
            // registry rather than on a guess about what it builds.
            assert_eq!(
                project_core(&core_layout, &index_of_core),
                registry_spine(PANES),
                "Registry::new_terminal no longer builds a left spine; \
                 revisit DEEP_SPINE_PANES and the blob depth analysis"
            );
        })
        .await;
}

/// A `ServerState` holding one session, one window, and `panes` wire-interned
/// panes, plus the two index maps needed to project its core and blob trees
/// onto the same normal form.
fn state_with_one_window(
    panes: usize,
) -> (
    ServerState,
    CoreWindowId,
    HashMap<CoreResourceId, u32>,
    HashMap<u32, u32>,
) {
    let mut state = ServerState::new();
    let sid = state.registry_mut().new_session("conformance".to_owned());
    let wid = state
        .registry_mut()
        .new_window(sid)
        .expect("session exists");
    let _ = state.idspace.intern_session(sid);
    let _ = state.intern_window_wire(wid);

    let mut index_of_core = HashMap::new();
    let mut index_of_blob = HashMap::new();
    for i in 0..panes {
        let tid = state
            .registry_mut()
            .new_terminal(wid)
            .expect("window exists");
        let wire = state
            .intern_terminal_wire(tid)
            .local_id()
            .expect("server mints Local wire ids");
        let index = u32::try_from(i).expect("small");
        index_of_core.insert(tid, index);
        index_of_blob.insert(wire, index);
    }
    (state, wid, index_of_core, index_of_blob)
}

/// The three ratio domains, asserted as a map. If they are ever unified this
/// fails by construction: update the map, do not weaken it.
#[test]
fn ratio_domains_diverge_by_design_and_here_is_the_map() {
    // --- phux_core: the open interval (0.0, 1.0), per ADR-0012. ------------
    let (mut reg, wid, ids) = seeded_registry(1);
    let mut attempt = |ratio: f32| -> Result<(), LayoutError> {
        let new_pane = reg.new_terminal(wid).expect("window exists");
        let window = reg.window_mut(wid).expect("window exists");
        window.split(ids[0], new_pane, CoreDir::Horizontal, ratio)
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
#[tokio::test(flavor = "current_thread")]
async fn depth_bounds_diverge_by_design_and_here_is_the_map() {
    // One pane past the deep-spine corpus entry.
    let panes = DEEP_SPINE_PANES + 1;
    let too_deep = registry_spine(panes);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // --- phux_core: no cap at all. The registry builds this happily.
            let (state, wid, index_of_core, index_of_blob) = state_with_one_window(panes);
            let core_layout = state
                .registry()
                .window(wid)
                .expect("window exists")
                .layout
                .clone()
                .expect("a window with panes has a layout");
            assert_eq!(
                project_core(&core_layout, &index_of_core),
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
            // boundary — the bytes come from this binary's own predecessor
            // image.
            let blob = state.build_upgrade_blob(7).await;
            let bytes = blob.to_bytes().expect("serializes");
            let restored = StateBlob::from_bytes(&bytes).expect("deserializes");
            assert_eq!(
                project_blob(
                    restored.windows[0]
                        .layout
                        .as_ref()
                        .expect("the restored window has a layout"),
                    &index_of_blob,
                ),
                too_deep,
                "the blob must carry the deep spine unchanged"
            );
        })
        .await;
}

/// A window deep enough to pass `serde_json`'s default recursion limit (two
/// JSON levels per split: 62 panes fit, 63 did not) must survive
/// `phux upgrade`; before the fix the blob was written but unreadable and the
/// whole server state was lost on resume.
#[tokio::test(flavor = "current_thread")]
async fn a_window_deep_enough_to_reach_the_json_recursion_limit_survives_an_upgrade() {
    let panes = DEEP_SPINE_PANES;
    assert!(
        panes > 62,
        "the corpus spine must be past the 62-pane serde_json ceiling or this \
         test proves nothing"
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (state, _wid, _index_of_core, index_of_blob) = state_with_one_window(panes);
            let blob = state.build_upgrade_blob(7).await;
            let expected = project_blob(
                blob.windows[0]
                    .layout
                    .as_ref()
                    .expect("the window has a layout"),
                &index_of_blob,
            );

            let bytes = blob.to_bytes().expect("a deep blob still serializes");
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
                    &index_of_blob,
                ),
                expected,
                "the deep layout changed crossing the upgrade blob"
            );
        })
        .await;
}
