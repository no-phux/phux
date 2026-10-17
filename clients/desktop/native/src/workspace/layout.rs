//! Tabs own a binary split tree of placements. Every operation here is pure.

use phux_client_runtime::ViewId;
use phux_protocol::ResourceId;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Smallest split ratio. The other side keeps the same margin.
pub const MIN_RATIO: f32 = 0.08;

/// Horizontal or vertical split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Left and right.
    Row,
    /// Above and below.
    Column,
}

impl Axis {
    /// Persisted spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Row => "row",
            Self::Column => "column",
        }
    }

    /// Parse a persisted spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "row" => Some(Self::Row),
            "column" => Some(Self::Column),
            _ => None,
        }
    }
}

/// A direction from a leaf toward its neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Toward smaller x.
    Left,
    /// Toward larger x.
    Right,
    /// Toward smaller y.
    Up,
    /// Toward larger y.
    Down,
}

/// One local presentation of a daemon terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Stable leaf id.
    pub id: String,
    /// Runtime id when the spelling parses. `None` never reaches `Client`.
    pub terminal_id: Option<ResourceId>,
    /// Persistence and display spelling, including an unparsed saved id.
    pub terminal_label: String,
    /// Runtime view. Absent until the terminal has a renderable replica.
    pub view_id: Option<ViewId>,
}

/// A leaf or a binary split.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    /// One placement.
    Leaf {
        /// The placement.
        placement: Placement,
    },
    /// Two children divided by `ratio`.
    Split {
        /// Split id, local to this process.
        id: String,
        /// Which axis `ratio` divides.
        axis: Axis,
        /// Fraction of the parent given to `first`, in `MIN_RATIO..=1-MIN_RATIO`.
        ratio: f32,
        /// First child.
        first: Box<LayoutNode>,
        /// Second child.
        second: Box<LayoutNode>,
    },
}

/// One desktop tab.
#[derive(Debug, Clone, PartialEq)]
pub struct DeskTab {
    /// Tab id.
    pub id: String,
    /// User title. Absent follows the focused terminal's title.
    pub title: Option<String>,
    /// Split tree.
    pub root: LayoutNode,
    /// Focused leaf.
    pub focused_id: String,
    /// Zoomed leaf. Absent shows the whole tree.
    pub zoomed_id: Option<String>,
}

/// A rectangle in the same units the caller passes to [`layout_rects`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// The unit square.
    pub const UNIT: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
}

/// A process-local id. Tests do not depend on the spelling, only uniqueness.
#[must_use]
pub fn new_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{millis:x}-{count}")
}

/// A leaf node.
#[must_use]
pub fn leaf(placement: Placement) -> LayoutNode {
    LayoutNode::Leaf { placement }
}

/// Every placement, first child before second.
#[must_use]
pub fn placements(node: &LayoutNode) -> Vec<&Placement> {
    match node {
        LayoutNode::Leaf { placement } => vec![placement],
        LayoutNode::Split { first, second, .. } => {
            let mut found = placements(first);
            found.extend(placements(second));
            found
        }
    }
}

/// Every placement, mutably, in the same order as [`placements`].
pub fn placements_mut(node: &mut LayoutNode) -> Vec<&mut Placement> {
    match node {
        LayoutNode::Leaf { placement } => vec![placement],
        LayoutNode::Split { first, second, .. } => {
            let mut found = placements_mut(first);
            found.extend(placements_mut(second));
            found
        }
    }
}

/// The placement with `id`.
#[must_use]
pub fn find_placement<'a>(node: &'a LayoutNode, id: &str) -> Option<&'a Placement> {
    placements(node)
        .into_iter()
        .find(|placement| placement.id == id)
}

/// The focused placement, or the first leaf.
#[must_use]
pub fn focused_placement(tab: &DeskTab) -> Option<&Placement> {
    find_placement(&tab.root, &tab.focused_id).or_else(|| placements(&tab.root).into_iter().next())
}

/// Put `added` beside `target_id`. `before` puts it left or above.
pub fn split_at(
    node: &mut LayoutNode,
    target_id: &str,
    added: Placement,
    axis: Axis,
    before: bool,
) {
    let Some(target) = leaf_mut(node, target_id) else {
        return;
    };
    let added = leaf(added);
    let kept = std::mem::replace(target, added.clone());
    let (first, second) = if before { (added, kept) } else { (kept, added) };
    *target = LayoutNode::Split {
        id: new_id("split"),
        axis,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    };
}

fn leaf_mut<'a>(node: &'a mut LayoutNode, id: &str) -> Option<&'a mut LayoutNode> {
    if matches!(node, LayoutNode::Leaf { placement } if placement.id == id) {
        return Some(node);
    }
    match node {
        LayoutNode::Leaf { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            leaf_mut(first, id).or_else(|| leaf_mut(second, id))
        }
    }
}

/// Remove a leaf. `None` when the tree is empty.
#[must_use]
pub fn remove_leaf(node: LayoutNode, id: &str) -> Option<LayoutNode> {
    match node {
        LayoutNode::Leaf { placement } => {
            (placement.id != id).then_some(LayoutNode::Leaf { placement })
        }
        LayoutNode::Split {
            id: split_id,
            axis,
            ratio,
            first,
            second,
        } => match (remove_leaf(*first, id), remove_leaf(*second, id)) {
            (None, other) | (other, None) => other,
            (Some(first), Some(second)) => Some(LayoutNode::Split {
                id: split_id,
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
        },
    }
}

fn splits_mut(node: &mut LayoutNode, visit: &mut impl FnMut(&str, &mut f32)) {
    if let LayoutNode::Split {
        id,
        ratio,
        first,
        second,
        ..
    } = node
    {
        visit(id, ratio);
        splits_mut(first, visit);
        splits_mut(second, visit);
    }
}

/// Set one split's ratio, clamped.
pub fn set_ratio(node: &mut LayoutNode, split_id: &str, ratio: f32) {
    splits_mut(node, &mut |id, current| {
        if id == split_id {
            *current = clamp_ratio(ratio);
        }
    });
}

/// Reset every split to an even division.
pub fn equalize(node: &mut LayoutNode) {
    splits_mut(node, &mut |_, ratio| *ratio = 0.5);
}

/// Clamp a ratio. Non-finite values become an even split.
#[must_use]
pub fn clamp_ratio(ratio: f32) -> f32 {
    if !ratio.is_finite() {
        return 0.5;
    }
    ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO)
}

/// Leaf rectangles inside `bounds`.
#[must_use]
pub fn layout_rects(node: &LayoutNode, bounds: Rect) -> HashMap<String, Rect> {
    let mut rects = HashMap::new();
    visit(node, bounds, &mut rects);
    rects
}

fn visit(node: &LayoutNode, rect: Rect, rects: &mut HashMap<String, Rect>) {
    match node {
        LayoutNode::Leaf { placement } => {
            rects.insert(placement.id.clone(), rect);
        }
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let (left, right) = divide(rect, *axis, *ratio);
            visit(first, left, rects);
            visit(second, right, rects);
        }
    }
}

fn divide(rect: Rect, axis: Axis, ratio: f32) -> (Rect, Rect) {
    match axis {
        Axis::Row => {
            let width = rect.width * ratio;
            (
                Rect { width, ..rect },
                Rect {
                    x: rect.x + width,
                    width: rect.width - width,
                    ..rect
                },
            )
        }
        Axis::Column => {
            let height = rect.height * ratio;
            (
                Rect { height, ..rect },
                Rect {
                    y: rect.y + height,
                    height: rect.height - height,
                    ..rect
                },
            )
        }
    }
}

/// Nearest leaf in `direction`. Ties go to the most overlap, then to the
/// earlier leaf in reading order.
#[must_use]
pub fn neighbor(node: &LayoutNode, from_id: &str, direction: Direction) -> Option<String> {
    let rects = layout_rects(node, Rect::UNIT);
    let from = rects.get(from_id)?;
    let mut best: Option<(String, f32, f32)> = None;
    for placement in placements(node) {
        let id = &placement.id;
        let Some(rect) = rects.get(id) else { continue };
        if id == from_id {
            continue;
        }
        let distance = gap(from, rect, direction);
        let overlap = cross_overlap(from, rect, direction);
        if distance < -1e-6 || overlap <= 1e-6 {
            continue;
        }
        let replace = best
            .as_ref()
            .is_none_or(|(_, best_distance, best_overlap)| {
                distance < best_distance - 1e-6
                    || ((distance - best_distance).abs() < 1e-6 && overlap > *best_overlap)
            });
        if replace {
            best = Some((id.clone(), distance, overlap));
        }
    }
    best.map(|(id, _, _)| id)
}

fn gap(from: &Rect, to: &Rect, direction: Direction) -> f32 {
    match direction {
        Direction::Left => from.x - (to.x + to.width),
        Direction::Right => to.x - (from.x + from.width),
        Direction::Up => from.y - (to.y + to.height),
        Direction::Down => to.y - (from.y + from.height),
    }
}

fn cross_overlap(from: &Rect, to: &Rect, direction: Direction) -> f32 {
    match direction {
        Direction::Left | Direction::Right => {
            (from.y + from.height).min(to.y + to.height) - from.y.max(to.y)
        }
        Direction::Up | Direction::Down => {
            (from.x + from.width).min(to.x + to.width) - from.x.max(to.x)
        }
    }
}

/// Next or previous leaf in reading order, wrapping.
#[must_use]
pub fn cycle(node: &LayoutNode, from_id: &str, step: i8) -> Option<String> {
    let order = placements(node);
    let index = order
        .iter()
        .position(|placement| placement.id == from_id)
        .unwrap_or(0);
    Some(order[step_index(index, step, order.len())?].id.clone())
}

/// `index` moved by `step`, wrapping within `len`. `None` when `len` is zero.
#[must_use]
pub fn step_index(index: usize, step: i8, len: usize) -> Option<usize> {
    (len > 0).then(|| (index as i64 + i64::from(step)).rem_euclid(len as i64) as usize)
}

/// The leaf to focus once `removed` leaves go.
#[must_use]
pub fn focus_after_removal(
    order: &[String],
    focused_id: &str,
    removed: &HashSet<String>,
) -> String {
    if !removed.contains(focused_id) {
        return focused_id.to_owned();
    }
    let index = order.iter().position(|id| id == focused_id).unwrap_or(0);
    order[..index]
        .iter()
        .rev()
        .find(|id| !removed.contains(id.as_str()))
        .or_else(|| {
            order[index + 1..]
                .iter()
                .find(|id| !removed.contains(id.as_str()))
        })
        .cloned()
        .unwrap_or_default()
}

/// Drop the `removed` leaves, handing focus to a neighbour. `None` when the tab empties.
#[must_use]
pub fn remove_leaves(tab: DeskTab, removed: &HashSet<String>) -> Option<DeskTab> {
    let order: Vec<String> = placements(&tab.root)
        .into_iter()
        .map(|placement| placement.id.clone())
        .collect();
    let root = removed
        .iter()
        .try_fold(tab.root, |node, id| remove_leaf(node, id))?;
    let mut tab = DeskTab {
        root,
        focused_id: focus_after_removal(&order, &tab.focused_id, removed),
        ..tab
    };
    refocus(&mut tab);
    Some(tab)
}

/// Drop every leaf that shows `terminal_label`. Empty tabs disappear.
#[must_use]
pub fn without_terminal(tabs: Vec<DeskTab>, terminal_label: &str) -> Vec<DeskTab> {
    tabs.into_iter()
        .filter_map(|tab| {
            let doomed: HashSet<String> = placements(&tab.root)
                .into_iter()
                .filter(|placement| placement.terminal_label == terminal_label)
                .map(|placement| placement.id.clone())
                .collect();
            if doomed.is_empty() {
                Some(tab)
            } else {
                remove_leaves(tab, &doomed)
            }
        })
        .collect()
}

/// Keep focus and zoom on leaves that still exist.
pub fn refocus(tab: &mut DeskTab) {
    let ids: Vec<&str> = placements(&tab.root)
        .into_iter()
        .map(|placement| placement.id.as_str())
        .collect();
    if !ids.contains(&tab.focused_id.as_str()) {
        tab.focused_id = ids.first().copied().unwrap_or_default().to_owned();
    }
    if tab
        .zoomed_id
        .as_deref()
        .is_some_and(|id| !ids.contains(&id))
    {
        tab.zoomed_id = None;
    }
}

/// Move `items[from]` to `to`, clamped.
pub fn reorder<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from < items.len() {
        let moved = items.remove(from);
        items.insert(to.min(items.len()), moved);
    }
}

/// Move the nearest divider along `direction` by `delta`, like Ghostty's resize_split.
pub fn nudge(node: &mut LayoutNode, leaf_id: &str, direction: Direction, delta: f32) {
    let (axis, sign) = match direction {
        Direction::Left => (Axis::Row, -1.0),
        Direction::Right => (Axis::Row, 1.0),
        Direction::Up => (Axis::Column, -1.0),
        Direction::Down => (Axis::Column, 1.0),
    };
    if let Some((_, ratio, id)) = path_to(node, leaf_id)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .find(|(split_axis, _, _)| *split_axis == axis)
    {
        set_ratio(node, &id, ratio + sign * delta);
    }
}

fn path_to(node: &LayoutNode, leaf_id: &str) -> Option<Vec<(Axis, f32, String)>> {
    match node {
        LayoutNode::Leaf { placement } => (placement.id == leaf_id).then(Vec::new),
        LayoutNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let below = path_to(first, leaf_id).or_else(|| path_to(second, leaf_id))?;
            let mut path = vec![(*axis, *ratio, id.clone())];
            path.extend(below);
            Some(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(id: &str, terminal: &str) -> Placement {
        Placement {
            id: id.to_owned(),
            terminal_id: None,
            terminal_label: terminal.to_owned(),
            view_id: None,
        }
    }

    fn split(mut node: LayoutNode, target: &str, added: Placement, axis: Axis) -> LayoutNode {
        split_at(&mut node, target, added, axis, false);
        node
    }

    fn tab(id: &str, root: LayoutNode, focused: &str) -> DeskTab {
        DeskTab {
            id: id.into(),
            title: None,
            root,
            focused_id: focused.into(),
            zoomed_id: None,
        }
    }

    fn ids(node: &LayoutNode) -> Vec<String> {
        placements(node)
            .into_iter()
            .map(|placement| placement.id.clone())
            .collect()
    }

    fn grid() -> LayoutNode {
        let root = split(leaf(place("a", "t-a")), "a", place("b", "t-b"), Axis::Row);
        split(root, "b", place("c", "t-c"), Axis::Column)
    }

    #[test]
    fn splitting_keeps_reading_order() {
        let root = split(leaf(place("a", "t-a")), "a", place("b", "t-b"), Axis::Row);
        assert_eq!(ids(&root), ["a", "b"]);
    }

    #[test]
    fn split_before_puts_the_new_leaf_above() {
        let mut root = grid();
        split_at(&mut root, "c", place("d", "t-d"), Axis::Column, true);
        assert_eq!(ids(&root), ["a", "b", "d", "c"]);
        let rects = layout_rects(&root, Rect::UNIT);
        assert!(rects["d"].y < rects["c"].y);
    }

    #[test]
    fn removing_a_leaf_promotes_its_sibling() {
        let without = remove_leaf(grid(), "b").expect("sibling");
        assert_eq!(ids(&without), ["a", "c"]);
        assert!(
            matches!(without, LayoutNode::Split { second, .. } if matches!(second.as_ref(), LayoutNode::Leaf { .. }))
        );
        assert!(remove_leaf(leaf(place("x", "t")), "x").is_none());
    }

    #[test]
    fn ratios_clamp_and_equalize() {
        let mut root = grid();
        let split_id = match &root {
            LayoutNode::Split { id, .. } => id.clone(),
            LayoutNode::Leaf { .. } => String::new(),
        };
        set_ratio(&mut root, &split_id, 5.0);
        assert!(matches!(root, LayoutNode::Split { ratio, .. } if ratio == clamp_ratio(5.0)));
        equalize(&mut root);
        assert!(matches!(root, LayoutNode::Split { ratio, .. } if ratio == 0.5));
        assert_eq!(clamp_ratio(f32::NAN), 0.5);
    }

    #[test]
    fn rects_tile_the_bounds() {
        let rect = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        let rects = layout_rects(&grid(), rect(0.0, 0.0, 100.0, 60.0));
        assert_eq!(rects["a"], rect(0.0, 0.0, 50.0, 60.0));
        assert_eq!(rects["b"], rect(50.0, 0.0, 50.0, 30.0));
        assert_eq!(rects["c"], rect(50.0, 30.0, 50.0, 30.0));
    }

    #[test]
    fn directional_focus_follows_geometry() {
        let root = grid();
        assert_eq!(neighbor(&root, "a", Direction::Right).as_deref(), Some("b"));
        assert_eq!(neighbor(&root, "c", Direction::Left).as_deref(), Some("a"));
        assert_eq!(neighbor(&root, "b", Direction::Down).as_deref(), Some("c"));
        assert_eq!(neighbor(&root, "c", Direction::Up).as_deref(), Some("b"));
        assert_eq!(neighbor(&root, "a", Direction::Left), None);
        assert_eq!(neighbor(&root, "a", Direction::Up), None);
    }

    #[test]
    fn cycling_wraps() {
        let root = grid();
        assert_eq!(cycle(&root, "c", 1).as_deref(), Some("a"));
        assert_eq!(cycle(&root, "a", -1).as_deref(), Some("c"));
    }

    #[test]
    fn closed_terminal_drops_empty_tabs_and_hands_focus_to_the_neighbour() {
        let shared = split(leaf(place("a", "t1")), "a", place("b", "t1"), Axis::Row);
        let other = split(leaf(place("c", "t2")), "c", place("d", "t1"), Axis::Column);
        let next = without_terminal(
            vec![tab("tab", shared, "a"), tab("other", other, "d")],
            "t1",
        );
        assert_eq!(
            next.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(),
            ["other"]
        );
        assert_eq!(ids(&next[0].root), ["c"]);
        assert_eq!(next[0].focused_id, "c");
        let mut exited = without_terminal(vec![tab("tab", grid(), "c")], "t-c");
        let only = exited.pop().expect("one tab");
        assert!(exited.is_empty());
        assert_eq!(ids(&only.root), ["a", "b"]);
        assert_eq!(only.focused_id, "b");
        assert_eq!(
            focus_after_removal(
                &["a".into(), "b".into(), "c".into()],
                "a",
                &HashSet::from(["a".into()])
            ),
            "b"
        );
    }

    #[test]
    fn refocus_drops_a_stale_zoom_and_reorder_clamps() {
        let mut fixed = tab("tab", leaf(place("a", "t")), "gone");
        fixed.zoomed_id = Some("gone".into());
        refocus(&mut fixed);
        assert_eq!(fixed.focused_id, "a");
        assert!(fixed.zoomed_id.is_none());
        let mut items = vec!["a", "b", "c"];
        reorder(&mut items, 0, 2);
        assert_eq!(items, ["b", "c", "a"]);
        reorder(&mut items, 2, 0);
        assert_eq!(items, ["a", "b", "c"]);
        reorder(&mut items, 9, 0);
        assert_eq!(items, ["a", "b", "c"]);
    }

    #[test]
    fn nudge_moves_only_the_matching_axis() {
        let nudged = |direction| {
            let mut root = grid();
            nudge(&mut root, "c", direction, 0.1);
            root
        };
        let right = nudged(Direction::Right);
        assert!(matches!(right, LayoutNode::Split { ratio, .. } if (ratio - 0.6).abs() < 1e-5));
        let LayoutNode::Split { second, .. } = nudged(Direction::Up) else {
            panic!("split")
        };
        let LayoutNode::Split { ratio, .. } = second.as_ref() else {
            panic!("column")
        };
        assert!((ratio - 0.4).abs() < 1e-5);
        let mut root = grid();
        let before = root.clone();
        nudge(&mut root, "a", Direction::Up, 0.1);
        assert_eq!(root, before);
    }
}
