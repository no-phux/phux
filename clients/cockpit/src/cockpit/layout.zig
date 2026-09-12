//! The pane layout tree.
//!
//! One tab owns one tree. A leaf is a terminal; a branch divides its rect
//! between two children along an orientation at a fraction. This is the
//! SINGLE source of truth for pane geometry: the painter, the hit-test
//! widget tree, and the PTY sizing pump all consume `resolve()` output
//! rather than deriving rects independently.
//!
//! The tree is fixed-capacity and allocation-free. `max_panes` leaves need
//! `2 * max_panes - 1` nodes, because every split adds one branch and one
//! leaf.

const std = @import("std");
const native_sdk = @import("native_sdk");
const support = @import("phux_support.zig");

const geometry = native_sdk.geometry;
const RectF = geometry.RectF;

pub const TerminalRef = support.TerminalRef;

/// Panes per tab. Ghostty is unbounded; this ceiling keeps the tree
/// allocation-free while being far past what a person splits in practice.
pub const max_panes: usize = 16;
pub const max_nodes: usize = max_panes * 2 - 1;

/// The sentinel for "no node". `max_nodes` is 31, so u8 has room.
pub const none: NodeId = std.math.maxInt(NodeId);
pub const NodeId = u8;

comptime {
    std.debug.assert(max_nodes < none);
}

/// `horizontal` divides left/right (children sit side by side);
/// `vertical` divides top/bottom. This matches Ghostty's naming for
/// "split right" (horizontal) and "split down" (vertical).
pub const Orientation = enum(u8) { horizontal, vertical };

pub const Direction = enum(u8) { left, right, up, down };

pub const Kind = enum(u8) { free, leaf, branch };

pub const Node = struct {
    kind: Kind = .free,
    parent: NodeId = none,
    /// Set when `kind == .leaf`, null otherwise. `TerminalRef` has no null
    /// identity of its own, so the optional carries "this node holds no
    /// terminal" rather than a sentinel ref that could compare equal to a
    /// real one.
    terminal: ?TerminalRef = null,
    /// Valid when `kind == .branch`.
    orientation: Orientation = .horizontal,
    /// Share of the branch's extent given to `first`, in (0, 1).
    fraction: f32 = 0.5,
    first: NodeId = none,
    second: NodeId = none,
};

pub const Pane = struct {
    node: NodeId,
    terminal: TerminalRef,
    rect: RectF,
};

/// A branch's divider, resolved to the strip the pointer can grab.
pub const Divider = struct {
    node: NodeId,
    orientation: Orientation,
    rect: RectF,
    /// The rect the branch itself occupies, so a drag can recompute the
    /// fraction without walking the tree again.
    bounds: RectF,
};

pub const Error = error{ Full, NotALeaf, UnknownNode };

/// Fraction bounds. A branch never collapses a child to nothing; the
/// layout pass additionally enforces pixel minimums.
pub const min_fraction: f32 = 0.05;
pub const max_fraction: f32 = 0.95;

pub const Tree = struct {
    /// Process-local session attachment that published this tree. It travels
    /// with native tab moves, but is never a durable terminal or wire identity.
    attachment_id: ?u64 = null,
    nodes: [max_nodes]Node = [_]Node{.{}} ** max_nodes,
    root: NodeId = none,
    /// Always a leaf when the tree is non-empty.
    focus: NodeId = none,

    pub fn initLeaf(terminal: TerminalRef) Tree {
        var tree: Tree = .{};
        const id = tree.allocNode(.{ .kind = .leaf, .parent = none, .terminal = terminal }) catch unreachable;
        tree.root = id;
        tree.focus = id;
        return tree;
    }

    pub fn isEmpty(tree: *const Tree) bool {
        return tree.root == none;
    }

    /// Claim a slot and write it in one step. Reserving without writing would
    /// leave the slot `.free`, so a second allocation before the first was
    /// filled in would hand back the same index.
    fn allocNode(tree: *Tree, value: Node) Error!NodeId {
        for (&tree.nodes, 0..) |*slot, index| {
            if (slot.kind == .free) {
                slot.* = value;
                return @intCast(index);
            }
        }
        return error.Full;
    }

    fn freeNode(tree: *Tree, id: NodeId) void {
        tree.nodes[id] = .{};
    }

    pub fn node(tree: *const Tree, id: NodeId) *const Node {
        return &tree.nodes[id];
    }

    /// The number of live leaves.
    pub fn paneCount(tree: *const Tree) usize {
        var total: usize = 0;
        for (tree.nodes) |candidate| {
            if (candidate.kind == .leaf) total += 1;
        }
        return total;
    }

    pub fn isSplit(tree: *const Tree) bool {
        return tree.root != none and tree.nodes[tree.root].kind == .branch;
    }

    pub fn find(tree: *const Tree, terminal: TerminalRef) ?NodeId {
        for (tree.nodes, 0..) |candidate, index| {
            if (candidate.kind != .leaf) continue;
            const held = candidate.terminal orelse continue;
            if (held.eql(terminal)) return @intCast(index);
        }
        return null;
    }

    pub fn focusedTerminal(tree: *const Tree) ?TerminalRef {
        if (tree.focus == none) return null;
        const focused = tree.nodes[tree.focus];
        if (focused.kind != .leaf) return null;
        return focused.terminal;
    }

    pub fn focusTerminal(tree: *Tree, terminal: TerminalRef) bool {
        const id = tree.find(terminal) orelse return false;
        tree.focus = id;
        return true;
    }

    /// Split `target` (a leaf), putting `terminal` in the NEW pane. The new
    /// pane takes the second position — right of, or below, the original —
    /// which is what "split right" and "split down" mean. Focus moves to it,
    /// matching Ghostty.
    pub fn split(tree: *Tree, target: NodeId, orientation: Orientation, terminal: TerminalRef) Error!NodeId {
        if (target >= max_nodes or tree.nodes[target].kind != .leaf) return error.NotALeaf;
        const parent = tree.nodes[target].parent;
        // Allocate both before relinking: a partial split would corrupt the
        // tree, and `Full` is reachable at the ceiling.
        const leaf_id = try tree.allocNode(.{ .kind = .leaf, .parent = none, .terminal = terminal });
        const branch_id = tree.allocNode(.{
            .kind = .branch,
            .parent = parent,
            .orientation = orientation,
            .fraction = 0.5,
            .first = target,
            .second = leaf_id,
        }) catch |err| {
            tree.freeNode(leaf_id);
            return err;
        };

        tree.nodes[leaf_id].parent = branch_id;
        tree.nodes[target].parent = branch_id;

        if (parent == none) {
            tree.root = branch_id;
        } else if (tree.nodes[parent].first == target) {
            tree.nodes[parent].first = branch_id;
        } else {
            tree.nodes[parent].second = branch_id;
        }
        tree.focus = leaf_id;
        return leaf_id;
    }

    /// Remove a leaf. Its sibling is promoted into the parent's place, so
    /// closing a pane never leaves a hole. Returns the terminal that was in
    /// the removed pane. Focus lands on the promoted subtree's first leaf
    /// when the closed pane held it.
    pub fn closePane(tree: *Tree, target: NodeId) Error!TerminalRef {
        if (target >= max_nodes or tree.nodes[target].kind != .leaf) return error.NotALeaf;
        const removed = tree.nodes[target].terminal orelse return error.NotALeaf;
        const parent = tree.nodes[target].parent;
        const had_focus = tree.focus == target;

        if (parent == none) {
            tree.freeNode(target);
            tree.root = none;
            tree.focus = none;
            return removed;
        }

        const branch = tree.nodes[parent];
        const sibling = if (branch.first == target) branch.second else branch.first;
        const grandparent = branch.parent;

        tree.nodes[sibling].parent = grandparent;
        if (grandparent == none) {
            tree.root = sibling;
        } else if (tree.nodes[grandparent].first == parent) {
            tree.nodes[grandparent].first = sibling;
        } else {
            tree.nodes[grandparent].second = sibling;
        }

        tree.freeNode(target);
        tree.freeNode(parent);

        if (had_focus) tree.focus = tree.firstLeaf(sibling);
        return removed;
    }

    /// Close by terminal identity, for the paths that know a ref and not a node.
    pub fn closeTerminal(tree: *Tree, terminal: TerminalRef) ?TerminalRef {
        const id = tree.find(terminal) orelse return null;
        return tree.closePane(id) catch null;
    }

    pub fn firstLeaf(tree: *const Tree, from: NodeId) NodeId {
        var current = from;
        while (current != none and tree.nodes[current].kind == .branch) {
            current = tree.nodes[current].first;
        }
        return current;
    }

    /// Resolve every pane rect. `divider` is the gap left between siblings;
    /// `min_w`/`min_h` clamp a child so a deep tree in a small window still
    /// yields usable panes. Returns the number written to `out`.
    pub fn resolve(
        tree: *const Tree,
        bounds: RectF,
        divider: f32,
        min_w: f32,
        min_h: f32,
        out: []Pane,
    ) usize {
        if (tree.root == none) return 0;
        var written: usize = 0;
        tree.resolveInto(tree.root, bounds, divider, min_w, min_h, out, &written);
        return written;
    }

    fn resolveInto(
        tree: *const Tree,
        id: NodeId,
        bounds: RectF,
        divider: f32,
        min_w: f32,
        min_h: f32,
        out: []Pane,
        written: *usize,
    ) void {
        const current = tree.nodes[id];
        switch (current.kind) {
            .free => return,
            .leaf => {
                if (written.* >= out.len) return;
                const held = current.terminal orelse return;
                out[written.*] = .{ .node = id, .terminal = held, .rect = bounds };
                written.* += 1;
            },
            .branch => {
                const halves = splitRect(bounds, current.orientation, current.fraction, divider, min_w, min_h);
                tree.resolveInto(current.first, halves[0], divider, min_w, min_h, out, written);
                tree.resolveInto(current.second, halves[1], divider, min_w, min_h, out, written);
            },
        }
    }

    /// Every divider, in the same coordinate space as `resolve`.
    pub fn dividers(
        tree: *const Tree,
        bounds: RectF,
        divider: f32,
        min_w: f32,
        min_h: f32,
        out: []Divider,
    ) usize {
        if (tree.root == none) return 0;
        var written: usize = 0;
        tree.dividersInto(tree.root, bounds, divider, min_w, min_h, out, &written);
        return written;
    }

    fn dividersInto(
        tree: *const Tree,
        id: NodeId,
        bounds: RectF,
        divider: f32,
        min_w: f32,
        min_h: f32,
        out: []Divider,
        written: *usize,
    ) void {
        const current = tree.nodes[id];
        if (current.kind != .branch) return;
        const halves = splitRect(bounds, current.orientation, current.fraction, divider, min_w, min_h);
        if (written.* < out.len) {
            const strip = switch (current.orientation) {
                .horizontal => RectF.init(
                    halves[0].x + halves[0].width,
                    bounds.y,
                    @max(0, halves[1].x - (halves[0].x + halves[0].width)),
                    bounds.height,
                ),
                .vertical => RectF.init(
                    bounds.x,
                    halves[0].y + halves[0].height,
                    bounds.width,
                    @max(0, halves[1].y - (halves[0].y + halves[0].height)),
                ),
            };
            out[written.*] = .{
                .node = id,
                .orientation = current.orientation,
                .rect = strip,
                .bounds = bounds,
            };
            written.* += 1;
        }
        tree.dividersInto(current.first, halves[0], divider, min_w, min_h, out, written);
        tree.dividersInto(current.second, halves[1], divider, min_w, min_h, out, written);
    }

    pub fn setFraction(tree: *Tree, id: NodeId, fraction: f32) void {
        if (id >= max_nodes or tree.nodes[id].kind != .branch) return;
        if (!std.math.isFinite(fraction)) return;
        tree.nodes[id].fraction = std.math.clamp(fraction, min_fraction, max_fraction);
    }

    /// Geometric directional focus, the way Ghostty moves between splits:
    /// from the focused pane's edge midpoint, take the nearest pane whose
    /// body lies on the far side in that direction and whose perpendicular
    /// span overlaps. Falls back to no move when nothing qualifies.
    pub fn focusDirection(
        tree: *const Tree,
        bounds: RectF,
        divider: f32,
        min_w: f32,
        min_h: f32,
        direction: Direction,
    ) ?NodeId {
        if (tree.focus == none) return null;
        var panes: [max_panes]Pane = undefined;
        const count = tree.resolve(bounds, divider, min_w, min_h, &panes);
        if (count == 0) return null;

        var origin: ?RectF = null;
        for (panes[0..count]) |pane| {
            if (pane.node == tree.focus) origin = pane.rect;
        }
        const from = origin orelse return null;

        var best: ?NodeId = null;
        var best_distance: f32 = std.math.floatMax(f32);
        for (panes[0..count]) |pane| {
            if (pane.node == tree.focus) continue;
            const candidate = pane.rect;
            const qualifies = switch (direction) {
                .left => candidate.x + candidate.width <= from.x + 1 and overlaps(
                    from.y,
                    from.y + from.height,
                    candidate.y,
                    candidate.y + candidate.height,
                ),
                .right => candidate.x >= from.x + from.width - 1 and overlaps(
                    from.y,
                    from.y + from.height,
                    candidate.y,
                    candidate.y + candidate.height,
                ),
                .up => candidate.y + candidate.height <= from.y + 1 and overlaps(
                    from.x,
                    from.x + from.width,
                    candidate.x,
                    candidate.x + candidate.width,
                ),
                .down => candidate.y >= from.y + from.height - 1 and overlaps(
                    from.x,
                    from.x + from.width,
                    candidate.x,
                    candidate.x + candidate.width,
                ),
            };
            if (!qualifies) continue;
            const distance = switch (direction) {
                .left => from.x - (candidate.x + candidate.width),
                .right => candidate.x - (from.x + from.width),
                .up => from.y - (candidate.y + candidate.height),
                .down => candidate.y - (from.y + from.height),
            };
            if (distance < best_distance) {
                best_distance = distance;
                best = pane.node;
            }
        }
        return best;
    }

    /// Cycle focus through leaves in layout order, for cmd+[ / cmd+].
    pub fn cycleFocus(tree: *const Tree, delta: i32) ?NodeId {
        if (tree.focus == none) return null;
        var order: [max_panes]NodeId = undefined;
        var count: usize = 0;
        tree.collectLeaves(tree.root, &order, &count);
        if (count == 0) return null;
        var current: usize = 0;
        for (order[0..count], 0..) |candidate, index| {
            if (candidate == tree.focus) current = index;
        }
        const signed: i32 = @intCast(count);
        const next = @mod(@as(i32, @intCast(current)) + delta, signed);
        return order[@intCast(next)];
    }

    fn collectLeaves(tree: *const Tree, id: NodeId, out: []NodeId, written: *usize) void {
        if (id == none or written.* >= out.len) return;
        const current = tree.nodes[id];
        switch (current.kind) {
            .free => return,
            .leaf => {
                out[written.*] = id;
                written.* += 1;
            },
            .branch => {
                tree.collectLeaves(current.first, out, written);
                tree.collectLeaves(current.second, out, written);
            },
        }
    }

    /// Every terminal in the tree, in layout order.
    pub fn terminals(tree: *const Tree, out: []TerminalRef) usize {
        var order: [max_panes]NodeId = undefined;
        var count: usize = 0;
        tree.collectLeaves(tree.root, &order, &count);
        var written: usize = 0;
        for (order[0..count]) |id| {
            if (written >= out.len) break;
            out[written] = tree.nodes[id].terminal orelse continue;
            written += 1;
        }
        return written;
    }

    /// The pane whose rect contains `point`, or null over a divider or gap.
    pub fn paneAt(
        tree: *const Tree,
        bounds: RectF,
        divider: f32,
        min_w: f32,
        min_h: f32,
        x: f32,
        y: f32,
    ) ?Pane {
        var panes: [max_panes]Pane = undefined;
        const count = tree.resolve(bounds, divider, min_w, min_h, &panes);
        for (panes[0..count]) |pane| {
            if (x >= pane.rect.x and x < pane.rect.x + pane.rect.width and
                y >= pane.rect.y and y < pane.rect.y + pane.rect.height) return pane;
        }
        return null;
    }
};

fn overlaps(a_start: f32, a_end: f32, b_start: f32, b_end: f32) bool {
    return a_start < b_end and b_start < a_end;
}

/// Divide `bounds` in two. The divider is carved out of the middle, and the
/// requested fraction is clamped so neither child falls under its minimum —
/// the same shape as `canvas.splitEffectiveFraction`, generalized to both
/// orientations.
pub fn splitRect(
    bounds: RectF,
    orientation: Orientation,
    fraction: f32,
    divider: f32,
    min_w: f32,
    min_h: f32,
) [2]RectF {
    return switch (orientation) {
        .horizontal => blk: {
            const available = @max(0, bounds.width - divider);
            const effective = effectiveFraction(fraction, available, min_w);
            const first = available * effective;
            break :blk .{
                RectF.init(bounds.x, bounds.y, first, bounds.height),
                RectF.init(bounds.x + first + divider, bounds.y, @max(0, available - first), bounds.height),
            };
        },
        .vertical => blk: {
            const available = @max(0, bounds.height - divider);
            const effective = effectiveFraction(fraction, available, min_h);
            const first = available * effective;
            break :blk .{
                RectF.init(bounds.x, bounds.y, bounds.width, first),
                RectF.init(bounds.x, bounds.y + first + divider, bounds.width, @max(0, available - first)),
            };
        },
    };
}

/// Clamp so both sides clear `minimum`. When the space cannot hold two
/// minimums, fall back to an even divide rather than starving one side —
/// a 50/50 squeeze reads as "too small", a 95/5 squeeze reads as broken.
fn effectiveFraction(fraction: f32, available: f32, minimum: f32) f32 {
    const requested = if (std.math.isFinite(fraction))
        std.math.clamp(fraction, min_fraction, max_fraction)
    else
        0.5;
    if (available <= 0) return requested;
    if (available < minimum * 2) return 0.5;
    const low = minimum / available;
    const high = 1 - low;
    return std.math.clamp(requested, low, high);
}
