//! Skins: the material, shape, chrome and motion the window wears, apart from
//! its colours. A theme says which colours; a skin says how much glass, how
//! round, how much chrome, and how things move. Pairs of numbers are
//! `[light, dark]`.

use std::time::{Duration, Instant};

/// A value a skin shift can pass through. Numbers interpolate; names, enums
/// and switches snap at the halfway point.
trait Blend: Copy {
    fn blend(self, to: Self, t: f32) -> Self;
}

impl Blend for f32 {
    fn blend(self, to: Self, t: f32) -> Self {
        if t <= 0. {
            self
        } else if t >= 1. {
            to
        } else {
            self + (to - self) * t
        }
    }
}

impl Blend for u32 {
    fn blend(self, to: Self, t: f32) -> Self {
        (self as f32).blend(to as f32, t).round() as u32
    }
}

impl<T: Blend, const N: usize> Blend for [T; N] {
    fn blend(self, to: Self, t: f32) -> Self {
        std::array::from_fn(|index| self[index].blend(to[index], t))
    }
}

impl<A: Blend, B: Blend, C: Blend> Blend for (A, B, C) {
    fn blend(self, to: Self, t: f32) -> Self {
        (
            self.0.blend(to.0, t),
            self.1.blend(to.1, t),
            self.2.blend(to.2, t),
        )
    }
}

macro_rules! snaps {
    ($($ty:ty),*) => {$(
        impl Blend for $ty {
            fn blend(self, to: Self, t: f32) -> Self {
                if t >= 0.5 { to } else { self }
            }
        }
    )*};
}

snaps!(bool, &'static str, Backdrop, TabsWhen);

/// A skin struct that blends field by field.
macro_rules! blended {
    ($(#[$meta:meta])* pub struct $name:ident {
        $($(#[$field_meta:meta])* pub $field:ident: $ty:ty,)*
    }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $ty,)*
        }

        impl Blend for $name {
            fn blend(self, to: Self, t: f32) -> Self {
                Self { $($field: self.$field.blend(to.$field, t),)* }
            }
        }
    };
}

/// What the window shows behind its sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// Native glass where the platform has it; blur otherwise.
    Glass,
    /// Nothing: the sheet is the window.
    Opaque,
}

impl Backdrop {
    /// How much of the desktop shows through, for picking the clearer of two.
    fn clarity(self) -> u8 {
        match self {
            Self::Glass => 1,
            Self::Opaque => 0,
        }
    }
}

/// When the titlebar shows workspace tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabsWhen {
    /// Always, even with one tab.
    Always,
    /// Only with two or more tabs.
    Many,
}

/// Popover rims and Kit hairlines in every skin, as foreground alpha.
pub const RIM: [f32; 2] = [0.12, 0.14];

blended! {
    /// One drop shadow. Alpha is of black.
    pub struct Shadow {
        pub alpha: [f32; 2],
        pub y: f32,
        pub blur: f32,
        pub spread: f32,
    }
}

impl Shadow {
    pub const NONE: Self = shadow([0., 0.], 0., 0., 0.);
}

const fn shadow(alpha: [f32; 2], y: f32, blur: f32, spread: f32) -> Shadow {
    Shadow {
        alpha,
        y,
        blur,
        spread,
    }
}

blended! {
    /// Foreground washes laid over the sheet, as alphas of the theme foreground.
    pub struct Washes {
        pub hover: f32,
        pub active: f32,
        pub border: f32,
        pub divider: f32,
    }
}

blended! {
    /// The sheet and the surfaces on it.
    pub struct Material {
        pub backdrop: Backdrop,
        /// Window sheet: the theme background at this alpha.
        pub sheet_alpha: [f32; 2],
        /// Light falling from the top edge: foreground alpha at the top.
        pub sheen: [f32; 2],
        /// How far down the sheen fades out, in points.
        pub sheen_height: f32,
        /// A focused pane's edge, as foreground alpha.
        pub lit: f32,
        /// Pane cards: 0 keeps `terminal_opacity`, 1 is solid.
        pub card_solid: f32,
        /// Popover and dialog backing: the raised colour at this alpha.
        pub raised: f32,
        /// Floating capsule controls: the raised colour at this alpha.
        pub capsule: f32,
        pub washes: Washes,
        pub pane_shadow: Shadow,
        /// Popover and dialog cards: a far lift and a near contact shadow.
        pub popover_shadow: [Shadow; 2],
        pub capsule_shadow: Shadow,
        /// The selected tab's lift.
        pub tab_shadow: Shadow,
    }
}

blended! {
    /// Corner radii and spacing, in points.
    pub struct Shape {
        /// Pane cards.
        pub radius: f32,
        /// Dialogs, palettes and popover cards.
        pub card_radius: f32,
        /// The glass main menu.
        pub popover_radius: f32,
        /// Workspace tabs: `[top, side]`.
        pub tab_radius: [f32; 2],
        /// Kit controls.
        pub kit_radius: f32,
        pub kit_radius_lg: f32,
        /// Sheet around the pane canvas.
        pub gutter: f32,
        /// Sheet between panes, which is also the split's grab area.
        pub gap: f32,
        /// A painted split line. Zero lets the gap separate panes.
        pub divider: f32,
    }
}

blended! {
    /// How much chrome surrounds the panes.
    pub struct Chrome {
        /// Titlebar height in points.
        pub titlebar: f32,
        /// Status bar height in points. Zero hides it.
        pub status: f32,
        /// Sidebar presence, 0 (hidden whatever the prefs say) to 1.
        pub sidebar: f32,
        pub tabs: TabsWhen,
        /// Whether the sheet carries its top sheen.
        pub sheen: bool,
    }
}

blended! {
    /// Spans and springs.
    pub struct Motion {
        pub pane_enter_ms: u32,
        pub pane_leave_ms: u32,
        pub menu_enter_ms: u32,
        pub menu_exit_ms: u32,
        /// Menu spring: stiffness, damping, mass.
        pub spring: (f32, f32, f32),
        /// Cursor trails such as the laser beam.
        pub cursor_effects: bool,
        /// How long a shift into this skin takes.
        pub shift_ms: u32,
    }
}

impl Motion {
    pub const fn pane_enter(&self) -> Duration {
        ms(self.pane_enter_ms)
    }

    pub const fn pane_leave(&self) -> Duration {
        ms(self.pane_leave_ms)
    }

    pub const fn menu_enter(&self) -> Duration {
        ms(self.menu_enter_ms)
    }

    pub const fn menu_exit(&self) -> Duration {
        ms(self.menu_exit_ms)
    }

    pub const fn shift(&self) -> Duration {
        ms(self.shift_ms)
    }
}

blended! {
    /// One named skin.
    pub struct Skin {
        pub id: &'static str,
        pub name: &'static str,
        pub material: Material,
        pub shape: Shape,
        pub chrome: Chrome,
        pub motion: Motion,
    }
}

/// The default: a sheet of glass with floating, rounded pane cards.
pub const GLASS: Skin = Skin {
    id: "glass",
    name: "Liquid Glass",
    material: Material {
        backdrop: Backdrop::Glass,
        sheet_alpha: [0.58, 0.44],
        sheen: [0.07, 0.05],
        sheen_height: 220.,
        lit: 0.22,
        card_solid: 0.,
        raised: 0.97,
        capsule: 0.9,
        washes: Washes {
            hover: 0.06,
            active: 0.11,
            border: 0.08,
            divider: 0.08,
        },
        pane_shadow: shadow([0.08, 0.18], 4., 16., -8.),
        popover_shadow: [
            shadow([0.14, 0.42], 20., 64., -8.),
            shadow([0.06, 0.18], 2., 8., 0.),
        ],
        capsule_shadow: shadow([0.22, 0.22], 4., 14., -2.),
        tab_shadow: shadow([0.05, 0.12], 1., 4., 0.),
    },
    shape: Shape {
        radius: 12.,
        card_radius: 14.,
        popover_radius: 26.,
        tab_radius: [10., 11.],
        kit_radius: 8.,
        kit_radius_lg: 14.,
        gutter: 12.,
        gap: 12.,
        divider: 0.,
    },
    chrome: Chrome {
        titlebar: 44.,
        status: 26.,
        sidebar: 1.,
        tabs: TabsWhen::Always,
        sheen: true,
    },
    motion: Motion {
        pane_enter_ms: 260,
        pane_leave_ms: 190,
        menu_enter_ms: 150,
        menu_exit_ms: 100,
        spring: (950., 48., 1.),
        cursor_effects: true,
        shift_ms: 420,
    },
};

/// A plain terminal: opaque, square, a slim drag strip and nothing else.
pub const TERMINAL: Skin = Skin {
    id: "terminal",
    name: "Terminal",
    material: Material {
        backdrop: Backdrop::Opaque,
        sheet_alpha: [1., 1.],
        sheen: [0., 0.],
        sheen_height: 0.,
        lit: 0.,
        card_solid: 1.,
        raised: 1.,
        capsule: 1.,
        washes: Washes {
            border: 0.1,
            divider: 0.12,
            ..GLASS.material.washes
        },
        pane_shadow: Shadow::NONE,
        popover_shadow: [Shadow::NONE; 2],
        capsule_shadow: Shadow::NONE,
        tab_shadow: Shadow::NONE,
    },
    shape: Shape {
        radius: 0.,
        card_radius: 10.,
        popover_radius: 12.,
        tab_radius: [6., 6.],
        kit_radius: 6.,
        kit_radius_lg: 10.,
        gutter: 0.,
        gap: 0.,
        divider: 1.,
    },
    chrome: Chrome {
        titlebar: 28.,
        status: 0.,
        sidebar: 0.,
        tabs: TabsWhen::Many,
        sheen: false,
    },
    motion: Motion {
        pane_enter_ms: 120,
        pane_leave_ms: 90,
        menu_enter_ms: 90,
        menu_exit_ms: 60,
        spring: (1400., 60., 1.),
        cursor_effects: false,
        shift_ms: 320,
    },
};

static SKINS: [Skin; 2] = [GLASS, TERMINAL];

/// Every shipped skin, default first.
#[must_use]
pub fn skins() -> &'static [Skin] {
    &SKINS
}

/// The skin with `id`, or the default.
#[must_use]
pub fn skin_by_id(id: &str) -> &'static Skin {
    SKINS.iter().find(|skin| skin.id == id).unwrap_or(&SKINS[0])
}

/// The skin id display preferences resolve to: plain wins over the chosen skin.
#[must_use]
pub fn resolved_id(skin_id: &str, plain: bool) -> &'static str {
    if plain {
        TERMINAL.id
    } else {
        skin_by_id(skin_id).id
    }
}

impl Skin {
    /// This skin on a solid window: nothing behind the sheet, and the sheet
    /// and pane cards opaque. Shape, chrome and motion are unchanged.
    #[must_use]
    pub fn opaque(mut self) -> Skin {
        self.material.backdrop = Backdrop::Opaque;
        self.material.sheet_alpha = [1., 1.];
        self.material.card_solid = 1.;
        self
    }

    /// The skin `t` of the way to `to`. Numbers interpolate; names, enums and
    /// switches snap at the halfway point.
    #[must_use]
    pub fn lerp(&self, to: &Skin, t: f32) -> Skin {
        self.blend(*to, t.clamp(0., 1.))
    }

    /// Whether this skin casts any shadow.
    #[must_use]
    pub fn shadows(&self) -> bool {
        let m = &self.material;
        [m.pane_shadow, m.capsule_shadow, m.tab_shadow]
            .iter()
            .chain(&m.popover_shadow)
            .any(|shadow| shadow.alpha.iter().any(|alpha| *alpha > 0.))
    }
}

/// `[light, dark]` pick.
#[must_use]
pub fn tone(pair: [f32; 2], light: bool) -> f32 {
    if light { pair[0] } else { pair[1] }
}

/// A change of skin in flight. The skin painted at `now` is
/// `from.lerp(to, eased progress)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shift {
    pub from: Skin,
    pub to: Skin,
    pub start: Instant,
    pub span: Duration,
}

impl Shift {
    /// A shift over the target's own span, or none when `instant`.
    #[must_use]
    pub fn new(from: Skin, to: Skin, start: Instant, instant: bool) -> Self {
        let span = if instant {
            Duration::ZERO
        } else {
            to.motion.shift()
        };
        Self {
            from,
            to,
            start,
            span,
        }
    }

    /// Linear progress, 0 to 1.
    #[must_use]
    pub fn progress(&self, now: Instant) -> f32 {
        span_progress(self.start, self.span, now)
    }

    #[must_use]
    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.
    }

    /// The skin to paint at `now`.
    #[must_use]
    pub fn skin(&self, now: Instant) -> Skin {
        let t = self.progress(now);
        if t >= 1. {
            self.to
        } else {
            self.from.lerp(&self.to, ease(t))
        }
    }

    /// The window backdrop at `now`: whichever end shows more of the desktop
    /// while moving, so a translucent sheet never sits on an opaque window,
    /// then the target's own once settled.
    #[must_use]
    pub fn backdrop(&self, now: Instant) -> Backdrop {
        let (from, to) = (self.from.material.backdrop, self.to.material.backdrop);
        if self.done(now) || to.clarity() >= from.clarity() {
            to
        } else {
            from
        }
    }
}

/// Linear progress of a span started at `start`, 0 to 1. An empty span is done.
#[must_use]
pub fn span_progress(start: Instant, span: Duration, now: Instant) -> f32 {
    if span.is_zero() {
        return 1.;
    }
    (now.saturating_duration_since(start).as_secs_f32() / span.as_secs_f32()).clamp(0., 1.)
}

/// Smooth start and landing.
fn ease(t: f32) -> f32 {
    if t < 0.5 {
        4. * t * t * t
    } else {
        1. - (-2. * t + 2.).powi(3) / 2.
    }
}

const fn ms(value: u32) -> Duration {
    Duration::from_millis(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_endpoints_are_the_skins_themselves() {
        assert_eq!(GLASS.lerp(&TERMINAL, 0.), GLASS);
        assert_eq!(GLASS.lerp(&TERMINAL, 1.), TERMINAL);
        assert_eq!(TERMINAL.lerp(&GLASS, -3.), TERMINAL);
        assert_eq!(TERMINAL.lerp(&GLASS, 7.), GLASS);
    }

    #[test]
    fn lerp_interpolates_numbers_and_snaps_switches_at_halfway() {
        let early = GLASS.lerp(&TERMINAL, 0.25);
        assert_eq!(early.chrome.titlebar, 40.);
        assert_eq!(early.shape.radius, 9.);
        assert_eq!(early.motion.pane_enter_ms, 225);
        assert_eq!(early.id, "glass");
        assert_eq!(early.material.backdrop, Backdrop::Glass);
        assert_eq!(early.chrome.tabs, TabsWhen::Always);
        assert!(early.motion.cursor_effects);
        let late = GLASS.lerp(&TERMINAL, 0.5);
        assert_eq!(late.id, "terminal");
        assert_eq!(late.material.backdrop, Backdrop::Opaque);
        assert_eq!(late.chrome.tabs, TabsWhen::Many);
        assert!(!late.chrome.sheen);
        assert!(!late.motion.cursor_effects);
        assert_eq!(late.chrome.status, 13.);
    }

    #[test]
    fn unknown_ids_fall_back_to_glass_and_plain_wins() {
        assert_eq!(skins()[0].id, "glass");
        assert_eq!(skin_by_id("terminal").id, "terminal");
        assert_eq!(skin_by_id("neon").id, "glass");
        assert_eq!(skin_by_id("").id, "glass");
        assert_eq!(resolved_id("glass", false), "glass");
        assert_eq!(resolved_id("neon", false), "glass");
        assert_eq!(resolved_id("glass", true), "terminal");
        assert_eq!(resolved_id("terminal", false), "terminal");
    }

    #[test]
    fn glass_keeps_the_shipped_motion_and_terminal_drops_effects() {
        let (glass, terminal) = (skin_by_id("glass"), skin_by_id("terminal"));
        assert_eq!(glass.motion.pane_enter(), Duration::from_millis(260));
        assert_eq!(glass.motion.pane_leave(), Duration::from_millis(190));
        assert!(glass.shadows());
        assert!(!terminal.shadows());
        assert!(!terminal.motion.cursor_effects);
        assert_eq!(terminal.material.backdrop, Backdrop::Opaque);
    }

    #[test]
    fn the_shift_clock_runs_from_start_to_span() {
        let start = Instant::now();
        let span = Duration::from_millis(400);
        assert_eq!(span_progress(start, span, start), 0.);
        assert!((span_progress(start, span, start + span / 4) - 0.25).abs() < 1e-4);
        assert_eq!(span_progress(start, span, start + span), 1.);
        assert_eq!(span_progress(start, span, start + span * 3), 1.);
        assert_eq!(span_progress(start + span, span, start), 0.);
        assert_eq!(span_progress(start, Duration::ZERO, start), 1.);
    }

    #[test]
    fn a_shift_paints_between_skins_and_settles_on_the_target() {
        let start = Instant::now();
        let shift = Shift::new(GLASS, TERMINAL, start, false);
        assert_eq!(shift.span, TERMINAL.motion.shift());
        assert_eq!(shift.skin(start), GLASS);
        let mid = shift.skin(start + shift.span / 2);
        assert!(mid.chrome.titlebar < 44. && mid.chrome.titlebar > 28.);
        assert!(!shift.done(start + shift.span / 2));
        assert_eq!(shift.skin(start + shift.span), TERMINAL);
        assert!(shift.done(start + shift.span));
        let instant = Shift::new(GLASS, TERMINAL, start, true);
        assert!(instant.done(start));
        assert_eq!(instant.skin(start), TERMINAL);
    }

    #[test]
    fn the_backdrop_stays_clear_until_the_sheet_is_solid() {
        let start = Instant::now();
        let out = Shift::new(GLASS, TERMINAL, start, false);
        assert_eq!(out.backdrop(start), Backdrop::Glass);
        assert_eq!(out.backdrop(start + out.span / 2), Backdrop::Glass);
        assert_eq!(out.backdrop(start + out.span), Backdrop::Opaque);
        let back = Shift::new(TERMINAL, GLASS, start, false);
        assert_eq!(back.backdrop(start), Backdrop::Glass);
        assert_eq!(back.backdrop(start + back.span), Backdrop::Glass);
    }
}
