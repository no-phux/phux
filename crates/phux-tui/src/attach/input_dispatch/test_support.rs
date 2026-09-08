//! Shared fixtures for the dispatcher test suites.

use std::collections::BTreeMap;

use phux_protocol::TerminalId;
use phux_protocol::input::InputEvent;

use crate::layout::{SplitDir, Workspace};

/// Ceiling for draining a scripted peer connection whose writer has
/// already been dropped.
///
/// Not load-bearing: the drain ends on the peer's EOF, and the
/// assertions are on the frames collected — never on how fast they
/// arrived. The timeout only stops a peer that never hangs up from
/// wedging the binary. The 5s it replaces was generous on an idle laptop
/// and a measurement of the scheduler on a saturated one (phux-br1f).
pub(super) const PEER_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) fn tid(id: u32) -> TerminalId {
    TerminalId::local(id)
}

pub(super) fn test_engine_kernel() -> super::super::pane_state::AttachKernel {
    phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    )
}

/// Build a [`ResolvedAction`] with no args.
pub(super) fn bare_action(name: &str) -> phux_config::keybind::ResolvedAction {
    phux_config::keybind::ResolvedAction {
        action: name.to_owned(),
        args: BTreeMap::new(),
    }
}

/// A two-pane Horizontal split with focus on the left leaf, root
/// ratio 0.5 — the fixture the `resize-pane` dispatch tests mutate.
pub(super) fn two_pane_workspace() -> Workspace {
    use crate::layout::{LayoutState, WindowState, split_at};
    let tree = split_at(
        &crate::layout::LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        0.5,
    )
    .unwrap();
    Workspace {
        windows: vec![WindowState {
            name: "1".to_owned(),
            state: LayoutState {
                tree: Some(tree),
                focus: Some(tid(1)),
            },
        }],
        active: 0,
    }
}

pub(super) fn press(key: phux_protocol::input::key::PhysicalKey, text: Option<&str>) -> InputEvent {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet};
    InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: text.and_then(|t| t.chars().next()).map(u32::from),
    })
}

pub(super) fn targets(
    needs_you: usize,
    windows: usize,
    roster: usize,
) -> crate::render::chrome::sidebar::SidebarTargets {
    use crate::render::chrome::sidebar::{SidebarCounts, SidebarTarget, SidebarTargets};
    SidebarTargets {
        counts: SidebarCounts {
            needs_you,
            windows,
            roster,
        },
        needs_you: (0..needs_you)
            .map(|j| {
                // Row 0 is local; the rest are peers, so one fixture
                // exercises both commit shapes.
                if j == 0 {
                    SidebarTarget::Window(1)
                } else {
                    SidebarTarget::Session {
                        name: format!("peer-{j}"),
                        window: 2,
                        pane: 3,
                    }
                }
            })
            .collect(),
        roster: (0..roster).map(|j| format!("space-{j}")).collect(),
    }
}
