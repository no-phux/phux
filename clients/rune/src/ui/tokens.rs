//! The window's design tokens: one object per appearance, built from
//! phux-brand's tokens (`phux_design`) and the chosen theme, and wrapping
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
use rune::{Color, Cx, El, Family, Spring, SpringPair, TextStyle};

use crate::desk::Desk;
use crate::theme::Theme;

mod kit;
pub(crate) mod phone;
mod status;
#[cfg(test)]
mod tests;
mod tiers;

pub(crate) use status::StatusInk;
pub(crate) use tiers::{Tier, Tiers};

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

/// The seven type roles (§1.4) at this window's text scale, plus `mono`:
/// `label` in the chrome's mono face, for machine nouns.
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub(crate) struct Type {
    pub(crate) micro: TextStyle,
    pub(crate) caption: TextStyle,
    pub(crate) label: TextStyle,
    pub(crate) body: TextStyle,
    pub(crate) lead: TextStyle,
    pub(crate) title: TextStyle,
    pub(crate) display: TextStyle,
    pub(crate) mono: TextStyle,
}

/// Space between the terminal area's edge and its grid: the terminal's
/// own inset, not chrome spacing.
pub(crate) const TERMINAL_PAD: f32 = 10.0;

/// The tallest a sheet grows before its body scrolls: layout geometry,
/// which no token group holds (§1.6 `layout`).
pub(crate) const SHEET_MAX_H: f32 = 620.0;

/// The chrome's mono face (bundled in `grid::FONTS`).
pub(crate) const MONO: Family = Family::Named("Paper Mono");

impl Type {
    fn new(scale: f32) -> Self {
        let r = &tok::TYPE.role;
        let mono = TextStyle {
            family: MONO,
            ..style(&r.label, scale)
        };
        Self {
            micro: style(&r.micro, scale),
            caption: style(&r.caption, scale),
            label: style(&r.label, scale),
            body: style(&r.body, scale),
            lead: style(&r.lead, scale),
            title: style(&r.title, scale),
            display: style(&r.display, scale),
            mono,
        }
    }
}

#[allow(clippy::cast_precision_loss, reason = "font weights are 100..=900")]
fn style(role: &tok::TypeRole, scale: f32) -> TextStyle {
    let size = role.size * scale;
    let features = if role.features.contains(&"tnum") {
        "'tnum' 1"
    } else {
        ""
    };
    TextStyle::new(size, role.weight as f32)
        .lh(role.line_height * scale)
        .tracking(role.tracking_em * size)
        .features(features)
        .family(if role.family == "mono" {
            MONO
        } else {
            Family::Sans
        })
}

/// §1.8's springs as Rune pairs, the curves and loops, and the enter scale.
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
    /// Every colour role (§1.2), the variant for increased contrast included.
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
    /// The system's forced colours drew every role (§1.11).
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
    let roles = solved(inputs);
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
/// 3:1 on the ground, the sidebar and raised surfaces (§1.2 identity).
pub(crate) fn ease_host(rgb: u32, roles: &Roles) -> Color {
    let lut = phux_design::Params::house().srgb_lut;
    let c = Rgb8::hex(rgb);
    let grounds = [roles.layer.ground, roles.layer.sidebar, roles.layer.raised].map(Rgba8::rgb);
    let extreme = roles.mode.extreme();
    let max = f64::from(tok::IDENTITY.host_ease_max);
    let bar = f64::from(tok::IDENTITY.host_bar);
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
        // Forced colours drop every shadow (§1.11).
        let tiers = if forced {
            Tiers::none(&roles)
        } else {
            Tiers::new(&roles)
        };
        let kit = kit::build(&roles, &tiers, &motion, scale);
        let host = inputs
            .host
            .map_or_else(|| roles.accent.accent.paint(), |rgb| ease_host(rgb, &roles));
        let mut t = Self {
            kit,
            status: StatusInk::new(&roles),
            roles,
            space: tok::SPACE,
            radius: tok::RADIUS,
            icon: tok::ICON,
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
        phux_design::BRAND_VERSION.hash(&mut h);
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

    /// `e` lifted to `tier`: its shadow layers, rings already coloured.
    pub(crate) fn elevate(&self, e: El, tier: Tier) -> El {
        self.tiers.of(tier).iter().fold(e, |e, s| e.box_shadow(*s))
    }

    /// `tier` with the glow layer in `color` in place of its second drop
    /// (§1.7): the focused card in its host colour, the waiting dot.
    pub(crate) fn elevate_glowing(&self, e: El, tier: Tier, color: Color) -> El {
        self.tiers
            .glowing(tier, color)
            .into_iter()
            .fold(e, El::box_shadow)
    }

    /// `c` at opacity `a`, blend-correct (`Kit::alpha`).
    pub(crate) fn alpha(&self, c: Color, a: f32) -> Color {
        self.kit.alpha(c, a)
    }

    /// Whether the window is drawn in its dark mode.
    pub(crate) const fn dark(&self) -> bool {
        self.roles.mode.is_dark()
    }

    /// Glass may draw: off under increased contrast and reduce
    /// transparency (§1.9).
    pub(crate) const fn glass_allowed(&self) -> bool {
        !self.increased_contrast && !self.reduce_transparency && !self.forced
    }

    /// The glass tint's alpha for the `Settings.glass` fraction `g`:
    /// `1 − g·(1 − tint_min)`, so full glass stops at the verified floor
    /// (§1.9). Opaque when glass may not draw.
    pub(crate) fn glass_tint_alpha(&self, g: f32) -> f32 {
        if !self.glass_allowed() || g <= 0.0 {
            return 1.0;
        }
        let min = tok::MATERIAL.glass.tint_min;
        let floor = if self.dark() { min.dark } else { min.light };
        1.0 - g.clamp(0.0, 1.0) * (1.0 - floor)
    }
}
