//! The one place a wire resource id becomes something the runtime can act on.
//!
//! Every terminal-scoped frame and command carries a
//! [`phux_protocol::ids::TerminalId`], and every handler needs the same three
//! answers from it: is this resource ours, does it belong to a satellite we
//! relay for, or is it nothing we know? [`ServerState::resolve_resource`]
//! answers all three once, so the handlers stop open-coding the
//! `is_local()` / `terminal_from_wire` / `resource_handle` ladder and the
//! classification cannot drift between them.
//!
//! The seam classifies; it does not act. [`Resolved::Remote`] hands back the
//! route the hub path already needs (host, the id rewritten into the
//! satellite's own `Local` space, and the link when this server is a hub for
//! that host) and leaves the forwarding — and the off-hub warn-drop or
//! `UnsupportedSatelliteRoute` reply — to the caller, because each frame
//! shapes those differently. Location stays orthogonal to kind (ADR-0016):
//! `Local` yields the kind-agnostic [`ResourceHandle`], and reaching a
//! Terminal-only channel still goes through
//! [`ResourceHandle::terminal`](crate::resource::ResourceHandle::terminal),
//! the crate's sole producer of `WrongResourceKind`.

use phux_protocol::ids::{SatelliteHost, TerminalId as WireTerminalId};

use super::ServerState;
use crate::hub::relay::RelayHandle;
use crate::resource::{ResourceHandle, ResourceId};

/// A resource this server owns, with the handle its engine listens on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalResource<'a> {
    /// The core-domain id, for the registry, lease, and metadata tables.
    pub(crate) id: ResourceId,
    /// The kind-agnostic engine handle.
    pub(crate) handle: &'a ResourceHandle,
}

impl LocalResource<'_> {
    /// Detach from the state borrow by cloning the handle, for callers that
    /// need a `&mut ServerState` afterwards or must send outside the lock.
    fn to_owned_resource(self) -> OwnedLocalResource {
        OwnedLocalResource {
            id: self.id,
            handle: self.handle.clone(),
        }
    }
}

/// [`LocalResource`] with the handle cloned out of the state borrow.
#[derive(Debug, Clone)]
pub(crate) struct OwnedLocalResource {
    /// The core-domain id.
    pub(crate) id: ResourceId,
    /// The kind-agnostic engine handle.
    pub(crate) handle: ResourceHandle,
}

/// Where a satellite-owned resource lives and what the hub link expects.
#[derive(Debug, Clone)]
pub(crate) struct RelayRoute {
    /// The satellite that owns the resource.
    pub(crate) host: SatelliteHost,
    /// The resource's id inside the satellite's own `Local` id space —
    /// what every relayed frame and command carries on the wire.
    pub(crate) id: u32,
    /// The link to `host`, or `None` when this server is not a federation
    /// hub for it. `None` is the caller's `UnsupportedSatelliteRoute`
    /// signal, exactly as a missing `hub_relay` lookup was.
    pub(crate) relay: Option<RelayHandle>,
}

impl RelayRoute {
    /// The satellite-local wire id to stamp into the relayed frame.
    pub(crate) fn local_wire_id(&self) -> WireTerminalId {
        WireTerminalId::local(self.id)
    }
}

/// What a wire resource id resolves to on this server.
#[derive(Debug)]
pub(crate) enum Resolved<'a> {
    /// This server owns the resource and its engine is live.
    Local(LocalResource<'a>),
    /// A satellite owns the resource; the hub relays to it.
    Remote(RelayRoute),
    /// No such resource here: never interned, already reaped, or its
    /// engine handle is gone.
    Unknown,
}

impl Resolved<'_> {
    /// Detach from the state borrow, so the caller can take `&mut
    /// ServerState` or await after resolving.
    pub(crate) fn into_owned(self) -> ResolvedOwned {
        match self {
            Self::Local(local) => ResolvedOwned::Local(local.to_owned_resource()),
            Self::Remote(route) => ResolvedOwned::Remote(route),
            Self::Unknown => ResolvedOwned::Unknown,
        }
    }
}

/// [`Resolved`] with the handle cloned out of the state borrow.
#[derive(Debug)]
pub(crate) enum ResolvedOwned {
    /// This server owns the resource and its engine is live.
    Local(OwnedLocalResource),
    /// A satellite owns the resource; the hub relays to it.
    Remote(RelayRoute),
    /// No such resource here.
    Unknown,
}

impl ServerState {
    /// Classify one wire resource id as local, relayed, or unknown.
    ///
    /// The single seam every terminal-scoped handler routes through.
    /// Satellite-tagged ids are `Remote` whether or not this server is a
    /// hub for the host — the tag alone decides location, and the absent
    /// [`RelayRoute::relay`] is what tells a non-hub server to refuse.
    /// A `Local`-tagged id is `Local` only when it is interned *and* an
    /// engine handle is registered; those two are installed and retired
    /// together, so either miss means the same thing to a caller.
    pub(crate) fn resolve_resource(&self, wire: &WireTerminalId) -> Resolved<'_> {
        if let Some((host, id)) = crate::hub::relay::satellite_route(wire) {
            let relay = self.hub_relay(&host);
            return Resolved::Remote(RelayRoute { host, id, relay });
        }
        let Some(id) = self.terminal_from_wire(wire) else {
            return Resolved::Unknown;
        };
        let Some(handle) = self.resource_handle(id) else {
            return Resolved::Unknown;
        };
        Resolved::Local(LocalResource { id, handle })
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{SatelliteHost, TerminalId as WireTerminalId};
    use tokio_util::sync::CancellationToken;

    use super::{Resolved, ServerState};
    use crate::hub::relay::{HubRelays, RelayHandle};
    use crate::state::tests::mk_handle;

    /// The three classifications, on one state: a resource this server
    /// spawned, a satellite-tagged id (with and without a link), and an id
    /// nothing here ever minted.
    #[test]
    fn resolve_resource_separates_local_relayed_and_unknown() {
        let mut server = ServerState::new();
        let (_session, _window, pane) = server.seed_session("default");
        let wire = server.register_resource_handle(pane, mk_handle(), CancellationToken::new());

        match server.resolve_resource(&wire) {
            Resolved::Local(local) => {
                assert_eq!(
                    local.id, pane,
                    "the seam yields the core id, not the wire id"
                );
                assert_eq!(local.handle.kind, crate::resource::ResourceKind::Terminal);
            }
            other => panic!("a registered local resource must resolve Local, got {other:?}"),
        }

        assert!(
            matches!(
                server.resolve_resource(&WireTerminalId::local(9_999)),
                Resolved::Unknown
            ),
            "a Local-tagged id this server never minted is Unknown",
        );

        let host = SatelliteHost::from("edge");
        let satellite_id = WireTerminalId::satellite(host.clone(), 7);
        match server.resolve_resource(&satellite_id) {
            Resolved::Remote(route) => {
                assert_eq!(route.host, host);
                assert_eq!(route.id, 7);
                assert_eq!(
                    route.local_wire_id(),
                    WireTerminalId::local(7),
                    "the relayed frame carries the satellite's own Local id",
                );
                assert!(
                    route.relay.is_none(),
                    "a server that is not a hub for the host has no link to forward on",
                );
            }
            other => panic!("a Satellite-tagged id must resolve Remote, got {other:?}"),
        }

        let relays = HubRelays::default();
        let (relay, _mailbox) = RelayHandle::new(host.clone());
        relays.insert(relay);
        server.set_hub_relays(relays);
        match server.resolve_resource(&satellite_id) {
            Resolved::Remote(route) => assert!(
                route.relay.is_some(),
                "once the hub dials the host, the route carries its link",
            ),
            other => panic!("a Satellite-tagged id must resolve Remote, got {other:?}"),
        }
    }
}
