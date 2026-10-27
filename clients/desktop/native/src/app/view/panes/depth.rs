//! A pane's depth: how far back an agent-driven card sits.

use crate::app::skin::Look;

/// One veil from two: `a` then `b` laid over the same content.
pub(super) fn veils(a: f32, b: f32) -> f32 {
    1. - (1. - a.clamp(0., 1.)) * (1. - b.clamp(0., 1.))
}

/// How far a pane `depth` of the way back draws its edge in on each axis:
/// the skin's inset, never more than the padding that absorbs it, so the
/// terminal grid inside keeps its exact bounds.
pub(super) fn depth_inset(look: &Look, depth: f32, padding_x: f32, padding_y: f32) -> (f32, f32) {
    let inset = (look.skin.material.depth.inset * depth.clamp(0., 1.)).max(0.);
    (inset.min(padding_x.max(0.)), inset.min(padding_y.max(0.)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::default_display;
    use crate::settings::skin::{GLASS, Skin, TERMINAL};
    use crate::settings::theme::{default_theme_id, palette, theme_by_id};

    fn look(skin: &Skin) -> Look {
        Look::of(
            skin,
            &palette(&theme_by_id(default_theme_id(), &[])),
            &default_display(),
        )
    }

    #[test]
    fn the_padding_absorbs_the_inset_so_the_grid_keeps_its_bounds() {
        let glass = look(&GLASS);
        let inset = GLASS.material.depth.inset;
        assert_eq!(depth_inset(&glass, 0., 8., 6.), (0., 0.));
        assert_eq!(depth_inset(&glass, 1., 8., 6.), (inset, inset));
        assert_eq!(depth_inset(&glass, 0.5, 8., 6.), (inset / 2., inset / 2.));
        // Never more than the padding on either axis, so content never moves.
        assert_eq!(depth_inset(&glass, 1., 2., 0.), (2., 0.));
        assert_eq!(depth_inset(&look(&TERMINAL), 1., 8., 6.), (0., 0.));
    }

    #[test]
    fn veils_compose_and_stay_in_range() {
        assert_eq!(veils(0., 0.), 0.);
        assert_eq!(veils(0.5, 0.), 0.5);
        assert!((veils(0.5, 0.5) - 0.75).abs() < 1e-6);
        assert_eq!(veils(2., 0.3), 1.);
    }
}
