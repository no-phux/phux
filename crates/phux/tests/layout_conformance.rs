//! Conformance: the three split-tree encodings must describe the same tree.
//!
//! The binary split tree of [ADR-0012] exists three times in this workspace,
//! deliberately:
//!
//!   * `phux_core::window::{LayoutNode, SplitDir}` — the domain tree, keyed by
//!     the registry's opaque `TerminalId`;
//!   * `phux_protocol::wire::info::{LayoutNode, SplitDir}` — the wire mirror,
//!     keyed by the wire `TerminalId`, with its own tag bytes and its own
//!     decode-time validation;
//!   * `phux_server::upgrade::blob::{LayoutBlob, SplitDirBlob}` — the
//!     graceful-upgrade carrier, keyed by `u32` wire id, deriving `Serialize`
//!     / `Deserialize`.
//!
//! Each duplication is a decision, and each rationale is sound. The
//! core/protocol split is `phux-protocol`'s independence rule, written down at
//! the top of `crates/phux-protocol/src/wire/info.rs`: the wire crate cannot
//! depend on the domain crate, so it mirrors the shape instead. The blob
//! exists because [ADR-0032] needs a `serde`-derivable shape keyed by *wire*
//! id — generational `SlotMap` keys are meaningless in a fresh process, so the
//! carrier cannot reuse the core type.
//!
//! What did not exist is a guard. Each encoding has its own round-trip test
//! and each passes in isolation, so a drift in axis convention (which of
//! `Horizontal`/`Vertical` means side-by-side), in child order (which child
//! `ratio` belongs to), or in the accepted ratio domain shows up only as a
//! layout that looks subtly wrong after an upgrade or a reattach — months
//! later, with nobody left who remembers which of the three moved. One
//! abstract corpus, built into all three and projected back to a shared normal
//! form, turns that into a red test on the commit that caused it.
//!
//! # There is no core-to-wire conversion (yet), and that shapes this file
//!
//! `phux_server`'s snapshot builder ships `layout: None` unconditionally —
//! see the comment at `crates/phux-server/src/state/snapshot.rs`, which defers
//! the `phux_core::LayoutNode` → `phux_protocol::wire::info::LayoutNode`
//! translation to a later ticket. So encodings 1 and 2 cannot drift *through*
//! a conversion, because there is no conversion. They can only drift as type
//! shapes and validation domains, which is therefore what this file asserts:
//! one corpus built independently into each encoding, not a conversion
//! round-trip. Encodings 1 and 3 *do* have a live conversion
//! (`crates/phux-server/src/state/upgrade_blob.rs`), and
//! [`core_built_tree_matches_the_blob_the_upgrade_writes`] drives the real
//! producer rather than a re-implementation of it. When the deferred
//! core↔wire conversion lands, add the same kind of end-to-end assertion for
//! it here; the corpus and the normal form are already in place.
//!
//! # Why this file lives in `crates/phux/tests/`
//!
//! The test must see all three encodings at once. The `phux` binary crate
//! already depends on `phux-core`, `phux-protocol`, and `phux-server`, so this
//! costs no new crate, no new dependency edge, and no new published surface —
//! `phux` is `publish = false`. It is *not* the only workspace member that
//! could host it: `phux-server` depends on core and protocol and defines
//! `LayoutBlob` itself, so its own integration tests could reach all three
//! too. The tie-breaker is precedent — `crates/phux/tests/cell_projection_conformance.rs`
//! is the existing cross-crate conformance file and this is its sibling.
//!
//! # What is deliberately outside the corpus
//!
//! `phux_client_core::layout::{LayoutNode, SplitDir}` are **not** a fourth
//! encoding: `crates/phux-client-core/src/layout/mod.rs` re-exports the wire
//! types verbatim (`pub use phux_protocol::wire::info::{LayoutNode,
//! SplitDir}`). Only `LayoutState` / `WindowState` / `Workspace` are
//! client-local, and those are a different *model* (per [ADR-0017] and
//! [ADR-0019]), not a mirror of this tree; including them would be comparing a
//! screen model against a domain model.
//!
//! There are two genuine further encodings, and both are unreachable from an
//! integration test:
//!
//!   * `CborLayoutNode` / `CborSplitDir` in
//!     `crates/phux-client-core/src/layout/serialize.rs` — the serde shadow
//!     behind the `phux.tui.layout/v1` L3 envelope. `pub(super)`.
//!   * `WorkspaceLayoutNode` / `WorkspaceSplitDir` in
//!     `crates/phux/src/commands/workspace/archive/model.rs` — the archive
//!     shape for `phux workspace save`. `pub(super)`, and doubly out of reach
//!     because `crates/phux` has no lib target, so a file in
//!     `crates/phux/tests/` can never name `phux::` at all.
//!
//! Both stay guarded by their crates' own unit tests. Widening this corpus to
//! cover them needs a visibility change, which is a bigger decision than
//! adding a test.
//!
//! # Where the three cannot agree, and why
//!
//! Two asymmetries are structural, not drift. Both are asserted here rather
//! than normalised away in silence:
//!
//!   1. **The three accept different ratio domains.** [ADR-0012] fixes `ratio`
//!      in the *open* interval `(0.0, 1.0)` and `phux_core`'s `Window::split`
//!      enforces exactly that. The wire decoder accepts the *closed* interval
//!      and rejects only non-finite values — deliberately, because the TUI's
//!      `resize-pane` banks unbounded ratios it has not yet applied
//!      (`crates/phux-client-core/src/multi_pane/layout.rs`, [ADR-0048]), and
//!      a transport that rejected the endpoints would drop legitimate client
//!      state. `LayoutBlob` validates nothing at all. Pinned by
//!      [`ratio_domains_diverge_by_design_and_here_is_the_map`]. **If a later
//!      change unifies the three domains — which would be the right long-term
//!      move — that test fails by construction. That is the test working, not
//!      a regression: update the map, do not delete the assertion.**
//!   2. **Only the wire bounds depth.** `phux_protocol` caps decode recursion
//!      at `MAX_LAYOUT_DEPTH` because it parses bytes from a peer; `phux_core`
//!      has no cap at all, and the blob's ceiling is whatever `serde_json`
//!      will parse. Pinned by
//!      [`depth_bounds_diverge_by_design_and_here_is_the_map`]. That core has
//!      no gate is a real gap — a 200-pane window is already un-sendable over
//!      the wire — but closing it means making `Registry::new_terminal`'s
//!      swallowed split error observable, which is a behaviour change and its
//!      own ticket, not a rider on a test.
//!
//! Writing asymmetry 2 down is what found the bug this change also fixes:
//! `serde_json`'s default 128-level recursion limit made a 63-pane window's
//! `StateBlob` writable but not readable, so `phux upgrade` on such a server
//! lost **all** state, not just that window. See
//! [`a_window_deep_enough_to_reach_the_json_recursion_limit_survives_an_upgrade`]
//! and the `# Depth` note in `crates/phux-server/src/upgrade/blob.rs`.
//!
//! The corpus itself carries `ratio` values in every encoding's *carrying*
//! capacity (including the exact `0.0` / `1.0` endpoints), because what the
//! corpus tests is the shape of the data, not the constructors' guards. The
//! guards are test 1 above.
//!
//! [ADR-0012]: ../../../ADR/0012-tui-layout-model.md
//! [ADR-0017]: ../../../ADR/0017-tui-client-architecture.md
//! [ADR-0019]: ../../../ADR/0019-tui-multi-pane-rendering.md
//! [ADR-0032]: ../../../ADR/0032-graceful-upgrade.md
//! [ADR-0048]: ../../../ADR/0048-tui-command-surface.md

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::collections::HashMap;

use phux_core::ids::{TerminalId as CoreTerminalId, WindowId as CoreWindowId};
use phux_core::registry::Registry;
use phux_core::window::{LayoutError, LayoutNode as CoreNode, SplitDir as CoreDir};
use phux_protocol::ids::{
    ClientId, SessionId as WireSessionId, TerminalId as WireTerminalId, WindowId as WireWindowId,
};
use phux_protocol::wire::error::DecodeError;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{
    LayoutNode as WireNode, MAX_LAYOUT_DEPTH, SessionSnapshot, SplitDir as WireDir, WindowInfo,
};
use phux_server::state::ServerState;
use phux_server::upgrade::blob::{LayoutBlob, SplitDirBlob, StateBlob};

// ---------------------------------------------------------------------------
// The shared normal form.
//
// Spelled in *semantic* vocabulary, not any encoding's. `Horizontal` is the
// word all three use for a side-by-side split (a vertical divider bar), which
// is exactly the kind of naming that invites a silent flip. Projecting through
// `SideBySide` / `Stacked` means a rename that changes the meaning of
// `Horizontal` in one encoding shows up as a corpus mismatch instead of
// passing on the strength of the shared spelling.
// ---------------------------------------------------------------------------

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
        first: Box<Shape>,
        second: Box<Shape>,
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

// ---------------------------------------------------------------------------
// The corpus.
// ---------------------------------------------------------------------------

/// Panes in the deep-spine corpus entry.
///
/// `Registry::new_terminal` always splits the window's *active* pane, and
/// splitting never moves `active`, so every new pane nests one level further
/// down the **left** spine: `n` panes is `n - 1` splits nested `n - 1` deep.
/// `MAX_LAYOUT_DEPTH` panes is therefore exactly the deepest single-window
/// tree the wire codec will carry, which makes it the right ceiling for a
/// corpus meant to cover what the system can legitimately produce.
const DEEP_SPINE_PANES: usize = MAX_LAYOUT_DEPTH;

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

/// The tree `Registry::new_terminal` actually builds for `panes` panes.
///
/// Pane 0 stays the window's active leaf for the whole sequence, so pane `i`
/// becomes the *right* sibling of the subtree built so far and the nesting
/// runs down the left edge. The outermost right child is therefore pane 1 and
/// the innermost is pane `n - 1`, which is the reverse of the naive guess —
/// it is asserted against the real registry in
/// [`core_built_tree_matches_the_blob_the_upgrade_writes`] rather than trusted.
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

// ---------------------------------------------------------------------------
// Encoding 1: phux_core::window::LayoutNode.
//
// Keyed by opaque `TerminalId`s, which only a `Registry` can mint, so the
// builder takes the ids it should use for pane indices `0..n`.
// ---------------------------------------------------------------------------

fn build_core(shape: &Shape, ids: &[CoreTerminalId]) -> CoreNode {
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

fn project_core(node: &CoreNode, index_of: &HashMap<CoreTerminalId, u32>) -> Shape {
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

// ---------------------------------------------------------------------------
// Encoding 2: phux_protocol::wire::info::LayoutNode.
//
// Wire pane ids are `index + 1`; `TerminalId::new(0)` is a legal id but a
// confusing sentinel to read in a failure message.
// ---------------------------------------------------------------------------

const fn wire_pane_id(index: u32) -> WireTerminalId {
    WireTerminalId::new(index + 1)
}

fn build_wire(shape: &Shape) -> WireNode {
    match shape {
        Shape::Pane(i) => WireNode::Leaf(wire_pane_id(*i)),
        Shape::Divide {
            axis,
            ratio,
            first,
            second,
        } => WireNode::Split {
            dir: match axis {
                Axis::SideBySide => WireDir::Horizontal,
                Axis::Stacked => WireDir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(build_wire(first)),
            right: Box::new(build_wire(second)),
        },
    }
}

fn project_wire(node: &WireNode) -> Shape {
    match node {
        WireNode::Leaf(tid) => Shape::Pane(
            tid.local_id()
                .expect("corpus builds Local wire ids only")
                .checked_sub(1)
                .expect("wire pane ids start at 1"),
        ),
        WireNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Shape::Divide {
            axis: match dir {
                WireDir::Horizontal => Axis::SideBySide,
                WireDir::Vertical => Axis::Stacked,
                other => panic!("wire SplitDir grew a variant this corpus does not map: {other:?}"),
            },
            ratio: *ratio,
            first: Box::new(project_wire(left)),
            second: Box::new(project_wire(right)),
        },
        // Both wire types are `#[non_exhaustive]` (additive growth is
        // non-breaking there). A new variant is not something this corpus can
        // silently ignore: it is a fourth tree shape the other two encodings
        // do not have, and it needs a decision, not a default.
        other => panic!("wire LayoutNode grew a variant this corpus does not map: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Encoding 3: phux_server::upgrade::blob::LayoutBlob.
//
// Keyed by the same `u32` wire id the wire encoding uses, so `build_blob` and
// `build_wire` agree on `index + 1`.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// A registry holding one session, one window, and `panes` terminals; returns
/// the ids in creation order so pane index `i` maps to `ids[i]`.
fn seeded_registry(panes: usize) -> (Registry, CoreWindowId, Vec<CoreTerminalId>) {
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

// ---------------------------------------------------------------------------
// The guard.
// ---------------------------------------------------------------------------

/// One abstract corpus, built independently into all three encodings and
/// projected back. Any drift in axis vocabulary, child order, ratio placement,
/// or leaf ordering shows up as an inequality naming the corpus entry.
#[test]
fn all_three_encodings_describe_the_same_tree() {
    for (name, shape) in corpus() {
        let indices = shape.panes();
        let (_reg, _wid, core_ids) = seeded_registry(indices.len());
        let index_of_core: HashMap<CoreTerminalId, u32> = core_ids
            .iter()
            .enumerate()
            .map(|(i, tid)| (*tid, u32::try_from(i).expect("corpus is small")))
            .collect();
        let index_of_blob = blob_index_map(&shape);

        let core = build_core(&shape, &core_ids);
        let wire = build_wire(&shape);
        let blob = build_blob(&shape);

        assert_eq!(
            project_core(&core, &index_of_core),
            shape,
            "{name}: phux_core encoding does not project back to the corpus shape"
        );
        assert_eq!(
            project_wire(&wire),
            shape,
            "{name}: phux_protocol encoding does not project back to the corpus shape"
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

/// Each encoding survives its own serialization round-trip *and still equals
/// the corpus shape afterwards*, so a codec bug cannot hide behind a
/// symmetric encode/decode pair that agrees with itself.
///
/// The core encoding has no serialization of its own — it never leaves the
/// process — so only the wire and the blob have a round trip to take.
#[test]
fn wire_and_blob_round_trips_land_back_on_the_corpus_shape() {
    for (name, shape) in corpus() {
        // Wire: encode a real ATTACHED frame and decode it back.
        let decoded = round_trip_layout_through_a_frame(&build_wire(&shape))
            .unwrap_or_else(|e| panic!("{name}: ATTACHED frame carrying the layout failed: {e:?}"));
        assert_eq!(
            project_wire(&decoded),
            shape,
            "{name}: layout changed crossing the wire"
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

/// Encode `node` into an `ATTACHED` frame and decode the frame back, returning
/// the layout the decoder produced.
fn round_trip_layout_through_a_frame(node: &WireNode) -> Result<WireNode, DecodeError> {
    let snapshot = SessionSnapshot::new(
        WireSessionId::new(1),
        WireWindowId::new(1),
        WireTerminalId::new(1),
    )
    .with_windows(vec![
        WindowInfo::new(WireWindowId::new(1), WireSessionId::new(1), "w")
            .with_layout(Some(node.clone())),
    ]);
    let mut buf = bytes::BytesMut::new();
    FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(1),
    }
    .encode(&mut buf);
    let (frame, tail) = FrameKind::decode(&buf)?;
    assert!(tail.is_empty(), "frame should consume its own bytes");
    let FrameKind::Attached { snapshot, .. } = frame else {
        panic!("decoded a frame that is not ATTACHED");
    };
    Ok(snapshot.windows[0]
        .layout
        .clone()
        .expect("layout survived the round trip"))
}

/// A golden-byte pin on the wire's tag numbering.
///
/// `encode`/`decode` symmetry cannot catch a renumbering — flip both tags and
/// every round-trip test still passes while every already-deployed client
/// misreads the axis. The tag constants are `pub(crate)`, so this reads the
/// numbering off real encoded bytes instead, the same technique
/// `crates/phux-protocol/tests/wire_roundtrip.rs` uses for its hand-rolled
/// frames.
#[test]
fn wire_split_tags_are_pinned_to_their_spec_bytes() {
    // LAYOUT_TAG_SPLIT = 0x01, SPLIT_DIR_HORIZONTAL = 0x00,
    // SPLIT_DIR_VERTICAL = 0x01, LAYOUT_TAG_LEAF = 0x00, and a `Local`
    // TerminalId is tag 0x00 followed by a big-endian u32.
    const RATIO_HALF_BE: [u8; 4] = [0x3F, 0x00, 0x00, 0x00];
    const LEAF_ONE: [u8; 6] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    const LEAF_TWO: [u8; 6] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x02];

    // Compared as bytes rather than as floats: exact, and it states the thing
    // being pinned (this byte string *is* how 0.5 goes on the wire) rather
    // than a numeric equality that would need a tolerance argument.
    assert_eq!(
        RATIO_HALF_BE,
        0.5_f32.to_be_bytes(),
        "the golden ratio bytes below must really be 0.5"
    );

    let mut side_by_side = vec![0x01, 0x00];
    side_by_side.extend_from_slice(&RATIO_HALF_BE);
    side_by_side.extend_from_slice(&LEAF_ONE);
    side_by_side.extend_from_slice(&LEAF_TWO);

    let mut stacked = vec![0x01, 0x01];
    stacked.extend_from_slice(&RATIO_HALF_BE);
    stacked.extend_from_slice(&LEAF_ONE);
    stacked.extend_from_slice(&LEAF_TWO);

    let horizontal = encoded_attached_bytes(&build_wire(&Shape::split(
        Axis::SideBySide,
        0.5,
        Shape::Pane(0),
        Shape::Pane(1),
    )));
    let vertical = encoded_attached_bytes(&build_wire(&Shape::split(
        Axis::Stacked,
        0.5,
        Shape::Pane(0),
        Shape::Pane(1),
    )));

    assert!(
        contains(&horizontal, &side_by_side),
        "a side-by-side split must encode as SPLIT=0x01, dir=0x00; \
         renumbering these tags breaks every deployed client"
    );
    assert!(
        contains(&vertical, &stacked),
        "a stacked split must encode as SPLIT=0x01, dir=0x01"
    );
    assert!(
        !contains(&vertical, &side_by_side),
        "the two axes must not share an encoding"
    );
}

fn encoded_attached_bytes(node: &WireNode) -> Vec<u8> {
    let snapshot = SessionSnapshot::new(
        WireSessionId::new(1),
        WireWindowId::new(1),
        WireTerminalId::new(1),
    )
    .with_windows(vec![
        WindowInfo::new(WireWindowId::new(1), WireSessionId::new(1), "w")
            .with_layout(Some(node.clone())),
    ]);
    let mut buf = bytes::BytesMut::new();
    FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(1),
    }
    .encode(&mut buf);
    buf.to_vec()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The one test that drives the real, private core→blob conversion rather than
/// this file's re-implementation of it: build a window through the registry,
/// ask `ServerState` for the upgrade blob it would hand the next image, and
/// assert the blob's tree is the core tree.
///
/// The panes here carry no actor, which is the documented "recorded from its
/// descriptor with no handoff" path in
/// `crates/phux-server/src/state/upgrade_blob.rs` — nothing is awaited per
/// pane, so this cannot block on a mailbox that never answers.
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
    HashMap<CoreTerminalId, u32>,
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

/// The three encodings accept three different ratio domains. This is the map,
/// asserted rather than normalised away — see the module doc's asymmetry 1.
///
/// If a later change unifies the domains this test fails by construction. That
/// is the test working. Update the map; do not weaken it.
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

    // --- phux_protocol: the closed interval [0.0, 1.0], finite only. ------
    //
    // Wider than core on purpose. The TUI's `resize-pane` banks ratios it has
    // not applied yet (crates/phux-client-core/src/multi_pane/layout.rs,
    // ADR-0048), so a transport that rejected the endpoints would drop
    // legitimate client state. `crates/phux-protocol/tests/wire_roundtrip.rs`
    // pins the same table from the decoder's side.
    for accepted in [0.0, 1.0, f32::EPSILON, 0.5] {
        let node = build_wire(&Shape::split(
            Axis::SideBySide,
            accepted,
            Shape::Pane(0),
            Shape::Pane(1),
        ));
        assert!(
            round_trip_layout_through_a_frame(&node).is_ok(),
            "the wire must carry ratio {accepted}"
        );
    }
    for rejected in [-0.5, 1.5, f32::NAN, f32::INFINITY] {
        let node = build_wire(&Shape::split(
            Axis::SideBySide,
            rejected,
            Shape::Pane(0),
            Shape::Pane(1),
        ));
        assert!(
            matches!(
                round_trip_layout_through_a_frame(&node),
                Err(DecodeError::MalformedLayoutRatio { .. })
            ),
            "the wire must reject ratio {rejected}"
        );
    }

    // --- upgrade blob: no validation at all, and non-finite is lossy. -----
    //
    // The blob is a core→core carrier across one `execve`; it inherits core's
    // domain and checks nothing itself. Out-of-domain finite ratios survive
    // verbatim...
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
    // ...but a non-finite ratio becomes JSON `null` on the way out and then
    // fails to parse on the way back in, taking the *whole* blob with it.
    // Nothing upstream can produce one today (core rejects NaN at the
    // constructor), which is exactly why this is worth writing down.
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

/// The three encodings bound nesting depth three different ways, and only one
/// of them bounds it deliberately — see the module doc's asymmetry 2.
#[tokio::test(flavor = "current_thread")]
async fn depth_bounds_diverge_by_design_and_here_is_the_map() {
    // One pane past the deepest tree the wire will carry.
    let panes = MAX_LAYOUT_DEPTH + 1;
    let too_deep = registry_spine(panes);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // --- phux_core: no cap at all. The registry builds this happily,
            // which is the real gap here: a window this deep is already
            // un-sendable, and core is where a gate would belong.
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

            // --- phux_protocol: capped at MAX_LAYOUT_DEPTH, and the cap earns
            // its keep — the decoder is recursive over bytes from a peer, so
            // an unbounded tree would overflow the stack.
            assert!(
                matches!(
                    round_trip_layout_through_a_frame(&build_wire(&too_deep)),
                    Err(DecodeError::LayoutTooDeep)
                ),
                "the wire must refuse a tree deeper than MAX_LAYOUT_DEPTH"
            );

            // --- upgrade blob: no cap of its own. It carries trees the wire
            // would refuse, because it never crosses a trust boundary — the
            // bytes come from this binary's own predecessor image.
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
                "the blob must carry trees the wire would refuse"
            );
        })
        .await;
}

/// A window with enough panes to nest the blob's JSON past `serde_json`'s
/// default recursion limit must still survive `phux upgrade`.
///
/// `LayoutBlob`'s externally-tagged serde shape costs **two** JSON object
/// levels per split (`{"Split":{...}}`), and `Registry::new_terminal` builds
/// one split per pane down a single spine, so the nesting grows at twice the
/// pane count. Measured against `serde_json`'s default 128-level limit, a
/// window of 62 panes round-trips and 63 does not. Before the fix in
/// `crates/phux-server/src/upgrade/blob.rs`, `StateBlob::to_bytes` happily
/// wrote such a blob and `StateBlob::from_bytes` then refused to read it back
/// — which surfaces as `ServerError::Resume` in
/// `crates/phux-server/src/runtime/resume.rs` and loses the *entire* server
/// state, every session, not just the offending window.
///
/// [`DEEP_SPINE_PANES`] is the deepest single-window tree the wire will carry,
/// so this is a layout the protocol itself considers legal.
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
