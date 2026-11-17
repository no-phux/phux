//! The full colour role set (spec §1.2), owned so a derived theme can hold one.

use crate::color::{Mode, Rgba8};
use crate::tokens::{
    self, AccentRoles, BorderRoles, DiffRoles, FlatLayer, FlatRoles, LayerRoles, MatchRoles,
    SelectionRoles, StatusRoles, TextRoles, WashRoles,
};

/// Every colour role, for one mode.
///
/// The house look's, a variant's, or one [`crate::derive()`] solved from a
/// terminal theme. Translucent roles carry their alpha (`#RRGGBBAA`); `flat`
/// holds each composited over each layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roles {
    pub mode: Mode,
    pub layer: LayerRoles,
    pub text: TextRoles,
    pub border: BorderRoles,
    pub wash: WashRoles,
    pub accent: AccentRoles,
    pub status: StatusRoles,
    pub selection: SelectionRoles,
    pub r#match: MatchRoles,
    pub diff: DiffRoles,
    pub syntax: [Rgba8; 16],
    pub flat: FlatRoles,
}

/// One of the four opaque layers (`T`, back to front).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    Ground,
    Sidebar,
    Raised,
    Overlay,
}

impl Layer {
    pub const ALL: [Self; 4] = [Self::Ground, Self::Sidebar, Self::Raised, Self::Overlay];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Ground => "ground",
            Self::Sidebar => "sidebar",
            Self::Raised => "raised",
            Self::Overlay => "overlay",
        }
    }
}

impl Roles {
    /// The authored house look.
    #[must_use]
    pub fn house(mode: Mode) -> Self {
        Self::from_tokens(mode, pick(mode, &tokens::COLOR))
    }

    /// The house look's increased-contrast variant (§1.11).
    #[must_use]
    pub fn house_contrast_more(mode: Mode) -> Self {
        Self::from_tokens(mode, pick(mode, &tokens::CONTRAST_MORE))
    }

    /// An owned copy of generated roles.
    #[must_use]
    pub fn from_tokens(mode: Mode, r: &tokens::Roles) -> Self {
        let mut syntax = [Rgba8::hex(0); 16];
        for (slot, c) in syntax.iter_mut().zip(r.syntax) {
            *slot = *c;
        }
        Self {
            mode,
            layer: r.layer,
            text: r.text,
            border: r.border,
            wash: r.wash,
            accent: r.accent,
            status: r.status,
            selection: r.selection,
            r#match: r.r#match,
            diff: r.diff,
            syntax,
            flat: r.flat,
        }
    }

    /// The opaque colour of `layer`.
    #[must_use]
    pub const fn layer(&self, layer: Layer) -> Rgba8 {
        match layer {
            Layer::Ground => self.layer.ground,
            Layer::Sidebar => self.layer.sidebar,
            Layer::Raised => self.layer.raised,
            Layer::Overlay => self.layer.overlay,
        }
    }

    /// The translucent roles composited over `layer`.
    #[must_use]
    pub const fn flat(&self, layer: Layer) -> &FlatLayer {
        match layer {
            Layer::Ground => &self.flat.ground,
            Layer::Sidebar => &self.flat.sidebar,
            Layer::Raised => &self.flat.raised,
            Layer::Overlay => &self.flat.overlay,
        }
    }

    /// A tone and its companions.
    #[must_use]
    pub const fn tone(&self, tone: crate::Tone) -> ToneInk {
        let s = &self.status;
        let (ink, wash, edge, glow) = match tone {
            crate::Tone::NeedsYou => (
                s.needs_you,
                s.needs_you_wash,
                s.needs_you_edge,
                s.needs_you_glow,
            ),
            crate::Tone::Working => (s.working, s.working_wash, s.working_edge, s.working_glow),
            crate::Tone::Done => (s.done, s.done_wash, s.done_edge, s.done_glow),
            crate::Tone::Failed => (s.failed, s.failed_wash, s.failed_edge, s.failed_glow),
            crate::Tone::Idle => (s.idle, s.idle_wash, s.idle_edge, s.idle_glow),
            crate::Tone::Unknown => (s.unknown, s.unknown_wash, s.unknown_edge, s.unknown_glow),
            crate::Tone::Bell => (s.bell, s.bell_wash, s.bell_edge, s.bell_glow),
        };
        ToneInk {
            ink,
            wash,
            edge,
            glow,
        }
    }
}

/// A status tone with its three companions (§1.2): the alpha lives in each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToneInk {
    pub ink: Rgba8,
    pub wash: Rgba8,
    pub edge: Rgba8,
    pub glow: Rgba8,
}

const fn pick(mode: Mode, modes: &tokens::RoleModes) -> &tokens::Roles {
    match mode {
        Mode::Dark => &modes.dark,
        Mode::Light => &modes.light,
    }
}

/// Every field, so a memo keyed on roles repaints on any change. The token
/// groups are generated without `Hash`; their `Debug` form lists every
/// colour, so hashing it can never miss a role a later brand release adds.
impl std::hash::Hash for Roles {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        format!("{self:?}").hash(state);
    }
}
