//! A continuous, proportional map of one group. Unlike the core's cell
//! rasterizer this has no dividers, minimum-cell freezing, or integer rounding.
//! Camera, minimap, hit testing and navigation all use these same rectangles.

use phux_client_core::layout::{Direction, LayoutNode, SplitDir};
use phux_protocol::ids::ResourceId;
use rune::{Drag, Key, KeyEvent, Phase};

use super::super::Desk;
use super::super::input::Modal;

const SLOP: f32 = 8.0;
const COMMIT: f32 = 28.0;
/// Rune stores f32 edges and snaps them in device subpixels. A finite but
/// enormous world can lose a pane's entire width when subtracting its edges.
const MAX_CANVAS: f32 = 16_384.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Zero-area leaves remain in the picker, but must not divide the camera
    /// by zero or win an ambiguous hit on a split boundary.
    pub fn has_area(self) -> bool {
        self.w > 0.0 && self.h > 0.0
    }

    fn contains(self, x: f32, y: f32) -> bool {
        self.has_area() && x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

pub fn rects(tree: &LayoutNode) -> Vec<(ResourceId, Rect)> {
    let mut out = Vec::new();
    walk(
        tree,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        },
        &mut out,
    );
    out
}

fn walk(tree: &LayoutNode, bounds: Rect, out: &mut Vec<(ResourceId, Rect)>) {
    match tree {
        LayoutNode::Leaf(id) => out.push((id.clone(), bounds)),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            let ratio = if ratio.is_finite() {
                ratio.clamp(0.0, 1.0)
            } else {
                0.5
            };
            let (a, b) = divide(bounds, *dir, ratio);
            walk(left, a, out);
            walk(right, b, out);
        }
    }
}

fn divide(r: Rect, dir: SplitDir, ratio: f32) -> (Rect, Rect) {
    match dir {
        SplitDir::Horizontal => {
            let w = r.w * ratio;
            (
                Rect { w, ..r },
                Rect {
                    x: r.x + w,
                    w: r.w - w,
                    ..r
                },
            )
        }
        SplitDir::Vertical => {
            let h = r.h * ratio;
            (
                Rect { h, ..r },
                Rect {
                    y: r.y + h,
                    h: r.h - h,
                    ..r
                },
            )
        }
    }
}

pub fn hit(tree: &LayoutNode, x: f32, y: f32) -> Option<ResourceId> {
    rects(tree)
        .into_iter()
        .find(|(_, r)| r.contains(x, y))
        .map(|(id, _)| id)
}

/// Both camera and pane placement must take the same fallback. Otherwise a
/// stage-sized fallback camera would still draw the tiny literal focus rect.
pub fn camera_fallback(focus: Rect, [w, h]: [f32; 2]) -> bool {
    if !focus.has_area() {
        return true;
    }
    ![w / focus.w, h / focus.h]
        .iter()
        .all(|span| span.is_finite() && *span <= MAX_CANVAS)
}

/// Canvas `[width, height, left, top]` at the destination. Collapsed or
/// precision-unsafe focus gets a stage-sized, focused-only presentation.
/// The literal map and retained group remain unchanged.
pub fn camera(focus: Rect, [w, h]: [f32; 2]) -> [f32; 4] {
    if camera_fallback(focus, [w, h]) {
        return [w, h, 0.0, 0.0];
    }
    let (cw, ch) = (w / focus.w, h / focus.h);
    [cw, ch, -focus.x * cw, -focus.y * ch]
}

/// Coordinates along the requested direction and across it, plus extents.
fn axes(r: Rect, dir: Direction) -> [f32; 4] {
    let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    match dir {
        Direction::Left => [-x, y, r.w, r.h],
        Direction::Right => [x, y, r.w, r.h],
        Direction::Up => [-y, x, r.h, r.w],
        Direction::Down => [y, x, r.h, r.w],
    }
}

fn neighbor_score(from: Rect, to: Rect, dir: Direction) -> Option<[f32; 3]> {
    let [a, b, w, h] = axes(from, dir);
    let [c, d, v, z] = axes(to, dir);
    if c <= a {
        return None;
    }
    let across = (d - b).abs();
    Some([
        (across - (h + z) / 2.0).max(0.0),
        (c - a - (w + v) / 2.0).max(0.0),
        across,
    ])
}

pub fn toward(tree: &LayoutNode, focus: &ResourceId, dir: Direction) -> Option<ResourceId> {
    let all = rects(tree);
    let from = all.iter().find(|(id, _)| id == focus)?.1;
    all.iter()
        .filter(|(id, _)| id != focus)
        .filter_map(|(id, r)| Some((id, neighbor_score(from, *r, dir)?)))
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _)| id.clone())
}

fn direction(dx: f32, dy: f32) -> Direction {
    if dx.abs() > dy.abs() {
        if dx < 0.0 {
            Direction::Right
        } else {
            Direction::Left
        }
    } else if dy < 0.0 {
        Direction::Down
    } else {
        Direction::Up
    }
}

impl Desk {
    /// Find and confirmation live inside the pane. They block navigation,
    /// but must not inert their own field along with the terminal canvas.
    pub fn phone_content_blocked(&self) -> bool {
        self.modal().is_some_and(|modal| modal.takes_pointer())
    }

    pub fn phone_picker_visible(&self) -> bool {
        self.modal() == Some(Modal::PhonePicker)
    }

    pub fn phone_stage_laid_out(&mut self, size: [f32; 2]) {
        if let Some(phone) = self.phone.as_mut() {
            phone.stage = size;
        }
    }

    pub fn phone_picker(&mut self) {
        if self.modal().is_some() {
            return;
        }
        if let Some(phone) = self.phone.as_mut() {
            phone.picker = !phone.picker;
        }
    }

    pub fn close_phone_picker(&mut self) {
        if let Some(phone) = self.phone.as_mut() {
            phone.picker = false;
        }
    }

    /// The Rune modal owns physical keys and paste; drawn keys are routed
    /// here synchronously as well, so no key can outlive this picker.
    pub fn phone_picker_key(&mut self, event: &KeyEvent) -> bool {
        match event.key {
            Key::Escape | Key::Enter => self.close_phone_picker(),
            Key::Left => self.phone_toward(Direction::Left),
            Key::Right => self.phone_toward(Direction::Right),
            Key::Up => self.phone_toward(Direction::Up),
            Key::Down => self.phone_toward(Direction::Down),
            Key::Tab => return false,
            _ => {}
        }
        true
    }

    /// Accessible explicit actions use the very same neighbors as swipes.
    pub fn phone_toward(&mut self, dir: Direction) {
        if matches!(self.modal(), None | Some(Modal::PhonePicker)) {
            self.focus_toward(dir);
        }
    }

    pub fn phone_pick(&mut self, id: &ResourceId) {
        if !matches!(self.modal(), None | Some(Modal::PhonePicker)) {
            return;
        }
        self.focus_pane(id);
        if let Some(phone) = self.phone.as_mut() {
            phone.picker = false;
        }
    }

    /// Only the dedicated map/handle owns this gesture. Content keeps its
    /// terminal mouse/selection behavior. One release commits one neighbor.
    pub fn phone_navigate(&mut self, drag: &Drag, map: bool) {
        if self.modal().is_some() {
            if let Some(phone) = self.phone.as_mut() {
                phone.reset_navigation();
            }
            return;
        }
        let Some(focus) = self.shown.as_ref().map(|s| s.terminal.clone()) else {
            return;
        };
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        if drag.phase == Phase::Down {
            phone.navigation = Some((focus, false));
            return;
        }
        let Some((started, moved)) = phone.navigation.as_mut() else {
            return;
        };
        if *started != focus {
            phone.reset_navigation();
            return;
        }
        *moved |= drag.dx.hypot(drag.dy) > SLOP;
        let moved = *moved;
        if drag.phase == Phase::Move {
            return self.phone_navigation_preview(drag);
        }
        phone.reset_navigation();
        if drag.cancelled() {
            return;
        }
        self.phone_navigation_end(drag, map, moved);
    }

    fn phone_navigation_preview(&mut self, drag: &Drag) {
        let dir = direction(drag.dx, drag.dy);
        let next = self
            .navigation_tree()
            .zip(self.shown.as_ref())
            .and_then(|(tree, shown)| toward(tree, &shown.terminal, dir));
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        let resistance = if next.is_some() { 1.0 } else { 0.2 };
        let [w, h] = phone.stage;
        phone.travel = [
            drag.dx.clamp(-w * 0.65, w * 0.65) * resistance,
            drag.dy.clamp(-h * 0.65, h * 0.65) * resistance,
        ];
    }

    fn phone_navigation_end(&mut self, drag: &Drag, map: bool, moved: bool) {
        if drag.dx.hypot(drag.dy) >= COMMIT {
            self.focus_toward(direction(drag.dx, drag.dy));
        } else if map && !moved {
            let tree = self.phone.as_ref().and_then(|p| p.tree.as_ref());
            let next = tree.and_then(|t| hit(t, drag.x / drag.w, drag.y / drag.h));
            if let Some(next) = next {
                self.focus_pane(&next);
            }
        }
    }
}

#[cfg(test)]
mod tests;
