//! One connection's open exchanges.
//!
//! A slot is reserved before its `SERVICE_HTTP_OPEN` runs and holds the
//! response body (or, on a hub, the satellite's exchange) once the head
//! arrives. A `SERVICE_HTTP_READ` leases the body out of its slot for the
//! length of the read, so one exchange has at most one read in flight; the
//! number of slots bounds the connection's concurrent service work. Ids come
//! from one process-wide counter, so an id from an earlier connection never
//! names a later connection's exchange.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use phux_protocol::ids::SatelliteHost;
use tokio_util::sync::CancellationToken;

use super::Refusal;
use super::upstream::BodyReader;

/// Most exchanges one connection holds open, opening, or reading.
pub(super) const MAX_EXCHANGES: usize = 16;
/// An exchange nobody read for this long is closed at the next open.
pub(super) const IDLE_REAP: Duration = Duration::from_mins(5);

static NEXT_EXCHANGE: AtomicU64 = AtomicU64::new(1);

/// Where an exchange's body comes from.
#[derive(Debug)]
pub(super) enum Upstream {
    /// This server's own service connection.
    Local(Box<BodyReader>),
    /// The same exchange on a satellite, under the satellite's id.
    Remote { host: SatelliteHost, exchange: u64 },
}

#[derive(Debug)]
struct Slot {
    /// `None` while the exchange is opening or a read holds it.
    upstream: Option<Upstream>,
    last_used: Instant,
    /// Cancelled by close; ends a read in flight.
    closed: CancellationToken,
}

/// A connection's exchange table. Cloning shares it.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "the connection dispatcher owns one per connection"
)]
pub(crate) struct Exchanges(Rc<RefCell<HashMap<u64, Slot>>>);

impl Exchanges {
    /// Reserve a slot for an exchange about to open. Dropping the
    /// reservation unfulfilled frees the slot.
    pub(super) fn reserve(&self) -> Result<Reservation, Refusal> {
        let mut slots = self.0.borrow_mut();
        if slots.len() >= MAX_EXCHANGES {
            return Err(Refusal::busy(format!(
                "this connection already holds {MAX_EXCHANGES} service exchanges; close one"
            )));
        }
        let exchange = NEXT_EXCHANGE.fetch_add(1, Ordering::Relaxed);
        slots.insert(
            exchange,
            Slot {
                upstream: None,
                last_used: Instant::now(),
                closed: CancellationToken::new(),
            },
        );
        Ok(Reservation {
            exchanges: self.clone(),
            exchange,
            fulfilled: false,
        })
    }

    /// Lease `exchange`'s upstream for one read.
    pub(super) fn lease(&self, exchange: u64) -> Result<Lease, Refusal> {
        let mut slots = self.0.borrow_mut();
        let slot = slots.get_mut(&exchange).ok_or_else(|| not_open(exchange))?;
        let upstream = slot.upstream.take().ok_or_else(|| {
            Refusal::busy(format!(
                "exchange {exchange} is still opening or already has a read in flight"
            ))
        })?;
        Ok(Lease {
            exchanges: self.clone(),
            exchange,
            upstream,
            closed: slot.closed.clone(),
        })
    }

    /// Close `exchange`: its slot is gone at once and a read in flight ends.
    /// Returns the upstream to release when no read holds it.
    pub(super) fn close(&self, exchange: u64) -> Option<Upstream> {
        let slot = self.0.borrow_mut().remove(&exchange)?;
        slot.closed.cancel();
        slot.upstream
    }

    /// Close every exchange (the connection is ending).
    pub(super) fn close_all(&self) -> Vec<Upstream> {
        self.0
            .borrow_mut()
            .drain()
            .filter_map(|(_, slot)| {
                slot.closed.cancel();
                slot.upstream
            })
            .collect()
    }

    /// Close every idle exchange nobody read for [`IDLE_REAP`].
    pub(super) fn reap_idle(&self, now: Instant) -> Vec<Upstream> {
        let mut slots = self.0.borrow_mut();
        let idle = |slot: &Slot| {
            slot.upstream.is_some() && now.duration_since(slot.last_used) >= IDLE_REAP
        };
        let mut reaped = Vec::new();
        slots.retain(|_, slot| {
            if !idle(slot) {
                return true;
            }
            reaped.extend(slot.upstream.take());
            false
        });
        reaped
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.borrow().len()
    }

    /// Put `upstream` back in its slot. `Err` hands it back when the slot
    /// was closed meanwhile.
    fn restore(&self, exchange: u64, upstream: Upstream) -> Result<(), Upstream> {
        let mut slots = self.0.borrow_mut();
        let Some(slot) = slots.get_mut(&exchange) else {
            return Err(upstream);
        };
        slot.upstream = Some(upstream);
        slot.last_used = Instant::now();
        Ok(())
    }

    fn remove(&self, exchange: u64) {
        self.0.borrow_mut().remove(&exchange);
    }
}

pub(super) fn not_open(exchange: u64) -> Refusal {
    Refusal::not_found(format!("no open service exchange {exchange}"))
}

/// A reserved slot, freed on drop unless fulfilled.
#[derive(Debug)]
pub(super) struct Reservation {
    exchanges: Exchanges,
    exchange: u64,
    fulfilled: bool,
}

impl Reservation {
    pub(super) const fn exchange(&self) -> u64 {
        self.exchange
    }

    /// Store the opened upstream. `Err` hands it back when the exchange was
    /// closed while it opened.
    pub(super) fn fulfill(mut self, upstream: Upstream) -> Result<(), Upstream> {
        self.fulfilled = true;
        self.exchanges.restore(self.exchange, upstream)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.fulfilled {
            self.exchanges.remove(self.exchange);
        }
    }
}

/// One read's hold on an exchange's upstream. A read always runs to its
/// end (the close token or the idle deadline ends it) and then finishes the
/// lease, so the upstream is never lost in between.
#[derive(Debug)]
pub(super) struct Lease {
    exchanges: Exchanges,
    exchange: u64,
    pub(super) upstream: Upstream,
    closed: CancellationToken,
}

impl Lease {
    pub(super) const fn exchange(&self) -> u64 {
        self.exchange
    }

    /// Cancelled when the exchange is closed during the read.
    pub(super) fn closed(&self) -> CancellationToken {
        self.closed.clone()
    }

    /// End the read. A kept upstream returns to its slot; otherwise, or when
    /// the slot was closed meanwhile, the exchange is gone and its upstream
    /// is returned for release.
    pub(super) fn finish(self, keep: bool) -> Option<Upstream> {
        if keep {
            return self.exchanges.restore(self.exchange, self.upstream).err();
        }
        self.exchanges.remove(self.exchange);
        Some(self.upstream)
    }
}
