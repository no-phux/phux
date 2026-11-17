//! The phone form factor's tokens (§1.4, §1.6, §4.1.5): its type roles and
//! page margin at the brand's iOS values, the iOS text size the bridge
//! reports, and the tokens for a terminal ground.
//!
//! The generated Rust carries desktop values only, so the phone values are
//! read from the same brand document (`phux.tokens.json`, platform-valued)
//! that generated it: embedded, parsed once, never copied by hand.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;

use phux_design::tokens as tok;
use rune::{Color, Cx};

use super::{Inputs, Tokens, Type, forced, style};
use crate::desk::Desk;
use crate::theme::Theme;

const SOURCE: &str = include_str!("../../../../../crates/phux-design/tokens/phux.tokens.json");

/// Chrome beside the terminal grid stops growing here, so the grid keeps
/// its rows at the largest text sizes (`ChromeScale.gridCapped`, §1.4).
pub(crate) const GRID_CAP: f32 = 1.35;

/// The phone's values from the brand document.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Form {
    roles: [tok::TypeRole; 7],
    page_margin: f32,
}

fn form() -> &'static Form {
    static FORM: OnceLock<Form> = OnceLock::new();
    FORM.get_or_init(|| parse(SOURCE, "ios").expect("the brand tokens carry iOS values"))
}

/// `platform`'s type roles and page margin in the brand document `source`.
fn parse(source: &str, platform: &str) -> Option<Form> {
    let doc: serde_json::Value = serde_json::from_str(source).ok()?;
    #[allow(clippy::cast_possible_truncation, reason = "points fit an f32")]
    let at = |v: &serde_json::Value| v.get(platform)?.as_f64().map(|n| n as f32);
    let r = &tok::TYPE.role;
    let base = [
        ("micro", r.micro),
        ("caption", r.caption),
        ("label", r.label),
        ("body", r.body),
        ("lead", r.lead),
        ("title", r.title),
        ("display", r.display),
    ];
    let mut roles = base.map(|(_, role)| role);
    for (slot, (name, role)) in roles.iter_mut().zip(base) {
        let json = doc.get("type")?.get("role")?.get(name)?;
        *slot = tok::TypeRole {
            size: at(json.get("size")?)?,
            line_height: at(json.get("line_height")?)?,
            ..role
        };
    }
    let page_margin = at(doc.get("space")?.get("page_margin")?)?;
    Some(Form { roles, page_margin })
}

/// The seven roles at the phone's sizes, times `scale`; `mono` is `label`
/// in the chrome's mono face.
pub(crate) fn ty(scale: f32) -> Type {
    let [micro, caption, label, body, lead, title, display] =
        form().roles.map(|r| style(&r, scale));
    Type {
        micro,
        caption,
        label,
        body,
        lead,
        title,
        display,
        mono: rune::TextStyle {
            family: super::MONO,
            ..label
        },
    }
}

/// A phone page's inset from the screen's edge.
pub(crate) fn page_margin() -> f32 {
    form().page_margin
}

/// The factor iOS `UIFontMetrics` applies to body text at a
/// `UIContentSizeCategory` (its raw value), against Large, the default.
/// An unknown category reads as Large.
pub(crate) fn content_scale(category: &str) -> f32 {
    let body = match category.strip_prefix("UICTContentSizeCategory") {
        Some("XS") => 14.0,
        Some("S") => 15.0,
        Some("M") => 16.0,
        Some("XL") => 19.0,
        Some("XXL") => 21.0,
        Some("XXXL") => 23.0,
        Some("AccessibilityM") => 28.0,
        Some("AccessibilityL") => 33.0,
        Some("AccessibilityXL") => 40.0,
        Some("AccessibilityXXL") => 47.0,
        Some("AccessibilityXXXL") => 53.0,
        _ => 17.0,
    };
    body / 17.0
}

/// What the phone's host reports that Rune has no API for: the system text
/// size and Reduce Transparency (§4.1.6). Main-thread state, as every host
/// call is; the bridge sets it, the phone's tokens read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Traits {
    /// [`content_scale`] of the reported category.
    pub(crate) scale: f32,
    pub(crate) reduce_transparency: bool,
}

impl Traits {
    pub(crate) const DEFAULT: Self = Self {
        scale: 1.0,
        reduce_transparency: false,
    };
}

/// The system contrast the phone's view read on its latest run, for the
/// surfaces it hosts that have no `Cx` (and the host's edge colour).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct System {
    increased_contrast: bool,
    forced: Option<forced::Forced>,
}

thread_local! {
    static TRAITS: Cell<Traits> = const { Cell::new(Traits::DEFAULT) };
    static SYSTEM: Cell<System> = Cell::new(System::default());
}

pub(crate) fn traits() -> Traits {
    TRAITS.get()
}

/// Record what the host reported. Returns whether anything changed.
pub(crate) fn set_traits(traits: Traits) -> bool {
    TRAITS.replace(traits) != traits
}

/// Note the system contrast for this run of the phone's view; every
/// [`Tokens::for_phone`] after it takes it.
pub(crate) fn observe(cx: &Cx<Desk>) {
    SYSTEM.set(System {
        increased_contrast: cx.system_increased_contrast(),
        forced: cx.system_contrast().map(forced::Forced::from),
    });
}

/// Which text scale a surface takes: chrome beside the grid is capped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scale {
    Free,
    GridCapped,
}

impl Scale {
    pub(crate) fn of(self, traits: Traits) -> f32 {
        match self {
            Self::Free => traits.scale,
            Self::GridCapped => traits.scale.min(GRID_CAP),
        }
    }
}

impl Tokens {
    /// The phone's tokens (§4.1.5): `theme` when its mode is `dark`'s, else
    /// the house look in that mode, at the reported text size, with the
    /// observed system contrast and the host's Reduce Transparency. Cached
    /// as every `Tokens` is.
    pub(crate) fn for_phone(dark: bool, theme: Option<&Theme>, scale: Scale) -> Rc<Self> {
        let traits = traits();
        let system = SYSTEM.get();
        Self::resolve(&Inputs {
            theme: theme.filter(|t| t.dark == dark).copied(),
            increased_contrast: system.increased_contrast,
            forced: system.forced,
            reduce_transparency: traits.reduce_transparency,
            type_scale: scale.of(traits),
            ..Inputs::system(dark)
        })
    }

    /// Whether `ground` reads as dark: what mode its controls take.
    pub(crate) fn dark_ground(ground: Color) -> bool {
        crate::theme::on_fill(ground) == Color::WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_document_reads_back_the_generated_desktop_values() {
        let desktop = parse(SOURCE, "desktop").expect("desktop values");
        let r = &tok::TYPE.role;
        assert_eq!(
            desktop.roles,
            [
                r.micro, r.caption, r.label, r.body, r.lead, r.title, r.display
            ]
        );
        assert_eq!(desktop.page_margin, tok::SPACE.page_margin);
    }

    #[test]
    fn the_phone_reads_at_its_own_sizes() {
        let ty = ty(1.0);
        // §1.4: the phone's body is 14, its label 14 and micro the HIG's 11.
        assert!((ty.body.size - 14.0).abs() < f32::EPSILON);
        assert!(ty.label.size > tok::TYPE.role.label.size);
        assert!(ty.micro.size >= 11.0);
        assert!(
            page_margin() >= 16.0,
            "§1.6: a phone page margin is at least 16"
        );
    }

    #[test]
    fn text_size_follows_ios_and_caps_beside_the_grid() {
        assert!((content_scale("UICTContentSizeCategoryL") - 1.0).abs() < f32::EPSILON);
        assert!((content_scale("not a category") - 1.0).abs() < f32::EPSILON);
        let ax5 = content_scale("UICTContentSizeCategoryAccessibilityXXXL");
        assert!(ax5 > 3.0);
        assert!(content_scale("UICTContentSizeCategoryXS") < 1.0);
        let traits = Traits {
            scale: ax5,
            reduce_transparency: false,
        };
        assert!((Scale::Free.of(traits) - ax5).abs() < f32::EPSILON);
        assert!((Scale::GridCapped.of(traits) - GRID_CAP).abs() < f32::EPSILON);
        assert!((ty(ax5).body.size - 14.0 * ax5).abs() < 0.01);
    }
}
