//! The navigation presentation seam. Names remain authoritative; glyphs come
//! from the existing program/config contract, never from guesses about a title.
use phux_protocol::ids::ResourceKind;

use super::Addressable;
use crate::desk::Desk;

/// Terminal display facts projected alongside the topology's one-pass pane
/// lookup. Navigation never rescans every pane for every row on each frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Program {
    name: String,
    glyph: Option<String>,
    folder: String,
}

impl Program {
    pub(crate) fn of(pane: &phux_client_runtime::control::PaneDescriptor) -> Self {
        Self {
            name: pane.label(),
            glyph: phux_config_core::pane_icon::resolve(
                pane.icon.as_deref(),
                pane.foreground.as_deref(),
                pane.cwd.as_deref(),
            ),
            folder: pane
                .cwd
                .as_deref()
                .map(folder)
                .unwrap_or_default()
                .to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Identity {
    pub kind: ResourceKind,
    pub tool: Option<String>,
    /// An agent name alone never implies a terminal/program or a logo.
    pub agent: bool,
    pub glyph: Option<String>,
}

impl Identity {
    pub(crate) fn kind(kind: ResourceKind) -> Self {
        Self {
            kind,
            tool: None,
            agent: false,
            glyph: None,
        }
    }
}

pub(crate) struct Presentation {
    pub name: String,
    /// A declared agent's concise name, else the resource's actual name.
    pub caption: String,
    pub identity: Identity,
    pub detail: String,
}

impl Addressable {
    /// Navigation separates a declared glyph from its name. The palette and
    /// window title keep `Addressable::label`, including their text glyph.
    pub(crate) fn presentation(&self, desk: &Desk) -> Presentation {
        let glyph = self.program.as_ref().and_then(|p| p.glyph.clone());
        let name = self
            .program
            .as_ref()
            .map_or_else(|| self.label.clone(), |p| p.name.clone());
        let detail = if self.kind.is_terminal() {
            self.program
                .as_ref()
                .map(|p| p.folder.clone())
                .unwrap_or_default()
        } else {
            desk.connection
                .resources
                .entries
                .get(&self.id)
                .and_then(|d| d.detail.as_deref())
                .unwrap_or(self.mirror)
                .to_owned()
        };
        Presentation {
            caption: desk
                .activities
                .agent_name(&self.id)
                .map_or_else(|| name.clone(), str::to_owned),
            name,
            identity: Identity {
                kind: self.kind,
                tool: desk.activities.agent_kind(&self.id).map(str::to_owned),
                agent: desk.activities.agent_name(&self.id).is_some(),
                glyph,
            },
            detail,
        }
    }
}

fn folder(cwd: &str) -> &str {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::ResourceId;

    #[test]
    fn declared_glyph_is_separate_from_navigation_name_without_changing_palette_label() {
        let session = crate::test_support::server::session(1, "work", 1);
        let mut pane =
            crate::test_support::server::pane(1, &session, "Build", Some("/repo/project"));
        pane.icon = Some("🦀".to_owned());
        let mut topology = crate::desk::topology(vec![session], vec![pane]);
        crate::test_support::server::terminal_resources(&mut topology);
        let mut desk = crate::test_support::window();
        desk.connection.topology = Some(topology);
        desk.activities.apply(
            &crate::test_support::server::agent(1, "claude", "working"),
            &[],
        );
        let resource = desk.addressable_of(&ResourceId::local(1)).unwrap();
        assert_eq!(resource.label, "🦀 Build");
        let p = resource.presentation(&desk);
        assert_eq!(p.name, "Build");
        assert_eq!(p.identity.glyph.as_deref(), Some("🦀"));
        assert_eq!(p.identity.tool.as_deref(), Some("claude"));
        assert_eq!(p.detail, "project");
    }

    #[test]
    fn names_do_not_declare_tools_and_invalid_glyphs_do_not_cross_the_seam() {
        let session = crate::test_support::server::session(1, "work", 1);
        let mut pane =
            crate::test_support::server::pane(1, &session, "claude codex opencode", None);
        pane.icon = Some("\u{202e}x".to_owned());
        let mut topology = crate::desk::topology(vec![session], vec![pane]);
        crate::test_support::server::terminal_resources(&mut topology);
        let mut desk = crate::test_support::window();
        desk.connection.topology = Some(topology);
        let resource = desk.addressable_of(&ResourceId::local(1)).unwrap();
        let p = resource.presentation(&desk);
        assert_eq!(p.name, "claude codex opencode");
        assert_eq!(p.identity.tool, None);
        assert_eq!(p.identity.glyph, None);
    }

    #[test]
    fn documents_and_future_kinds_use_declared_detail_without_terminal_capabilities() {
        let desk = crate::test_support::window();
        for kind in [
            ResourceKind::Document,
            ResourceKind::PixelSurface,
            ResourceKind::Unknown { tag: 90 },
        ] {
            let resource = Addressable {
                id: ResourceId::local(7),
                label: "Preview".to_owned(),
                session_id: 1,
                session: "work".to_owned(),
                kind,
                mirror: "Read-only mirror",
                parent: None,
                program: None,
            };
            let p = resource.presentation(&desk);
            assert_eq!(p.name, "Preview");
            assert_eq!(p.detail, "Read-only mirror");
            assert_eq!(p.identity, Identity::kind(kind));
        }
        // Addressable's actual listing still rejects unknown kinds; this only
        // exercises presentation's neutral fallback, not a new navigation target.
        assert!(desk.addressable().is_empty());
    }
}
