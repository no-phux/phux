//! Splits and windows parked by request id until their spawn or attach
//! reply, and the satellite pane each one adopts.

use phux_client::conditional_kill::BoundResource;
use phux_protocol::ResourceId;
use phux_protocol::ids::ServerInstance;

use crate::layout::SplitDir;

/// An in-flight `split-pane`, parked by request id until its
/// `RESOURCE_SPAWNED` reply.
#[derive(Debug, Clone)]
pub(in crate::attach) struct PendingSplit {
    /// The leaf the chord targeted; the split anchors here even if focus
    /// moved meanwhile.
    pub focused_at_request: ResourceId,
    /// Axis along which to split.
    pub dir: SplitDir,
    /// Zoom the new pane instead of un-zooming (`placement = "zoomed"`).
    pub zoom_on_spawn: bool,
    /// The host the new pane was asked of.
    pub host: SplitHost,
    /// `Some` when the split waits on an `ATTACH_RESOURCE` instead of a
    /// spawn: an existing pane (left alone on refusal) or the satellite pane
    /// a spawn answered with (killed on refusal).
    pub adopt: Option<Adopt>,
}

impl PendingSplit {
    /// Whether this split still waits on its spawn reply rather than on an
    /// attach of the pane it adopts.
    pub(in crate::attach) const fn awaiting_spawn(&self) -> bool {
        self.adopt.is_none()
    }

    /// The satellite pane this client spawned for this split, once the
    /// spawn answered.
    pub(in crate::attach) const fn spawned_pane(&self) -> Option<&SpawnedPane> {
        match &self.adopt {
            Some(Adopt::Spawned(pane)) => Some(pane),
            _ => None,
        }
    }

    /// Whether this split opens an existing pane this client did not spawn.
    pub(in crate::attach) const fn opens_existing(&self) -> bool {
        matches!(self.adopt, Some(Adopt::Existing(_)))
    }
}

/// A pane this client spawned on a satellite, with the instance token the
/// reply bound it to (ADR-0109); `None` means it can never be killed
/// conditionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) struct SpawnedPane {
    /// The pane, as the spawn reply named it (satellite-tagged).
    pub id: ResourceId,
    /// The satellite's instance token the spawn was bound to, if any.
    pub instance: Option<ServerInstance>,
}

impl SpawnedPane {
    /// A pane whose spawn reply carried no instance token.
    #[cfg(test)]
    pub(in crate::attach) const fn unbound(id: ResourceId) -> Self {
        Self { id, instance: None }
    }

    /// The pane as a conditional kill names it, when its spawn was bound.
    pub(in crate::attach) fn bound(&self) -> Option<BoundResource> {
        let instance = self.instance?;
        Some(BoundResource {
            id: self.id.clone(),
            instance,
        })
    }
}

/// Which host a parked split's pane is spawned on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::attach) enum SplitHost {
    /// The attached server's own host: a split of a local pane.
    #[default]
    Attached,
    /// A satellite, through the attached hub: a split of a pane there.
    Satellite(phux_protocol::ids::SatelliteHost),
}

/// A window or split parked on a satellite pane's `ATTACH_RESOURCE`.
#[derive(Debug, Clone)]
pub(in crate::attach) enum ParkedAdopt {
    /// A window adopting its pane ([`PendingWindow::adopt`]).
    Window(PendingWindow),
    /// A split adopting its pane ([`PendingSplit::adopt`]).
    Split(PendingSplit),
}

impl ParkedAdopt {
    /// The satellite pane whose attach decides this window or split.
    pub(in crate::attach) fn pane(&self) -> Option<&ResourceId> {
        let adopt = match self {
            Self::Window(window) => window.adopt.as_ref(),
            Self::Split(split) => split.adopt.as_ref(),
        };
        adopt.map(Adopt::pane)
    }

    /// The pane this client spawned for it, if it did.
    pub(in crate::attach) const fn spawned_pane(&self) -> Option<&SpawnedPane> {
        match self {
            Self::Window(window) => window.spawned_pane(),
            Self::Split(split) => split.spawned_pane(),
        }
    }
}

impl PendingWindow {
    /// The satellite pane this client spawned for this window, once the
    /// spawn answered.
    pub(in crate::attach) const fn spawned_pane(&self) -> Option<&SpawnedPane> {
        match &self.adopt {
            Some(Adopt::Spawned(pane)) => Some(pane),
            _ => None,
        }
    }
}

/// An in-flight `new-window`, parked by request id; its reply opens a window
/// named `name` on the spawned pane.
#[derive(Debug, Clone)]
pub(in crate::attach) struct PendingWindow {
    /// Name for the window the spawned pane will seed.
    pub name: String,
    /// `Some` when the window waits on a satellite pane's `ATTACH_RESOURCE`
    /// reply instead of a spawn; it opens only if that attach succeeds.
    pub adopt: Option<Adopt>,
}

/// The satellite pane a parked window attaches before it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) enum Adopt {
    /// A satellite session's existing pane; a refused attach leaves it alone.
    Existing(ResourceId),
    /// A pane this client just spawned; a refused attach kills it.
    Spawned(SpawnedPane),
}

impl Adopt {
    /// The pane, whichever way it came.
    pub(in crate::attach) const fn pane(&self) -> &ResourceId {
        match self {
            Self::Existing(pane) | Self::Spawned(SpawnedPane { id: pane, .. }) => pane,
        }
    }
}
