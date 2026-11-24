//! The window's design tokens: one object per appearance, built from
//! the design tokens (`phux_design`) and the chosen theme, and wrapping
//! a `rune_kit::Kit` built from the same values.
//!
//! This is an owner path: the raw values and the Rune calls the lints keep
//! out of views (`TextStyle::new`, `El::box_shadow`, `Spring::new`, colour
//! literals) are legal here and nowhere else. Views read `Tokens`.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use phux_design::tokens as tok;
use phux_design::{Mode, Rgb8, Rgba8, Roles};
use rune::{Color, Cx, El, Family, Spring, SpringPair};

use crate::desk::Desk;
use crate::theme::Theme;

pub(crate) mod glass;
mod kit;
pub(crate) mod layout;
pub(crate) mod phone;
mod status;
#[cfg(test)]
mod tests;
mod tiers;
mod ty;

pub(crate) use glass::GlassSurface;
pub(crate) use status::StatusInk;
pub(crate) use tiers::{Tier, Tiers};
use ty::style;
pub(crate) use ty::{Type, Voice};

/// A token colour as Rune paints it: sRGB bytes decoded to linear light,
/// straight alpha. The window blends in sRGB, so a translucent role lands
/// exactly as the brand composited it.
pub(crate) trait Paint {
    fn paint(self) -> Color;
}

impl Paint for Rgba8 {
    fn paint(self) -> Color {
        let rgb = self.rgb();
        let Color { r, g, b, .. } = crate::grid::color(phux_client_runtime::publication::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        });
        Color {
            r,
            g,
            b,
            a: f32::from(self.a) / 255.0,
        }
    }
}

impl Paint for Rgb8 {
    fn paint(self) -> Color {
        self.opaque().paint()
    }
}

/// Space between the terminal area's edge and its grid: the terminal's
/// own inset, not chrome spacing.
pub(crate) const TERMINAL_PAD: f32 = 10.0;
pub(crate) use layout::SHEET_MAX_H;
/// The chrome's mono face (bundled in `grid::FONTS`).
pub(crate) const MONO: Family = Family::Named("Paper Mono");

/// The design springs as Rune pairs, the curves and loops, and the enter scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MotionPairs {
    pub(crate) press: SpringPair,
    pub(crate) control: SpringPair,
    pub(crate) select: SpringPair,
    pub(crate) list: SpringPair,
    pub(crate) pop: SpringPair,
    pub(crate) drawer: SpringPair,
    pub(crate) page: SpringPair,
    pub(crate) settle: SpringPair,
    pub(crate) fade: tok::Curve,
    pub(crate) recolor: tok::Curve,
    pub(crate) pulse: tok::Pulse,
    pub(crate) shimmer: f32,
    pub(crate) indeterminate: f32,
    pub(crate) enter_scale: f32,
}

const fn spring(s: tok::Spring) -> Spring {
    Spring::new(s.duration, s.bounce)
}

const fn pair(p: tok::SpringPair) -> SpringPair {
    SpringPair::new(spring(p.enter), spring(p.exit))
}

impl MotionPairs {
    const fn house() -> Self {
        let m = &tok::MOTION;
        Self {
            press: pair(m.spring.press),
            control: pair(m.spring.control),
            select: pair(m.spring.select),
            list: pair(m.spring.list),
            pop: pair(m.spring.pop),
            drawer: pair(m.spring.drawer),
            page: pair(m.spring.page),
            settle: pair(m.spring.settle),
            fade: m.curve.fade,
            recolor: m.curve.recolor,
            pulse: m.r#loop.pulse,
            shimmer: m.r#loop.shimmer.period,
            indeterminate: m.r#loop.indeterminate.period,
            enter_scale: m.enter_scale,
        }
    }

    fn hash_into(&self, h: &mut impl Hasher) {
        for p in [
            self.press,
            self.control,
            self.select,
            self.list,
            self.pop,
            self.drawer,
            self.page,
            self.settle,
        ] {
            for s in [p.enter, p.exit] {
                [s.duration, s.bounce].map(f32::to_bits).hash(h);
            }
        }
        [
            self.fade.duration,
            self.recolor.duration,
            self.pulse.period,
            self.pulse.min_opacity,
            self.shimmer,
            self.indeterminate,
            self.enter_scale,
        ]
        .map(f32::to_bits)
        .hash(h);
    }
}

/// Icon and mark sizes at the chrome's text scale: a
/// mark keeps its size against the label beside it, so the largest text
/// never sets 30 pt words beside 10 pt marks. The 16 pt drawing grid is
/// the icons' coordinate space, not a size, and stays.
fn icon_at(scale: f32) -> tok::Icon {
    let s = |v: f32| (v * scale * 2.0).round() / 2.0;
    let i = tok::ICON;
    tok::Icon {
        size: tok::IconSize {
            xs: s(i.size.xs),
            sm: s(i.size.sm),
            md: s(i.size.md),
            lg: s(i.size.lg),
            xl: s(i.size.xl),
            xxl: s(i.size.xxl),
        },
        grid: i.grid,
        button: tok::IconButton {
            sm: s(i.button.sm),
            md: s(i.button.md),
            lg: s(i.button.lg),
        },
    }
}

/// What a window's tokens depend on. Equal inputs share one `Tokens`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Inputs {
    /// The chosen theme, by value: Omarchy swaps palettes under one id.
    pub(crate) theme: Option<Theme>,
    /// The system appearance, which picks the house mode with no theme.
    pub(crate) dark: bool,
    /// The system's forced colours (Windows contrast themes), when on.
    pub(crate) forced: Option<forced::Forced>,
    /// The window's machine colour, `0xRRGGBB`, before easing.
    pub(crate) host: Option<u32>,
    /// `Settings.glass`, 0 to 1.
    pub(crate) glass: f32,
    pub(crate) type_scale: f32,
    pub(crate) increased_contrast: bool,
    pub(crate) reduce_transparency: bool,
}

impl Inputs {
    /// The inputs a window has with no theme, glass or host.
    pub(crate) const fn system(dark: bool) -> Self {
        Self {
            theme: None,
            dark,
            forced: None,
            host: None,
            glass: 0.0,
            type_scale: 1.0,
            increased_contrast: false,
            reduce_transparency: false,
        }
    }
}

/// Everything a view styles with.
#[derive(Debug)]
pub(crate) struct Tokens {
    /// Palette, type, radius, motion and elevation for rune-kit components.
    pub(crate) kit: rune_kit::Kit,
    /// The kit the selection controls build with ([`Self::controls`]).
    controls: [rune_kit::Kit; 2],
    /// Every colour role, the variant for increased contrast included.
    pub(crate) roles: Roles,
    pub(crate) space: tok::Space,
    pub(crate) radius: tok::Radius,
    pub(crate) icon: tok::Icon,
    pub(crate) ty: Type,
    pub(crate) motion: MotionPairs,
    pub(crate) tiers: Tiers,
    pub(crate) status: StatusInk,
    /// The window's machine ink, eased to read on every layer.
    pub(crate) host: Color,
    pub(crate) type_scale: f32,
    pub(crate) glass: f32,
    pub(crate) increased_contrast: bool,
    pub(crate) reduce_transparency: bool,
    /// The system's forced colours drew every role.
    pub(crate) forced: bool,
    /// Changes whenever anything a memo reads here does.
    pub(crate) version: u64,
}

mod forced;

/// The house look's mode: the theme's own, else the system appearance.
fn mode_of(inputs: &Inputs) -> Mode {
    let dark = inputs.theme.map_or(inputs.dark, |t| t.dark);
    if dark { Mode::Dark } else { Mode::Light }
}

/// The solver's input for a Rune theme.
pub(crate) fn theme_input(t: &Theme) -> phux_design::Theme {
    phux_design::Theme {
        bg: Rgb8::hex(t.background),
        fg: Rgb8::hex(t.foreground),
        ansi: t.ansi.map(Rgb8::hex),
        cursor: Some(Rgb8::hex(t.cursor)),
        accent: Some(Rgb8::hex(t.accent)),
        selection_bg: t.selection.map(Rgb8::hex),
        dark: Some(t.dark),
    }
}

/// The roles a window draws with: the authored house look with no theme
/// (or the house terminal picked as a theme), else the theme derived.
/// Under increased contrast, the same solved for its higher floors.
fn roles_for(inputs: &Inputs) -> Roles {
    let mut roles = solved(inputs);
    // A grey accent quieter than dim text would invert every emphasis it
    // inks: a palette match, an accent icon or link would be the dimmest
    // thing on screen (Omarchy's Vantablack). Emphasis takes body text
    // there, as the TUI's focus does; weight still marks a match.
    roles.accent.accent_text = roles.emphasis();
    inputs.forced.map_or(roles, |f| f.roles(&roles))
}

fn solved(inputs: &Inputs) -> Roles {
    let mode = mode_of(inputs);
    let params = phux_design::Params::house();
    let theme = inputs
        .theme
        .filter(|t| !matches!(t.id, "phux" | "phux-light"));
    match (theme, inputs.increased_contrast) {
        (None, false) => Roles::house(mode),
        (None, true) => Roles::house_contrast_more(mode),
        (Some(t), more) => {
            let input = theme_input(&t);
            let derived = phux_design::derive(&input, &params);
            if more {
                phux_design::contrast_more(&derived, &input.ansi, &params)
            } else {
                derived
            }
        }
    }
}

/// `rgb` moved toward the mode's extreme, at most 50%, until it reads at
/// 3:1 on the ground, the sidebar and raised surfaces.
pub(crate) fn ease_host(rgb: u32, roles: &Roles) -> Color {
    let lut = phux_design::Params::house().srgb_lut;
    let c = Rgb8::hex(rgb);
    let grounds = [roles.layer.ground, roles.layer.sidebar, roles.layer.raised].map(Rgba8::rgb);
    let extreme = roles.mode.extreme();
    let max = tok::IDENTITY.host_ease_max;
    let bar = tok::IDENTITY.host_bar;
    let step = |s: u8| phux_design::solve::mix(c, extreme, max * f64::from(s) / 50.0);
    (0..=50)
        .map(step)
        .find(|x| phux_design::solve::worst(*x, &grounds, lut) >= bar)
        .unwrap_or_else(|| step(50))
        .paint()
}

impl Tokens {
    fn build(inputs: &Inputs) -> Self {
        let roles = roles_for(inputs);
        let scale = inputs.type_scale;
        let ty = Type::new(scale);
        let motion = MotionPairs::house();
        let forced = inputs.forced.is_some();
        // Forced colours drop every shadow.
        let tiers = if forced {
            Tiers::none(&roles)
        } else {
            Tiers::new(&roles)
        };
        let kit = kit::build(&roles, &tiers, &motion, scale);
        let host = inputs
            .host
            .map_or_else(|| roles.accent.accent.paint(), |rgb| ease_host(rgb, &roles));
        let controls = [false, true].map(|disabled| kit::controls(&kit, &roles, disabled));
        let mut t = Self {
            kit,
            controls,
            status: StatusInk::new(&roles),
            roles,
            space: tok::SPACE,
            radius: tok::RADIUS,
            icon: icon_at(scale),
            ty,
            motion,
            tiers,
            host,
            type_scale: scale,
            glass: inputs.glass,
            increased_contrast: inputs.increased_contrast || forced,
            reduce_transparency: inputs.reduce_transparency,
            forced,
            version: 0,
        };
        t.version = t.fingerprint();
        t
    }

    /// A hash of everything a memo reads here that Rune's memo environment
    /// does not already cover.
    fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        phux_design::DESIGN_VERSION.hash(&mut h);
        self.roles.hash(&mut h);
        self.kit.c.hash(&mut h);
        self.kit.ty.hash(&mut h);
        self.kit.radius.hash(&mut h);
        self.ty.hash(&mut h);
        self.tiers.hash_into(&mut h);
        self.motion.hash_into(&mut h);
        self.host.hash(&mut h);
        [self.type_scale, self.glass].map(f32::to_bits).hash(&mut h);
        (
            self.increased_contrast,
            self.reduce_transparency,
            self.forced,
        )
            .hash(&mut h);
        h.finish()
    }

    /// The tokens for `inputs`, shared with every earlier caller that had
    /// the same: built once, not per frame.
    pub(crate) fn resolve(inputs: &Inputs) -> Rc<Self> {
        const KEEP: usize = 8;
        thread_local! {
            static CACHE: RefCell<Vec<(Inputs, Rc<Tokens>)>> = const { RefCell::new(Vec::new()) };
        }
        CACHE.with_borrow_mut(|cache| {
            if let Some(at) = cache.iter().position(|(k, _)| k == inputs) {
                let hit = cache.remove(at);
                let tokens = Rc::clone(&hit.1);
                cache.insert(0, hit);
                return tokens;
            }
            let tokens = Rc::new(Self::build(inputs));
            cache.insert(0, (*inputs, Rc::clone(&tokens)));
            cache.truncate(KEEP);
            tokens
        })
    }

    /// The desktop window's tokens: the chosen theme, the system
    /// appearance and contrast, the machine and the glass setting.
    pub(crate) fn for_window(desk: &Desk, cx: &Cx<Desk>) -> Rc<Self> {
        let dark = crate::theme::system_dark(cx.system_dark());
        Self::resolve(&Inputs {
            theme: desk.settings.theme.copied(),
            host: Some(crate::host_color::entry(desk.machines.label()).0),
            glass: desk.glass(),
            increased_contrast: cx.system_increased_contrast(),
            forced: cx.system_contrast().map(forced::Forced::from),
            reduce_transparency: desk.reduce_transparency(),
            type_scale: desk.type_scale(),
            ..Inputs::system(dark)
        })
    }

    /// The terminal's selection colours, as the grid paints them.
    pub(crate) fn selection(&self) -> crate::theme::Selection {
        crate::theme::Selection {
            foreground: self.roles.selection.fg.paint(),
            background: self.roles.selection.bg.paint(),
        }
    }

    /// The look of a button that ends something (§1.10): the kit danger
    /// button, unless the failed tone is a grey quieter than dim text
    /// ([`Roles::recedes`]). Omarchy's Vantablack solves it to `#a4a4a4`,
    /// so Close looked disabled beside a white Cancel. The verb still says
    /// what it does.
    pub(crate) fn danger_button(&self) -> rune_kit::ButtonVariant {
        if self.roles.recedes(self.roles.status.failed) {
            rune_kit::ButtonVariant::Neutral
        } else {
            rune_kit::ButtonVariant::Danger
        }
    }

    /// `e` lifted to `tier`: its shadow layers, rings already coloured.
    pub(crate) fn elevate(&self, e: El, tier: Tier) -> El {
        // Tiers retain CSS front-to-back order, as Kit's elevation does.
        self.tiers
            .of(tier)
            .iter()
            .rev()
            .fold(e, |e, s| e.box_shadow(*s))
    }

    /// `tier` with the glow layer in `color` in place of its second drop
    ///: the focused card in its host colour, the waiting dot.
    pub(crate) fn elevate_glowing(&self, e: El, tier: Tier, color: Color) -> El {
        // A cleared role must not regain alpha from the normal glow metadata.
        if self.forced || color.a <= 0.0 {
            return self.elevate(e, tier);
        }
        self.tiers
            .glowing(tier, color)
            .into_iter()
            .rev()
            .fold(e, El::box_shadow)
    }

    /// The kit a switch, checkbox or progress bar builds with: control
    /// boundaries in `border.control`, a checked box in the accent, and a
    /// disabled control's label in `text.faint`.
    pub(crate) const fn controls(&self, disabled: bool) -> &rune_kit::Kit {
        &self.controls[disabled as usize]
    }

    /// `top` over `under`, both translucent, as one fill: the two washes
    /// composited the way the window blends them (in sRGB, straight
    /// alpha), so a held hover over a selection lands as the live one does.
    pub(crate) fn stack(&self, top: Rgba8, under: Rgba8) -> Color {
        let (ta, ua) = (f32::from(top.a) / 255.0, f32::from(under.a) / 255.0);
        let a = ta + ua * (1.0 - ta);
        if a <= 0.0 {
            return Color::TRANSPARENT;
        }
        let mix = |t: u8, u: u8| {
            let v = (f32::from(t) * ta + f32::from(u) * ua * (1.0 - ta)) / a;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "0..=255"
            )]
            let byte = v.round().clamp(0.0, 255.0) as u8;
            byte
        };
        let (tr, ur) = (top.rgb(), under.rgb());
        Rgb8 {
            r: mix(tr.r, ur.r),
            g: mix(tr.g, ur.g),
            b: mix(tr.b, ur.b),
        }
        .with_alpha_byte({
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "0..=1"
            )]
            let byte = (a * 255.0).round() as u8;
            byte
        })
        .paint()
    }

    /// The radius of a surface nested `inset` inside one of `outer`:
    /// the outer radius minus the inset, never under `xs`, so the
    /// two curves run parallel.
    pub(crate) fn nested(&self, outer: f32, inset: f32) -> f32 {
        (outer - inset).max(self.radius.xs)
    }

    /// The one modal entry: exits fade; entries scale up from
    /// `enter_scale` on the pop spring.
    pub(crate) fn enter(&self) -> rune::Transition {
        rune::Transition::asymmetric(
            rune::Transition::scale(self.motion.enter_scale).springs(self.motion.pop),
            rune::Transition::opacity(),
        )
    }

    /// `c` at opacity `a`, blend-correct (`Kit::alpha`).
    pub(crate) fn alpha(&self, c: Color, a: f32) -> Color {
        self.kit.alpha(c, a)
    }

    /// Whether the window is drawn in its dark mode.
    pub(crate) const fn dark(&self) -> bool {
        self.roles.mode.is_dark()
    }
}
