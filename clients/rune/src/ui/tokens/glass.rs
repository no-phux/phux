//! Glass: where it draws, its blur and tint, and the one place a
//! view gets a backdrop blur.

use phux_design::Rgba8;
use phux_design::tokens as tok;
use rune::{Color, El};

use super::{Paint, Tokens};

/// The trace name every glass panel carries ([`Tokens::glass_panel`]).
pub(crate) const GLASS_TRACE: &str = "phux-glass";

/// A surface glass may draw on (`material.glass.where`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlassSurface {
    /// The find bar.
    Floating,
    /// The open modal surface: the palette, the inbox.
    Overlay,
    /// The phone's session dock, its one blur.
    PhoneBar,
}

impl GlassSurface {
    const fn key(self) -> &'static str {
        match self {
            Self::Floating => "floating",
            Self::Overlay => "overlay",
            Self::PhoneBar => "phone_bar",
        }
    }

    const fn blur(self) -> f32 {
        let b = tok::MATERIAL.glass.blur;
        match self {
            Self::Floating => b.floating,
            Self::Overlay => b.overlay,
            Self::PhoneBar => b.phone_bar,
        }
    }
}

impl Tokens {
    /// Glass may draw: off under increased contrast and reduce
    /// transparency.
    pub(crate) const fn glass_allowed(&self) -> bool {
        !self.increased_contrast && !self.reduce_transparency && !self.forced
    }

    /// The layer the look tints glass with (`material.glass.tint`).
    pub(crate) fn glass_tint_layer(&self) -> Rgba8 {
        let tint = tok::MATERIAL.glass.tint;
        let layer = if self.dark() { tint.dark } else { tint.light }.layer;
        let l = &self.roles.layer;
        match layer {
            "ground" => l.ground,
            "raised" => l.raised,
            "overlay" => l.overlay,
            other => {
                debug_assert_eq!(other, "sidebar", "glass tints a layer");
                l.sidebar
            }
        }
    }

    /// Glass for `surface` over `layer`: its backdrop blur and the layer as
    /// a tint, or `None` where it draws opaque. Glass draws only on
    /// the surfaces `material.glass.where` lists, never under increased
    /// contrast, reduce transparency or forced colours, and never thinner
    /// than the verified floor. A desktop surface's tint follows the
    /// `Settings.glass` slider; the phone, which has no slider, keeps the
    /// look's authored tint.
    pub(crate) fn glass(&self, surface: GlassSurface, layer: Rgba8) -> Option<(f32, Color)> {
        let glass = tok::MATERIAL.glass;
        if !self.glass_allowed() || !glass.r#where.contains(&surface.key()) {
            return None;
        }
        let alpha = match surface {
            GlassSurface::PhoneBar => {
                let (tint, min) = if self.dark() {
                    (glass.tint.dark.alpha, glass.tint_min.dark)
                } else {
                    (glass.tint.light.alpha, glass.tint_min.light)
                };
                tint.max(min)
            }
            _ => self.glass_tint_alpha(self.glass),
        };
        (alpha < 1.0).then(|| (surface.blur(), self.alpha(layer.paint(), alpha)))
    }

    /// `panel` as glass for `surface` over `layer` ([`Self::glass`]),
    /// traced as [`GLASS_TRACE`] so the budget test can count it;
    /// else filled with `opaque`. The one place a view gets a backdrop blur.
    pub(crate) fn glass_panel(
        &self,
        panel: El,
        surface: GlassSurface,
        layer: Rgba8,
        opaque: Rgba8,
    ) -> El {
        match self.glass(surface, layer) {
            Some((blur, tint)) => panel.backdrop_blur(blur).fill(tint).trace_name(GLASS_TRACE),
            None => panel.fill(opaque.paint()),
        }
    }

    /// The glass tint's alpha for the `Settings.glass` fraction `g`:
    /// `1 − g·(1 − tint_min)`, so full glass stops at the verified floor
    ///. Opaque when glass may not draw.
    pub(crate) fn glass_tint_alpha(&self, g: f32) -> f32 {
        if !self.glass_allowed() || g <= 0.0 {
            return 1.0;
        }
        let min = tok::MATERIAL.glass.tint_min;
        let floor = if self.dark() { min.dark } else { min.light };
        1.0 - g.clamp(0.0, 1.0) * (1.0 - floor)
    }
}
