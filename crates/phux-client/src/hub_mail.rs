//! A satellite's mail and claims, carried to its hub
//! (`docs/agents.md#mail-and-path-claims`).
//!
//! A server joined to a hub with `phux host join` has an `[[uplink]]` entry:
//! the hub's address, its CA fingerprint, and the satellite credential.
//! [`hub_link`] reads the first enabled one, and its [`HubLink`] sends each
//! work-store call there under [`COORD_ALPN`], one stream per call, so
//! `phux mail` and `phux claim` in a box use the hub's store. The hub
//! answers in the satellite's own name.
//!
//! [`COORD_ALPN`]: phux_protocol::policy::COORD_ALPN

use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use phux_dial::{CertTrust, QuicDial, TlsClientIdentity};
use phux_protocol::policy::COORD_ALPN;
use phux_work::Error;

/// How long dialing the hub may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest reply line, the work store's own limit.
const MAX_LINE: usize = 1 << 20;

/// How often the lifeline watcher checks whether the call is over.
const LIFELINE_POLL: Duration = Duration::from_millis(200);

/// The hub this server joined, as its first enabled `[[uplink]]` entry
/// names it.
#[derive(Debug, Clone)]
pub struct HubLink {
    /// The satellite name the hub knows this server by.
    name: String,
    /// The hub's endpoint as written, for messages.
    hub: String,
    host: String,
    port: u16,
    server_name: String,
    ca: String,
    client_cert: PathBuf,
    client_key: PathBuf,
}

/// The hub to send this host's mail and claims to, when it joined one.
///
/// # Errors
///
/// Wording naming the `[[uplink]]` entry that cannot be dialed. `Ok(None)`
/// when this host joined no hub, or its config does not load.
pub fn hub_link() -> Result<Option<HubLink>, String> {
    let Ok(config) = phux_config::loader::load() else {
        return Ok(None);
    };
    config
        .uplink
        .into_iter()
        .find(|entry| entry.enabled)
        .map(|entry| HubLink::from_entry(&entry))
        .transpose()
}

impl HubLink {
    fn from_entry(entry: &phux_config::UplinkConfigEntry) -> Result<Self, String> {
        let refused = |why: String| format!("[[uplink]] {:?} to {}: {why}", entry.name, entry.hub);
        let (host, port) = entry.hub_host_port().map_err(refused)?;
        let ca = phux_config::known_authorities::canonical_authority(&entry.ca_fingerprint)
            .ok_or_else(|| refused("ca-fingerprint is not a sha256: fingerprint".to_owned()))?;
        Ok(Self {
            name: entry.name.clone(),
            hub: entry.hub.clone(),
            server_name: entry.server_name.clone().unwrap_or_else(|| host.clone()),
            host,
            port,
            ca,
            client_cert: entry.client_cert.clone(),
            client_key: entry.client_key.clone(),
        })
    }

    /// The satellite name the hub knows this server by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The hub's endpoint, `HOST:PORT`.
    #[must_use]
    pub fn hub(&self) -> &str {
        &self.hub
    }

    async fn connect(&self) -> Result<(quinn::Endpoint, quinn::Connection), String> {
        let addr = match self.host.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, self.port),
            Err(_) => tokio::net::lookup_host((self.host.as_str(), self.port))
                .await
                .map_err(|err| format!("resolve {}: {err}", self.hub))?
                .next()
                .ok_or_else(|| format!("resolve {}: no addresses", self.hub))?,
        };
        let dial = QuicDial {
            addr,
            server_name: self.server_name.clone(),
            token: None,
            trust: CertTrust::Authority {
                ca: self.ca.clone(),
            },
            identity: Some(TlsClientIdentity::PemFiles {
                certificate: self.client_cert.clone(),
                private_key: self.client_key.clone(),
            }),
            inner: None,
        };
        phux_dial::quic::connect_with_alpn(&dial, COORD_ALPN)
            .await
            .map_err(|err| err.to_string())
    }

    async fn call(&self, line: &str, reply: Duration, gone: Lifeline) -> Result<String, Error> {
        let unreachable = |why: String| {
            Error::Keeper(format!(
                "could not reach the hub {} that {} joined: {why}",
                self.hub, self.name
            ))
        };
        let (endpoint, connection) = tokio::time::timeout(CONNECT_TIMEOUT, self.connect())
            .await
            .map_err(|_| unreachable("no answer within 5s".to_owned()))?
            .map_err(unreachable)?;
        let exchange = async {
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(line.as_bytes()).await?;
            send.finish()?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(recv.read_to_end(MAX_LINE).await?)
        };
        let outcome = tokio::select! {
            outcome = tokio::time::timeout(reply, exchange) => Some(outcome),
            () = gone.closed() => None,
        };
        connection.close(0_u32.into(), b"done");
        drop(tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await);
        match outcome {
            None => Err(Error::Keeper(
                "the waiting caller exited; nothing was taken".to_owned(),
            )),
            Some(Err(_)) => Err(Error::Keeper("no reply before the deadline".to_owned())),
            Some(Ok(Err(err))) => Err(unreachable(err.to_string())),
            Some(Ok(Ok(bytes))) => String::from_utf8(bytes)
                .map_err(|_| Error::Keeper("the hub's reply is not UTF-8".to_owned())),
        }
    }
}

impl phux_work::Exchange for HubLink {
    fn exchange(
        &self,
        line: &str,
        reply: Duration,
        lifeline: Option<BorrowedFd<'_>>,
    ) -> Result<String, Error> {
        let gone = Lifeline::watch(lifeline)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| Error::Keeper(format!("could not start a runtime: {err}")))?;
        runtime.block_on(self.call(line, reply, gone))
    }
}

/// Resolves once the caller's lifeline closes; never without one.
struct Lifeline {
    closed: Option<tokio::sync::oneshot::Receiver<()>>,
    done: Arc<AtomicBool>,
}

impl Lifeline {
    /// Watch a copy of `lifeline` on a thread, as the local client polls it.
    fn watch(lifeline: Option<BorrowedFd<'_>>) -> Result<Self, Error> {
        let done = Arc::new(AtomicBool::new(false));
        let Some(lifeline) = lifeline else {
            return Ok(Self { closed: None, done });
        };
        let fd: OwnedFd = lifeline
            .try_clone_to_owned()
            .map_err(|err| Error::Keeper(format!("could not watch the caller: {err}")))?;
        let (closed, receiver) = tokio::sync::oneshot::channel();
        let over = Arc::clone(&done);
        std::thread::spawn(move || {
            while !over.load(Ordering::Relaxed) {
                if readable(&fd) {
                    closed.send(()).ok();
                    return;
                }
            }
        });
        Ok(Self {
            closed: Some(receiver),
            done,
        })
    }

    async fn closed(mut self) {
        if let Some(receiver) = self.closed.take()
            && receiver.await.is_ok()
        {
            return;
        }
        std::future::pending::<()>().await;
    }
}

impl Drop for Lifeline {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

/// Whether `fd` turned readable (its write end closed) within one poll.
fn readable(fd: &OwnedFd) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let mut fds = [PollFd::from_borrowed_fd(fd.as_fd(), PollFlags::IN)];
    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: i64::from(LIFELINE_POLL.subsec_nanos()),
    };
    matches!(rustix::event::poll(&mut fds, Some(&timeout)), Ok(n) if n > 0)
}
