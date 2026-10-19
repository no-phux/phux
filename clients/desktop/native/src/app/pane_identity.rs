//! What a pane shows and who drives it: the one classification pane cards,
//! tabs and navigator rows style from, instead of each asking "is this an
//! agent?" on its own.
//!
//! [`PaneKind`] is the pane's content. Today a placement shows a terminal or
//! the native conversation face of the agent bound to it; a future kind (an
//! embedded browser, a preview) is a new variant here plus a body renderer in
//! `view/panes/card.rs`, and every surface that styles by identity follows.
//!
//! [`PaneOrigin`] is who drives it. The wire does not carry who created a
//! Terminal to clients, and an agent's `phux spawn` and a person's share one
//! client name, so origin is read from what the server does publish: a
//! `phux.agent/v1` badge on the terminal, or an `AgentSession` bound to it.
//! Either makes the pane agent-driven, with the harness that slug names.

use gpui_kit::assets::IconName;
use phux_client_runtime::control::Topology;
use phux_protocol::ResourceId;

use crate::connection::AgentInfo;
use crate::workspace::layout::{self, DeskTab, Placement};

use super::desk::Desk;

/// An agent harness phux knows by its slug, or any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Codex,
    OpenCode,
    Pi,
    Omp,
    Grok,
    Amp,
    Cursor,
    Gemini,
    Goose,
    Aider,
    /// A slug phux has no mark for, or an agent that named none.
    Other,
}

impl Harness {
    /// Every harness, [`Harness::Other`] last.
    #[cfg(test)]
    pub const ALL: [Self; 12] = [
        Self::Claude,
        Self::Codex,
        Self::OpenCode,
        Self::Pi,
        Self::Omp,
        Self::Grok,
        Self::Amp,
        Self::Cursor,
        Self::Gemini,
        Self::Goose,
        Self::Aider,
        Self::Other,
    ];

    /// The harness a provider slug names: the built-in detector kinds
    /// (`phux-agent-rules`), their binary aliases, and ACP `[acp] provider`
    /// slugs with an `-acp` suffix. Case and surrounding space are ignored.
    #[must_use]
    pub fn from_slug(slug: &str) -> Self {
        let slug = slug.trim().to_ascii_lowercase();
        let slug = slug.strip_prefix('@').unwrap_or(&slug);
        let slug = slug.strip_suffix("-acp").unwrap_or(slug);
        match slug {
            "claude" | "claude-code" | "claude-agent" => Self::Claude,
            "codex" | "codex-cli" => Self::Codex,
            "opencode" | "opencode2" | "opencode-ai" | "opencode-v2" => Self::OpenCode,
            "pi" => Self::Pi,
            "omp" | "oh-my-pi" => Self::Omp,
            "grok" => Self::Grok,
            "amp" => Self::Amp,
            "cursor" | "cursor-agent" => Self::Cursor,
            "gemini" | "gemini-cli" => Self::Gemini,
            "goose" => Self::Goose,
            "aider" => Self::Aider,
            _ => Self::Other,
        }
    }

    /// The name a person reads.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Pi => "Pi",
            Self::Omp => "Oh My Pi",
            Self::Grok => "Grok",
            Self::Amp => "Amp",
            Self::Cursor => "Cursor Agent",
            Self::Gemini => "Gemini",
            Self::Goose => "Goose",
            Self::Aider => "Aider",
            Self::Other => "Agent",
        }
    }

    /// A generic monochrome mark, tinted by [`Self::tint`]. These are plain
    /// Lucide glyphs chosen to be told apart, never a vendor's logo.
    #[must_use]
    pub const fn glyph(self) -> IconName {
        match self {
            Self::Claude => IconName::Asterisk,
            Self::Codex => IconName::CodeXml,
            Self::OpenCode => IconName::Braces,
            Self::Pi => IconName::Pi,
            Self::Omp => IconName::SquarePi,
            Self::Grok => IconName::Orbit,
            Self::Amp => IconName::Zap,
            Self::Cursor => IconName::MousePointer2,
            Self::Gemini => IconName::Sparkle,
            Self::Goose => IconName::Feather,
            Self::Aider => IconName::WandSparkles,
            Self::Other => IconName::Bot,
        }
    }

    /// The command that installs the phux plugin this harness reports its
    /// AgentSession and transcript through (`docs/agents.md`). `None` for a
    /// harness with no hook plugin: its native view is an ACP launch.
    #[must_use]
    pub const fn plugin_install(self) -> Option<&'static str> {
        Some(match self {
            Self::Claude => {
                "claude plugin marketplace add no-phux/phux && claude plugin install phux@phux"
            }
            Self::Grok => {
                "grok plugin marketplace add no-phux/phux && grok plugin install phux@phux --trust"
            }
            Self::Omp => "omp plugin install @phux/omp",
            Self::Pi => "pi install npm:@phux/pi",
            Self::OpenCode => "opencode plugin add @phux/opencode",
            Self::Codex
            | Self::Amp
            | Self::Cursor
            | Self::Gemini
            | Self::Goose
            | Self::Aider
            | Self::Other => return None,
        })
    }

    /// The mark's colour as `[light, dark]` hex, fixed across themes as the
    /// agent state swatches are. `None` takes the theme's muted ink.
    #[must_use]
    pub const fn tint(self) -> Option<[&'static str; 2]> {
        Some(match self {
            Self::Claude => ["#fe640b", "#fab387"],
            Self::Codex => ["#209fb5", "#74c7ec"],
            Self::OpenCode => ["#7287fd", "#b4befe"],
            Self::Pi => ["#8839ef", "#cba6f7"],
            Self::Omp => ["#ea76cb", "#f5c2e7"],
            Self::Grok => ["#dd7878", "#f2cdcd"],
            Self::Amp => ["#e64553", "#eba0ac"],
            Self::Cursor => ["#1e66f5", "#89b4fa"],
            Self::Gemini => ["#04a5e5", "#89dceb"],
            Self::Goose => ["#40a02b", "#a6e3a1"],
            Self::Aider => ["#179299", "#94e2d5"],
            Self::Other => return None,
        })
    }
}

/// Who drives a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneOrigin {
    /// A person's own terminal: nothing says an agent runs there.
    Human,
    /// An agent harness opened it or runs in it.
    Agent(Harness),
}

impl PaneOrigin {
    /// Classify from the two published signals. A bound `AgentSession`'s
    /// provider is structured and wins over a badge's self-declared kind;
    /// either alone makes the pane agent-driven, even when it names no
    /// harness phux has a mark for.
    #[must_use]
    pub fn classify(badge: Option<&AgentInfo>, session_provider: Option<Option<&str>>) -> Self {
        if badge.is_none() && session_provider.is_none() {
            return Self::Human;
        }
        let slug = session_provider
            .flatten()
            .or_else(|| badge.and_then(|info| info.kind.as_deref()));
        Self::Agent(slug.map_or(Harness::Other, Harness::from_slug))
    }

    /// The harness, when an agent drives the pane.
    #[must_use]
    pub const fn harness(self) -> Option<Harness> {
        match self {
            Self::Agent(harness) => Some(harness),
            Self::Human => None,
        }
    }

    #[must_use]
    pub const fn is_agent(self) -> bool {
        matches!(self, Self::Agent(_))
    }
}

/// What a pane shows. The renderer in `view/panes/card.rs` matches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneKind {
    /// A terminal view, or its placeholder while the terminal is missing.
    Terminal,
    /// The native conversation face of the agent bound to its terminal.
    Conversation,
}

/// A pane's kind and origin together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneIdentity {
    pub kind: PaneKind,
    pub origin: PaneOrigin,
}

/// The provider of the `AgentSession` bound to `terminal`: `None` when none
/// is, `Some(None)` when one is but declares no provider.
fn session_provider<'a>(topology: &'a Topology, terminal: &ResourceId) -> Option<Option<&'a str>> {
    topology
        .agent_sessions
        .iter()
        .find(|session| session.parent.as_ref() == Some(terminal))
        .map(|session| session.provider.as_deref())
}

/// Recessed when every placement in the tab is agent-driven; a tab holding
/// any person's pane stays forward.
#[must_use]
pub fn tab_recessed(origins: impl IntoIterator<Item = PaneOrigin>) -> bool {
    let mut any = false;
    for origin in origins {
        if !origin.is_agent() {
            return false;
        }
        any = true;
    }
    any
}

impl Desk {
    /// Who drives `terminal`.
    pub(crate) fn terminal_origin(&self, terminal: &ResourceId) -> PaneOrigin {
        let provider = self
            .link
            .as_ref()
            .and_then(|link| link.connection.topology.as_ref())
            .and_then(|topology| session_provider(topology, terminal));
        PaneOrigin::classify(self.agent(terminal), provider)
    }

    /// Who drives `placement`'s terminal; a placement with none is a person's.
    pub(crate) fn placement_origin(&self, placement: &Placement) -> PaneOrigin {
        placement
            .terminal_id
            .as_ref()
            .map_or(PaneOrigin::Human, |terminal| self.terminal_origin(terminal))
    }

    /// What `placement` shows and who drives it.
    pub(crate) fn pane_identity(&self, placement: &Placement) -> PaneIdentity {
        let kind = if self.conversation_visible(&placement.id) {
            PaneKind::Conversation
        } else {
            PaneKind::Terminal
        };
        PaneIdentity {
            kind,
            origin: self.placement_origin(placement),
        }
    }

    /// The harness of `tab`'s focused pane, else of its first agent pane.
    pub(crate) fn tab_harness(&self, tab: &DeskTab) -> Option<Harness> {
        let focused = layout::focused_placement(tab)
            .and_then(|placement| self.placement_origin(placement).harness());
        focused.or_else(|| {
            layout::placements(&tab.root)
                .into_iter()
                .find_map(|placement| self.placement_origin(placement).harness())
        })
    }

    /// Whether `tab` sits back: only agents drive its panes.
    pub(crate) fn tab_recessed(&self, tab: &DeskTab) -> bool {
        tab_recessed(
            layout::placements(&tab.root)
                .into_iter()
                .map(|placement| self.placement_origin(placement)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{AgentAttention, AgentState};
    use std::collections::HashSet;

    fn badge(kind: Option<&str>) -> AgentInfo {
        AgentInfo {
            name: "agent".into(),
            kind: kind.map(str::to_owned),
            state: AgentState::Working,
            attention: AgentAttention::Low,
            changed_at_ms: 0,
        }
    }

    #[test]
    fn every_builtin_detector_kind_has_its_own_harness() {
        // The `phux-agent-rules` built-in manifests, in their order.
        let kinds = [
            ("claude", Harness::Claude),
            ("codex", Harness::Codex),
            ("opencode", Harness::OpenCode),
            ("pi", Harness::Pi),
            ("omp", Harness::Omp),
            ("grok", Harness::Grok),
            ("amp", Harness::Amp),
            ("cursor-agent", Harness::Cursor),
            ("gemini", Harness::Gemini),
            ("goose", Harness::Goose),
            ("aider", Harness::Aider),
        ];
        for (slug, harness) in kinds {
            assert_eq!(Harness::from_slug(slug), harness, "{slug}");
        }
    }

    #[test]
    fn aliases_acp_suffixes_and_case_resolve_and_unknowns_are_other() {
        assert_eq!(Harness::from_slug("claude-code"), Harness::Claude);
        assert_eq!(Harness::from_slug(" Claude-ACP "), Harness::Claude);
        assert_eq!(Harness::from_slug("codex-acp"), Harness::Codex);
        assert_eq!(Harness::from_slug("@opencode-ai"), Harness::OpenCode);
        assert_eq!(Harness::from_slug("opencode2"), Harness::OpenCode);
        assert_eq!(Harness::from_slug("cursor"), Harness::Cursor);
        assert_eq!(Harness::from_slug("my-agent"), Harness::Other);
        assert_eq!(Harness::from_slug(""), Harness::Other);
        assert_eq!(Harness::from_slug("-acp"), Harness::Other);
    }

    #[test]
    fn plugin_installs_are_the_documented_commands() {
        let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let doc = std::fs::read_to_string(root.join("../../../docs/agents.md")).unwrap();
        let plugged_harnesses = [
            Harness::Claude,
            Harness::Grok,
            Harness::Omp,
            Harness::Pi,
            Harness::OpenCode,
        ];
        for harness in plugged_harnesses {
            let command = harness.plugin_install().expect("a documented plugin");
            for step in command.split(" && ") {
                assert!(doc.contains(step), "{harness:?}: {step}");
            }
        }
        let plugged = Harness::ALL
            .iter()
            .filter(|harness| harness.plugin_install().is_some())
            .count();
        assert_eq!(
            plugged,
            plugged_harnesses.len(),
            "every plugin install is checked here"
        );
    }

    #[test]
    fn each_harness_has_a_distinct_mark_and_only_other_is_untinted() {
        let glyphs: HashSet<_> = Harness::ALL.iter().map(|h| h.glyph()).collect();
        assert_eq!(glyphs.len(), Harness::ALL.len());
        let tints: HashSet<_> = Harness::ALL.iter().filter_map(|h| h.tint()).collect();
        assert_eq!(tints.len(), Harness::ALL.len() - 1);
        assert_eq!(Harness::Other.tint(), None);
        assert_eq!(Harness::Other.glyph(), IconName::Bot);
        for harness in Harness::ALL {
            assert!(!harness.label().is_empty());
        }
    }

    #[test]
    fn every_mark_is_an_embedded_asset() {
        use gpui::AssetSource;
        for harness in Harness::ALL {
            let path = harness.glyph().path();
            assert!(
                crate::assets::Assets.load(&path).unwrap().is_some(),
                "{harness:?} {path}"
            );
        }
    }

    #[test]
    fn a_plain_terminal_is_a_persons() {
        assert_eq!(PaneOrigin::classify(None, None), PaneOrigin::Human);
        assert_eq!(PaneOrigin::Human.harness(), None);
    }

    #[test]
    fn a_badge_or_a_bound_session_makes_the_pane_agent_driven() {
        assert_eq!(
            PaneOrigin::classify(Some(&badge(Some("codex"))), None),
            PaneOrigin::Agent(Harness::Codex)
        );
        assert_eq!(
            PaneOrigin::classify(Some(&badge(None)), None),
            PaneOrigin::Agent(Harness::Other)
        );
        assert_eq!(
            PaneOrigin::classify(None, Some(Some("gemini"))),
            PaneOrigin::Agent(Harness::Gemini)
        );
        assert_eq!(
            PaneOrigin::classify(None, Some(None)),
            PaneOrigin::Agent(Harness::Other)
        );
    }

    #[test]
    fn a_bound_sessions_provider_wins_over_the_badge_kind() {
        let info = badge(Some("claude"));
        assert_eq!(
            PaneOrigin::classify(Some(&info), Some(Some("pi"))),
            PaneOrigin::Agent(Harness::Pi)
        );
        // A session that names no provider defers to the badge.
        assert_eq!(
            PaneOrigin::classify(Some(&info), Some(None)),
            PaneOrigin::Agent(Harness::Claude)
        );
    }

    #[test]
    fn only_an_all_agent_tab_recedes() {
        let agent = PaneOrigin::Agent(Harness::Amp);
        assert!(tab_recessed([agent, agent]));
        assert!(!tab_recessed([agent, PaneOrigin::Human]));
        assert!(!tab_recessed([]));
    }
}
