//! Satellites that dial in (ADR-0180, `workload-auth.md` §8.3).
//!
//! A satellite with no reachable listener dials the hub's QUIC listener under
//! [`SATELLITE_ALPN`] and presents its satellite credential. The listener
//! hands an admitted connection to the [`SatelliteAdopter`]; the link
//! supervisor of the matching `inbound://` entry takes it in place of a dial
//! ([`super::link`]) and runs the ordinary relay session over a stream it
//! opens. A link is a link regardless of who dialed.
//!
//! One live connection per satellite: a newer one closes the older, and the
//! supervisor asking for the next one closes the one it held. A revoked or
//! expired credential closes its connection within [`REVOCATION_POLL`].
//!
//! [`SATELLITE_ALPN`]: phux_protocol::policy::SATELLITE_ALPN

#![allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) module; pub would be crate-visible and unreachable_pub would reject it"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use phux_protocol::ids::SatelliteHost;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// How often a live adopted connection re-checks its credential.
pub(crate) const REVOCATION_POLL: Duration = Duration::from_secs(1);

/// QUIC application close code for a refused or revoked satellite, the
/// listener's `AUTH_FAILED`.
const AUTH_FAILED_CODE: u32 = 0x01;

/// One admitted satellite connection, waiting for its link supervisor.
#[derive(Debug)]
pub(crate) struct Adopted {
    /// The listener's endpoint, kept alive while the link uses it.
    pub(crate) endpoint: quinn::Endpoint,
    /// The satellite's connection.
    pub(crate) connection: quinn::Connection,
    /// The satellite credential that admitted it.
    pub(crate) credential: String,
}

#[derive(Debug, Default)]
struct Slot {
    /// Admitted, not yet taken by the supervisor.
    waiting: Option<Adopted>,
    /// The connection the supervisor runs its session over.
    handed: Option<quinn::Connection>,
    notify: Arc<Notify>,
}

/// The `inbound://` satellites a hub accepts, and the connections waiting for
/// their link supervisors. Cheap to clone; shared by the QUIC listener and
/// [`super::link::HubLinks`].
#[derive(Debug, Clone, Default)]
pub(crate) struct SatelliteAdopter {
    slots: Arc<Mutex<BTreeMap<SatelliteHost, Slot>>>,
}

impl SatelliteAdopter {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<SatelliteHost, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Accept connections for `host` from now on (its supervisor started).
    pub(crate) fn register(&self, host: &SatelliteHost) {
        let previous = self.lock().insert(host.clone(), Slot::default());
        if let Some(previous) = previous {
            close_slot(previous, b"satellite link restarted");
        }
    }

    /// Stop accepting `host` and close whatever it had (its supervisor
    /// stopped: the entry was removed or disabled).
    pub(crate) fn unregister(&self, host: &SatelliteHost) {
        let removed = self.lock().remove(host);
        if let Some(removed) = removed {
            close_slot(removed, b"satellite link stopped");
        }
    }

    /// Hand `arrival` to `host`'s supervisor, closing any older connection of
    /// the same satellite. `Err` returns it when `host` is not accepted.
    pub(crate) fn offer(&self, host: &SatelliteHost, arrival: Adopted) -> Result<(), Adopted> {
        let mut arrival = Some(arrival);
        let replaced = self.with_slot(host, |slot| {
            let older = (
                slot.waiting.take().map(|older| older.connection),
                slot.handed.take(),
            );
            slot.waiting = arrival.take();
            (older, Arc::clone(&slot.notify))
        });
        let Some((older, notify)) = replaced else {
            return arrival.map_or(Ok(()), Err);
        };
        for older in <[Option<quinn::Connection>; 2]>::from(older)
            .into_iter()
            .flatten()
        {
            older.close(0_u32.into(), b"superseded");
        }
        notify.notify_one();
        Ok(())
    }

    /// The next connection for `host`, closing the one handed out before
    /// (its session is over). `None` once `host` is no longer accepted.
    pub(crate) async fn next(&self, host: &SatelliteHost) -> Option<Adopted> {
        loop {
            let (ended, waiting, notify) = self.with_slot(host, |slot| {
                let ended = slot.handed.take();
                let waiting = slot.waiting.take();
                slot.handed = waiting.as_ref().map(|arrival| arrival.connection.clone());
                (ended, waiting, Arc::clone(&slot.notify))
            })?;
            if let Some(ended) = ended {
                ended.close(0_u32.into(), b"link ended");
            }
            match waiting {
                Some(arrival) if arrival.connection.close_reason().is_none() => {
                    return Some(arrival);
                }
                // Lost before its supervisor took it; `handed` is closed
                // and cleared on the next pass.
                Some(_) | None => notify.notified().await,
            }
        }
    }

    /// Run `f` on `host`'s slot under the lock; `None` when it has none.
    fn with_slot<R>(&self, host: &SatelliteHost, f: impl FnOnce(&mut Slot) -> R) -> Option<R> {
        self.lock().get_mut(host).map(f)
    }
}

fn close_slot(slot: Slot, why: &'static [u8]) {
    if let Some(waiting) = slot.waiting {
        waiting.connection.close(0_u32.into(), why);
    }
    if let Some(handed) = slot.handed {
        handed.close(0_u32.into(), why);
    }
    // Wake a waiting supervisor so it sees the slot gone.
    slot.notify.notify_one();
}

/// Admit one connection that negotiated the satellite ALPN, or close it with
/// `AUTH_FAILED` before any stream: the certificate must map to an active
/// satellite credential whose name is an accepted `inbound://` satellite.
pub(crate) fn admit(
    connection: quinn::Connection,
    endpoint: quinn::Endpoint,
    registry: Option<&Arc<crate::workload::ReloadingWorkloadRegistry>>,
    adopter: &SatelliteAdopter,
) {
    let remote = connection.remote_address();
    let refuse = |connection: &quinn::Connection, why: &str| {
        debug!(%remote, "satellite refused: {why}");
        connection.close(AUTH_FAILED_CODE.into(), b"unauthorized");
    };
    let Some(registry) = registry else {
        return refuse(
            &connection,
            "this listener verifies no workload certificates",
        );
    };
    let Some((credential, name)) =
        peer_leaf(&connection).and_then(|leaf| registry.lookup_satellite_certificate(&leaf))
    else {
        return refuse(&connection, "no active satellite credential");
    };
    let host = SatelliteHost::new(name.clone());
    let arrival = Adopted {
        endpoint,
        connection: connection.clone(),
        credential: credential.clone(),
    };
    if adopter.offer(&host, arrival).is_err() {
        warn!(
            satellite = %host,
            %remote,
            "satellite refused: no enabled inbound:// [[satellites]] entry names it"
        );
        return refuse(&connection, "not an inbound satellite");
    }
    info!(satellite = %host, %remote, %credential, "satellite dialed in; adopting its link");
    tokio::task::spawn_local(watch_revocation(
        connection,
        Arc::clone(registry),
        credential,
        name,
    ));
}

/// Close `connection` once its credential stops being an active satellite
/// credential of `name` (revoked, expired, or the registry broke).
async fn watch_revocation(
    connection: quinn::Connection,
    registry: Arc<crate::workload::ReloadingWorkloadRegistry>,
    credential: String,
    name: String,
) {
    loop {
        tokio::select! {
            _ = connection.closed() => return,
            () = tokio::time::sleep(REVOCATION_POLL) => {}
        }
        if !registry.satellite_is_active(&credential, &name) {
            warn!(satellite = %name, %credential, "satellite credential revoked or expired; closing its link");
            connection.close(AUTH_FAILED_CODE.into(), b"unauthorized");
            return;
        }
    }
}

fn peer_leaf(connection: &quinn::Connection) -> Option<Vec<u8>> {
    let identity = connection.peer_identity()?;
    let certs = identity.downcast_ref::<Vec<rustls::pki_types::CertificateDer<'static>>>()?;
    certs.first().map(|leaf| leaf.as_ref().to_vec())
}
