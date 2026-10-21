//! A link's refusals, its observable status, and its reconnect backoff.

use super::{Arc, BTreeMap, Duration, LINK_STABLE_AFTER, Mutex, SatelliteHost};

/// Why the planner refused to dial a satellite (fail closed, ADR-0038).
/// A configuration error; never retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRefusal {
    /// Plaintext `ws://` to a routable host (no TLS, so no pin possible).
    PlaintextRoutable {
        /// The configured endpoint URL.
        url: String,
    },
    /// Routable endpoint with no `token-file` on the registry entry.
    MissingToken {
        /// The configured endpoint.
        endpoint: String,
    },
    /// Routable endpoint with no `cert-fingerprint` pin on the entry.
    MissingFingerprint {
        /// The configured endpoint.
        endpoint: String,
    },
    /// The endpoint failed the dialer's stricter URL parse.
    Malformed {
        /// The configured endpoint.
        endpoint: String,
        /// Why it did not parse.
        reason: String,
    },
}

impl core::fmt::Display for LinkRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PlaintextRoutable { url } => write!(
                f,
                "{url}: refusing plaintext ws:// to a routable host; use wss:// with `phux pair` \
                 credentials"
            ),
            Self::MissingToken { endpoint } => write!(
                f,
                "{endpoint}: refusing to dial a routable satellite without a token-file; run \
                 `phux pair` on the satellite host and register the token file with \
                 `phux host add --role satellite`"
            ),
            Self::MissingFingerprint { endpoint } => write!(
                f,
                "{endpoint}: refusing to dial a routable satellite without a cert-fingerprint \
                 pin; run `phux pair` on the satellite host and register the printed fingerprint"
            ),
            Self::Malformed { endpoint, reason } => {
                write!(f, "{endpoint}: malformed endpoint: {reason}")
            }
        }
    }
}

/// Per-satellite connection state, published by the link supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkStatus {
    /// Fail-closed refusal; never dialed until the registry entry changes.
    Refused {
        /// Human-readable refusal, from [`LinkRefusal`]'s `Display`.
        reason: String,
    },
    /// A dial attempt is in flight.
    Connecting {
        /// 1-based attempt number since the last successful connection.
        attempt: u32,
    },
    /// An `inbound://` satellite has not dialed in yet (ADR-0182).
    Awaiting,
    /// The link is established and authenticated.
    Connected,
    /// The last attempt failed or the link dropped; redial after `retry_in`.
    Backoff {
        /// 1-based number of the attempt that just failed.
        attempt: u32,
        /// Delay before the next dial.
        retry_in: Duration,
        /// Why the attempt failed or the connection dropped.
        last_error: String,
    },
}

impl core::fmt::Display for LinkStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused { reason } => write!(f, "refused (fail closed): {reason}"),
            Self::Connecting { attempt } => write!(f, "connecting (attempt {attempt})"),
            Self::Awaiting => f.write_str("awaiting the satellite's inbound connection"),
            Self::Connected => f.write_str("connected"),
            Self::Backoff {
                attempt,
                retry_in,
                last_error,
            } => write!(
                f,
                "backoff {}ms after attempt {attempt}: {last_error}",
                retry_in.as_millis()
            ),
        }
    }
}

/// Shared per-satellite [`LinkStatus`] map; the mutex is never held across
/// an await.
#[derive(Debug, Clone, Default)]
pub struct HubLinkStatuses {
    pub(super) inner: Arc<Mutex<BTreeMap<SatelliteHost, LinkStatus>>>,
}

impl HubLinkStatuses {
    /// Publish `status` for `host`.
    pub fn set(&self, host: &SatelliteHost, status: LinkStatus) {
        self.lock().insert(host.clone(), status);
    }

    /// The current status for `host`, if its supervisor has reported yet.
    #[must_use]
    pub fn get(&self, host: &SatelliteHost) -> Option<LinkStatus> {
        self.lock().get(host).cloned()
    }

    /// Forget `host` (its link was stopped).
    pub fn remove(&self, host: &SatelliteHost) {
        self.lock().remove(host);
    }

    /// Snapshot every satellite's status, in deterministic name order.
    #[must_use]
    pub fn snapshot(&self) -> BTreeMap<SatelliteHost, LinkStatus> {
        self.lock().clone()
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<SatelliteHost, LinkStatus>> {
        // Poison only means a panic mid-insert; the map stays consistent.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Capped exponential backoff: `base * 2^failures`, saturating at `cap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    pub(super) base: Duration,
    pub(super) cap: Duration,
    pub(super) failures: u32,
}

impl Backoff {
    /// A fresh backoff starting at `base` and never exceeding `cap`.
    #[must_use]
    pub const fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            failures: 0,
        }
    }

    /// Consecutive failures recorded since the last [`Self::reset`].
    #[must_use]
    pub const fn failures(&self) -> u32 {
        self.failures
    }

    /// Record a failure and return the delay before the next attempt.
    pub fn next_delay(&mut self) -> Duration {
        // Clamp the exponent so the shift cannot overflow.
        let exponent = self.failures.min(20);
        let delay = self
            .base
            .saturating_mul(2u32.saturating_pow(exponent))
            .min(self.cap);
        self.failures = self.failures.saturating_add(1);
        delay
    }

    /// Forget the failure streak after a stable connection.
    pub const fn reset(&mut self) {
        self.failures = 0;
    }

    /// Settle the streak after a lost connection and return the 1-based
    /// attempt number. Only a connection up for `LINK_STABLE_AFTER` clears
    /// the streak.
    pub fn settle_after_loss(&mut self, up_for: Duration) -> u32 {
        if up_for >= LINK_STABLE_AFTER {
            self.reset();
        }
        self.failures.saturating_add(1)
    }
}
