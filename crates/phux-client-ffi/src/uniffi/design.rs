//! The design language for the mobile apps: the theme solver, the colour
//! maths, the status vocabulary and the theme catalog, from `phux-design`, so
//! iOS and Android draw with the one solver instead of porting it.
//!
//! Inputs are opaque `0xRRGGBB` colours, as the apps hold theme palettes.
//! Outputs are [`DesignRoles`]: every role as an [`Rgba`], in the token
//! groups and role names of `PhuxTokens.Roles`, so the apps map them field by
//! field.

use phux_design::tokens::{
    AccentRoles, BorderRoles, DiffRoles, FlatBorder, FlatDiff, FlatLayer, FlatMatch, FlatRoles,
    FlatScrim, FlatStatus, LayerRoles, MatchRoles, Rgba, SelectionRoles, StatusRoles, TextRoles,
    WashRoles,
};
use phux_design::{Agent, Link, Mode, Params, Rgb8, Roles, State, Theme, ToneInput};

#[uniffi::remote(Record)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[uniffi::remote(Record)]
pub struct LayerRoles {
    pub ground: Rgba,
    pub sidebar: Rgba,
    pub raised: Rgba,
    pub overlay: Rgba,
    pub scrim: Rgba,
}

#[uniffi::remote(Record)]
pub struct TextRoles {
    pub text: Rgba,
    pub dim: Rgba,
    pub faint: Rgba,
    pub inverse: Rgba,
}

#[uniffi::remote(Record)]
pub struct BorderRoles {
    pub muted: Rgba,
    pub base: Rgba,
    pub strong: Rgba,
    pub control: Rgba,
    pub focus: Rgba,
}

#[uniffi::remote(Record)]
pub struct WashRoles {
    pub hover: Rgba,
    pub press: Rgba,
    pub current: Rgba,
    pub selected: Rgba,
}

#[uniffi::remote(Record)]
pub struct AccentRoles {
    pub accent: Rgba,
    pub on_accent: Rgba,
    pub accent_text: Rgba,
    pub hi: Rgba,
    pub lo: Rgba,
}

#[uniffi::remote(Record)]
pub struct StatusRoles {
    pub needs_you: Rgba,
    pub needs_you_wash: Rgba,
    pub needs_you_edge: Rgba,
    pub needs_you_glow: Rgba,
    pub working: Rgba,
    pub working_wash: Rgba,
    pub working_edge: Rgba,
    pub working_glow: Rgba,
    pub done: Rgba,
    pub done_wash: Rgba,
    pub done_edge: Rgba,
    pub done_glow: Rgba,
    pub failed: Rgba,
    pub failed_wash: Rgba,
    pub failed_edge: Rgba,
    pub failed_glow: Rgba,
    pub idle: Rgba,
    pub idle_wash: Rgba,
    pub idle_edge: Rgba,
    pub idle_glow: Rgba,
    pub unknown: Rgba,
    pub unknown_wash: Rgba,
    pub unknown_edge: Rgba,
    pub unknown_glow: Rgba,
    pub bell: Rgba,
    pub bell_wash: Rgba,
    pub bell_edge: Rgba,
    pub bell_glow: Rgba,
}

#[uniffi::remote(Record)]
pub struct SelectionRoles {
    pub bg: Rgba,
    pub fg: Rgba,
}

#[uniffi::remote(Record)]
pub struct MatchRoles {
    pub bg: Rgba,
    pub current_bg: Rgba,
    pub current_fg: Rgba,
}

#[uniffi::remote(Record)]
pub struct DiffRoles {
    pub add: Rgba,
    pub remove: Rgba,
    pub add_text: Rgba,
    pub remove_text: Rgba,
}

#[uniffi::remote(Record)]
pub struct FlatRoles {
    pub ground: FlatLayer,
    pub sidebar: FlatLayer,
    pub raised: FlatLayer,
    pub overlay: FlatLayer,
}

#[uniffi::remote(Record)]
pub struct FlatLayer {
    pub layer: FlatScrim,
    pub border: FlatBorder,
    pub wash: WashRoles,
    pub status: FlatStatus,
    pub r#match: FlatMatch,
    pub diff: FlatDiff,
}

#[uniffi::remote(Record)]
pub struct FlatScrim {
    pub scrim: Rgba,
}

#[uniffi::remote(Record)]
pub struct FlatBorder {
    pub muted: Rgba,
    pub base: Rgba,
    pub strong: Rgba,
}

#[uniffi::remote(Record)]
pub struct FlatStatus {
    pub needs_you_wash: Rgba,
    pub needs_you_edge: Rgba,
    pub needs_you_glow: Rgba,
    pub working_wash: Rgba,
    pub working_edge: Rgba,
    pub working_glow: Rgba,
    pub done_wash: Rgba,
    pub done_edge: Rgba,
    pub done_glow: Rgba,
    pub failed_wash: Rgba,
    pub failed_edge: Rgba,
    pub failed_glow: Rgba,
    pub idle_wash: Rgba,
    pub idle_edge: Rgba,
    pub idle_glow: Rgba,
    pub unknown_wash: Rgba,
    pub unknown_edge: Rgba,
    pub unknown_glow: Rgba,
    pub bell_wash: Rgba,
    pub bell_edge: Rgba,
    pub bell_glow: Rgba,
}

#[uniffi::remote(Record)]
pub struct FlatMatch {
    pub bg: Rgba,
}

#[uniffi::remote(Record)]
pub struct FlatDiff {
    pub add: Rgba,
    pub remove: Rgba,
}

/// A look's mode.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignMode {
    Dark,
    Light,
}

/// Every colour role of one look, for one mode.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct DesignRoles {
    pub mode: DesignMode,
    pub layer: LayerRoles,
    pub text: TextRoles,
    pub border: BorderRoles,
    pub wash: WashRoles,
    pub accent: AccentRoles,
    pub status: StatusRoles,
    pub selection: SelectionRoles,
    pub r#match: MatchRoles,
    pub diff: DiffRoles,
    /// Sixteen code colours.
    pub syntax: Vec<Rgba>,
    /// Each translucent role composited over each layer.
    pub flat: FlatRoles,
}

/// A terminal theme, the solver's input.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct DesignTheme {
    pub bg: u32,
    pub fg: u32,
    /// Exactly sixteen colours.
    pub ansi: Vec<u32>,
    pub cursor: Option<u32>,
    /// The chrome accent; `ansi[6]` when the theme names none.
    pub accent: Option<u32>,
    pub selection_bg: Option<u32>,
    /// An explicit mode beats the luminance test.
    pub dark: Option<bool>,
}

/// One theme of the shipped catalog.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct DesignCatalogTheme {
    pub id: String,
    pub name: String,
    pub theme: DesignTheme,
    /// The clients that ship it: `rune`, `mobile`, `tui`.
    pub ships: Vec<String>,
    /// Whether it has an alternate app icon.
    pub icon: bool,
}

/// What a client knows about a pane, for [`design_tone_of`].
#[derive(uniffi::Record, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "the independent pane facts")]
pub struct DesignToneInput {
    pub agent: Option<DesignAgent>,
    pub attention: bool,
    pub asked: bool,
    pub exit: Option<i32>,
    pub link: Option<DesignLink>,
    pub crashed: bool,
    /// Visited since it finished.
    pub seen: bool,
}

/// The agent state a pane reports.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignAgent {
    Working,
    Blocked,
    Idle,
    Done,
    Unknown,
}

/// The transport to the pane's host.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignLink {
    Ok,
    Lost,
}

/// A rung of the status ladder.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignState {
    NeedsYou,
    Failed,
    Lost,
    Done,
    DoneSeen,
    Working,
    Idle,
    Unknown,
    Ended,
}

#[derive(uniffi::Error, Debug, thiserror::Error, PartialEq, Eq)]
pub enum DesignError {
    #[error("a theme has 16 ansi colours, got {0}")]
    Ansi(u64),
    #[error("a role set has 16 syntax colours, got {0}")]
    Syntax(u64),
}

const fn rgb(c: u32) -> Rgb8 {
    Rgb8::hex(c)
}

impl From<Mode> for DesignMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self::Dark,
            Mode::Light => Self::Light,
        }
    }
}

impl From<DesignMode> for Mode {
    fn from(mode: DesignMode) -> Self {
        match mode {
            DesignMode::Dark => Self::Dark,
            DesignMode::Light => Self::Light,
        }
    }
}

impl From<Roles> for DesignRoles {
    fn from(r: Roles) -> Self {
        Self {
            mode: r.mode.into(),
            layer: r.layer,
            text: r.text,
            border: r.border,
            wash: r.wash,
            accent: r.accent,
            status: r.status,
            selection: r.selection,
            r#match: r.r#match,
            diff: r.diff,
            syntax: r.syntax.to_vec(),
            flat: r.flat,
        }
    }
}

impl TryFrom<DesignRoles> for Roles {
    type Error = DesignError;

    fn try_from(r: DesignRoles) -> Result<Self, DesignError> {
        let len = r.syntax.len() as u64;
        Ok(Self {
            mode: r.mode.into(),
            layer: r.layer,
            text: r.text,
            border: r.border,
            wash: r.wash,
            accent: r.accent,
            status: r.status,
            selection: r.selection,
            r#match: r.r#match,
            diff: r.diff,
            syntax: r.syntax.try_into().map_err(|_| DesignError::Syntax(len))?,
            flat: r.flat,
        })
    }
}

fn palette16(colours: &[u32]) -> Result<[Rgb8; 16], DesignError> {
    let palette: [u32; 16] = colours
        .try_into()
        .map_err(|_| DesignError::Ansi(colours.len() as u64))?;
    Ok(palette.map(rgb))
}

impl TryFrom<&DesignTheme> for Theme {
    type Error = DesignError;

    fn try_from(t: &DesignTheme) -> Result<Self, DesignError> {
        Ok(Self {
            bg: rgb(t.bg),
            fg: rgb(t.fg),
            ansi: palette16(&t.ansi)?,
            cursor: t.cursor.map(rgb),
            accent: t.accent.map(rgb),
            selection_bg: t.selection_bg.map(rgb),
            dark: t.dark,
        })
    }
}

impl From<&phux_design::CatalogTheme> for DesignCatalogTheme {
    fn from(t: &phux_design::CatalogTheme) -> Self {
        let input = t.input();
        Self {
            id: t.id.to_owned(),
            name: t.name.to_owned(),
            theme: DesignTheme {
                bg: input.bg.to_u32(),
                fg: input.fg.to_u32(),
                ansi: input.ansi.map(Rgb8::to_u32).to_vec(),
                cursor: input.cursor.map(Rgb8::to_u32),
                accent: input.accent.map(Rgb8::to_u32),
                selection_bg: input.selection_bg.map(Rgb8::to_u32),
                dark: input.dark,
            },
            ships: t.ships.iter().map(|s| (*s).to_owned()).collect(),
            icon: t.icon,
        }
    }
}

impl From<DesignToneInput> for ToneInput {
    fn from(i: DesignToneInput) -> Self {
        Self {
            agent: i.agent.map(|a| match a {
                DesignAgent::Working => Agent::Working,
                DesignAgent::Blocked => Agent::Blocked,
                DesignAgent::Idle => Agent::Idle,
                DesignAgent::Done => Agent::Done,
                DesignAgent::Unknown => Agent::Unknown,
            }),
            attention: i.attention,
            asked: i.asked,
            exit: i.exit,
            link: i.link.map(|l| match l {
                DesignLink::Ok => Link::Ok,
                DesignLink::Lost => Link::Lost,
            }),
            crashed: i.crashed,
            seen: i.seen,
        }
    }
}

impl From<State> for DesignState {
    fn from(s: State) -> Self {
        match s {
            State::NeedsYou => Self::NeedsYou,
            State::Failed => Self::Failed,
            State::Lost => Self::Lost,
            State::Done => Self::Done,
            State::DoneSeen => Self::DoneSeen,
            State::Working => Self::Working,
            State::Idle => Self::Idle,
            State::Unknown => Self::Unknown,
            State::Ended => Self::Ended,
        }
    }
}

/// Every role of `theme`, solved by the house parameters.
#[uniffi::export]
pub fn design_derive(theme: DesignTheme) -> Result<DesignRoles, DesignError> {
    Ok(phux_design::derive(&Theme::try_from(&theme)?, &Params::house()).into())
}

/// `roles` re-solved at the increased-contrast bars; `ansi` is the palette
/// they were derived from (the house terminal's for the house look).
#[uniffi::export]
pub fn design_contrast_more(
    roles: DesignRoles,
    ansi: Vec<u32>,
) -> Result<DesignRoles, DesignError> {
    let base = Roles::try_from(roles)?;
    Ok(phux_design::contrast_more(&base, &palette16(&ansi)?, &Params::house()).into())
}

/// The house look in `mode`, or its increased-contrast variant.
#[uniffi::export]
pub fn design_house(mode: DesignMode, increased_contrast: bool) -> DesignRoles {
    let mode = mode.into();
    let roles = if increased_contrast {
        Roles::house_contrast_more(mode)
    } else {
        Roles::house(mode)
    };
    roles.into()
}

/// WCAG 2.1 relative luminance of `0xRRGGBB`, through the shipped table.
#[uniffi::export]
pub fn design_luminance(color: u32) -> f64 {
    phux_design::solve::lum(rgb(color), Params::house().srgb_lut)
}

/// WCAG 2.1 contrast ratio of two `0xRRGGBB` colours, symmetric.
#[uniffi::export]
pub fn design_contrast(one: u32, other: u32) -> f64 {
    phux_design::solve::contrast(rgb(one), rgb(other), Params::house().srgb_lut)
}

/// The per-channel sRGB blend `fraction` of the way from `from` to `to`.
#[uniffi::export]
pub fn design_mix(from: u32, to: u32, fraction: f64) -> u32 {
    phux_design::solve::mix(rgb(from), rgb(to), fraction).to_u32()
}

/// A host's `0xRRGGBB` identity colour eased onto `roles` (see
/// `Roles::ease_host`). Pure: memoise per (roles, colour) on a hot path.
#[uniffi::export]
pub fn design_ease_host(color: u32, roles: DesignRoles) -> Result<u32, DesignError> {
    Ok(Roles::try_from(roles)?.ease_host(rgb(color)).to_u32())
}

/// The status-ladder state for a pane's facts.
#[uniffi::export]
pub fn design_tone_of(input: DesignToneInput) -> DesignState {
    phux_design::tone_of(&input.into()).into()
}

/// The English plural message `key` (e.g. `count.needs_you`) for `count`.
#[uniffi::export]
pub fn design_plural(key: String, count: u64) -> Option<String> {
    phux_design::plural(&key, count)
}

/// An ICU plural `message` formatted for `count`.
#[uniffi::export]
pub fn design_plural_message(message: String, count: u64) -> Option<String> {
    phux_design::plural_message(&message, count)
}

/// The catalog themes `client` ships (`mobile`, `rune`, `tui`), in order.
#[uniffi::export]
pub fn design_catalog(client: String) -> Vec<DesignCatalogTheme> {
    phux_design::catalog()
        .shipped(&client)
        .map(DesignCatalogTheme::from)
        .collect()
}

#[cfg(test)]
mod tests;
