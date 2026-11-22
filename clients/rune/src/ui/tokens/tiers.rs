//! Elevation: four tiers of at most four shadow layers, rings
//! coloured `border.base` when the stack is built, glow the only colour.

use std::hash::{Hash, Hasher};

use phux_design::Roles;
use phux_design::tokens as tok;
use rune::{Color, Shadow};

use super::Paint;

/// Where a surface sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Tier {
    /// Sidebar, tab strip, rows: no shadow.
    Flat,
    /// Terminal cards at rest, banners, the active tab.
    Raised,
    /// Find bar, toasts, the ask card, the focused card's lift.
    Floating,
    /// Menus, popovers, the palette, sheets, the inbox.
    Overlay,
}

/// The shadow stacks for one mode, ready to paint.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tiers {
    pub(crate) raised: Vec<Shadow>,
    pub(crate) floating: Vec<Shadow>,
    pub(crate) overlay: Vec<Shadow>,
    glow: tok::Glow,
}

fn layer(s: &tok::Shadow, ring: Color) -> Shadow {
    let color = match s.kind {
        "ring" => ring,
        _ => {
            let c = s.color.paint();
            Color { a: s.alpha, ..c }
        }
    };
    Shadow {
        x: s.x,
        y: s.y,
        blur: s.blur,
        spread: s.spread,
        color,
        // The lit edge is the top highlight inside the box.
        inset: s.kind == "lit",
    }
}

impl Tiers {
    pub(crate) fn new(roles: &Roles) -> Self {
        let e = if roles.mode.is_dark() {
            tok::ELEVATION.dark
        } else {
            tok::ELEVATION.light
        };
        let ring = roles.border.base.paint();
        let stack = |t: &[tok::Shadow]| t.iter().map(|s| layer(s, ring)).collect();
        Self {
            raised: stack(e.raised),
            floating: stack(e.floating),
            overlay: stack(e.overlay),
            glow: e.glow,
        }
    }

    /// No shadows at all: forced colours drop them.
    pub(crate) fn none(roles: &Roles) -> Self {
        Self {
            raised: Vec::new(),
            floating: Vec::new(),
            overlay: Vec::new(),
            ..Self::new(roles)
        }
    }

    pub(crate) fn of(&self, tier: Tier) -> &[Shadow] {
        match tier {
            Tier::Flat => &[],
            Tier::Raised => &self.raised,
            Tier::Floating => &self.floating,
            Tier::Overlay => &self.overlay,
        }
    }

    /// `tier` with a glow in `color` replacing its second drop shadow, so
    /// the stack stays within the four layers Rune springs.
    pub(crate) fn glowing(&self, tier: Tier, color: Color) -> Vec<Shadow> {
        let g = self.glow;
        let glow = Shadow {
            x: g.x,
            y: g.y,
            blur: g.blur,
            spread: g.spread,
            color: Color {
                a: g.alpha,
                ..color
            },
            inset: false,
        };
        let mut layers = self.of(tier).to_vec();
        let drops: Vec<usize> = (layers.iter().enumerate())
            .filter(|(_, s)| !s.inset && s.blur > 0.0)
            .map(|(i, _)| i)
            .collect();
        match drops.get(1) {
            Some(&second) => layers[second] = glow,
            None => layers.push(glow),
        }
        layers.truncate(4);
        layers
    }

    /// The first drop's colour: what a legacy `shade` was.
    pub(crate) fn drop_color(&self, tier: Tier) -> Color {
        (self.of(tier).iter())
            .find(|s| !s.inset && s.blur > 0.0)
            .map_or(Color::TRANSPARENT, |s| s.color)
    }

    pub(crate) fn hash_into(&self, h: &mut impl Hasher) {
        for stack in [&self.raised, &self.floating, &self.overlay] {
            for s in stack {
                [s.x, s.y, s.blur, s.spread].map(f32::to_bits).hash(h);
                (s.color, s.inset).hash(h);
            }
            stack.len().hash(h);
        }
        [self.glow.blur, self.glow.alpha].map(f32::to_bits).hash(h);
    }
}
