//! Satellites that dial in (ADR-0182, `docs/remote.md#linking-machines`).
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

pub(crate) mod coord;
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
    /// The store satellites' mail and claims go to ([`coord`]).
    work_store: Arc<Mutex<Option<phux_work::Client>>>,
}

impl SatelliteAdopter {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<SatelliteHost, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answer satellites' mail and claims from `store`, or refuse them.
    pub(crate) fn set_work_store(&self, store: Option<phux_work::Client>) {
        *self
            .work_store
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = store;
    }

    /// The store satellites' mail and claims go to, if any.
    pub(crate) fn work_store(&self) -> Option<phux_work::Client> {
        self.work_store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
pub(crate) async fn watch_revocation(
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

pub(crate) fn peer_leaf(connection: &quinn::Connection) -> Option<Vec<u8>> {
    let identity = connection.peer_identity()?;
    let certs = identity.downcast_ref::<Vec<rustls::pki_types::CertificateDer<'static>>>()?;
    certs.first().map(|leaf| leaf.as_ref().to_vec())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::{Adopted, SatelliteAdopter};
    use phux_protocol::ids::SatelliteHost;
    use std::sync::Arc;
    use std::time::Duration;

    /// A loopback listener and a client that trusts it, standing in for the
    /// hub's listener and a dialing satellite.
    struct Loopback {
        _dir: tempfile::TempDir,
        server: quinn::Endpoint,
        client: quinn::Endpoint,
        /// The satellites' ends, kept open: dropping one closes its link.
        dialed: std::sync::Mutex<Vec<quinn::Connection>>,
    }

    impl Loopback {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let cert = dir.path().join("cert.pem");
            let key = dir.path().join("key.pem");
            crate::transport::tls::ensure_self_signed(&cert, &key).unwrap();
            let tls =
                crate::transport::tls::quic_server_config_with_client_ca(&cert, &key, None, false)
                    .unwrap();
            let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
            let server = quinn::Endpoint::server(
                quinn::ServerConfig::with_crypto(Arc::new(crypto)),
                "127.0.0.1:0".parse().unwrap(),
            )
            .unwrap();
            let crypto = phux_dial::tls::client_config(
                &phux_dial::CertTrust::SkipVerify,
                Some(phux_protocol::policy::QUIC_ALPN),
            )
            .unwrap();
            let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
            client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
                quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
            )));
            Self {
                _dir: dir,
                server,
                client,
                dialed: std::sync::Mutex::default(),
            }
        }

        /// One admitted satellite connection, as the listener would offer it.
        async fn arrival(&self, credential: &str) -> Adopted {
            let addr = self.server.local_addr().unwrap();
            let (accepted, dialed) = tokio::join!(
                async { self.server.accept().await.unwrap().await.unwrap() },
                async {
                    self.client
                        .connect(addr, "localhost")
                        .unwrap()
                        .await
                        .unwrap()
                },
            );
            self.dialed.lock().unwrap().push(dialed);
            Adopted {
                endpoint: self.server.clone(),
                connection: accepted,
                credential: credential.to_owned(),
            }
        }
    }

    fn host() -> SatelliteHost {
        SatelliteHost::new("sandbox".to_owned())
    }

    async fn bounded<F: std::future::Future>(future: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("adopter answered in time")
    }

    fn spawn_next(adopter: &SatelliteAdopter) -> tokio::task::JoinHandle<Option<Adopted>> {
        let adopter = adopter.clone();
        tokio::spawn(async move { adopter.next(&host()).await })
    }

    #[tokio::test]
    async fn an_unregistered_satellite_is_handed_back() {
        let net = Loopback::new();
        let adopter = SatelliteAdopter::default();
        let returned = adopter
            .offer(&host(), net.arrival("a").await)
            .expect_err("not accepted");
        assert_eq!(returned.credential, "a");
    }

    #[tokio::test]
    async fn a_newer_connection_supersedes_the_waiting_and_the_handed_one() {
        let net = Loopback::new();
        let adopter = SatelliteAdopter::default();
        adopter.register(&host());

        // A waiting connection is replaced before its supervisor takes it.
        let first = net.arrival("first").await;
        let first_conn = first.connection.clone();
        adopter.offer(&host(), first).unwrap();
        adopter.offer(&host(), net.arrival("second").await).unwrap();
        assert!(
            first_conn.close_reason().is_some(),
            "superseded while waiting"
        );
        let second = bounded(adopter.next(&host())).await.expect("second");
        assert_eq!(second.credential, "second");

        // A handed connection is closed when a newer one arrives, and the
        // supervisor's next call takes the newer one.
        adopter.offer(&host(), net.arrival("third").await).unwrap();
        assert!(
            second.connection.close_reason().is_some(),
            "superseded while handed"
        );
        let third = bounded(adopter.next(&host())).await.expect("third");
        assert_eq!(third.credential, "third");
    }

    #[tokio::test]
    async fn next_closes_the_connection_it_handed_before() {
        let net = Loopback::new();
        let adopter = SatelliteAdopter::default();
        adopter.register(&host());
        adopter.offer(&host(), net.arrival("one").await).unwrap();
        let one = bounded(adopter.next(&host())).await.expect("one");
        assert!(one.connection.close_reason().is_none());

        let waiter = spawn_next(&adopter);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(one.connection.close_reason().is_some(), "its session ended");
        adopter.offer(&host(), net.arrival("two").await).unwrap();
        let two = bounded(waiter).await.unwrap().expect("two");
        assert_eq!(two.credential, "two");
    }

    #[tokio::test]
    async fn a_connection_lost_while_waiting_is_skipped() {
        let net = Loopback::new();
        let adopter = SatelliteAdopter::default();
        adopter.register(&host());
        let lost = net.arrival("lost").await;
        lost.connection.close(0_u32.into(), b"gone");
        adopter.offer(&host(), lost).unwrap();

        let waiter = spawn_next(&adopter);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "a closed connection is never handed out"
        );
        adopter.offer(&host(), net.arrival("live").await).unwrap();
        let live = bounded(waiter).await.unwrap().expect("live");
        assert_eq!(live.credential, "live");
    }

    #[tokio::test]
    async fn unregister_wakes_a_waiting_supervisor_and_refuses_later_arrivals() {
        let net = Loopback::new();
        let adopter = SatelliteAdopter::default();
        adopter.register(&host());
        let waiter = spawn_next(&adopter);
        tokio::time::sleep(Duration::from_millis(50)).await;
        adopter.unregister(&host());
        assert!(
            bounded(waiter).await.unwrap().is_none(),
            "no longer accepted"
        );

        // Unregister closes a waiting and a handed connection alike.
        adopter.register(&host());
        adopter.offer(&host(), net.arrival("held").await).unwrap();
        let held = bounded(adopter.next(&host())).await.expect("held");
        let waiting = net.arrival("waiting").await;
        let waiting_conn = waiting.connection.clone();
        assert!(held.connection.close_reason().is_none());
        adopter.unregister(&host());
        assert!(
            held.connection.close_reason().is_some(),
            "handed link closed"
        );
        assert!(adopter.offer(&host(), waiting).is_err());
        assert!(
            waiting_conn.close_reason().is_none(),
            "handed back, not closed"
        );

        adopter.register(&host());
        let queued = net.arrival("queued").await;
        let queued_conn = queued.connection.clone();
        adopter.offer(&host(), queued).unwrap();
        adopter.unregister(&host());
        assert!(queued_conn.close_reason().is_some(), "waiting link closed");
        assert!(bounded(adopter.next(&host())).await.is_none());
    }
}
