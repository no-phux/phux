//! One connection's open exchanges.
//!
//! A slot is reserved before its `SERVICE_HTTP_OPEN` runs and holds the
//! response body (or, on a hub, the satellite's exchange) once the head
//! arrives. A `SERVICE_HTTP_READ` leases the body out of its slot for the
//! length of the read, so one exchange has at most one read in flight; the
//! number of slots bounds the connection's concurrent service work, and a
//! byte budget bounds the request bodies opening slots retain for a retry.
//! Ids come from one process-wide counter, so an id from an earlier
//! connection never names a later connection's exchange.
//!
//! [`Exchanges::reap`] closes exchanges whose reader went away: one never
//! read within [`UNREAD_REAP`] of its head (an open whose caller stopped
//! waiting, or a hub's relayed open that timed out there), and one not read
//! for [`IDLE_REAP`].

use std::cell::{Cell, RefCell};
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
/// Most request-body bytes one connection's opening exchanges retain.
pub(super) const MAX_RETAINED_BODY_BYTES: usize = 16 * 1024 * 1024;
/// An open exchange never read this long after its head is closed.
pub(super) const UNREAD_REAP: Duration = Duration::from_mins(1);
/// An open exchange not read for this long is closed.
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
    /// A read has leased it at least once.
    read: bool,
    /// Request-body bytes held while the exchange opens.
    retained: usize,
    /// Cancelled by close; ends a read in flight.
    closed: CancellationToken,
}

impl Slot {
    fn reapable(&self, now: Instant) -> bool {
        let quiet = now.saturating_duration_since(self.last_used);
        let limit = if self.read { IDLE_REAP } else { UNREAD_REAP };
        self.upstream.is_some() && quiet >= limit
    }
}

#[derive(Debug, Default)]
struct Table {
    slots: HashMap<u64, Slot>,
    /// A reaper task is running for this table.
    reaping: Cell<bool>,
}

/// A connection's exchange table. Cloning shares it.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "the connection dispatcher owns one per connection"
)]
pub(crate) struct Exchanges(Rc<RefCell<Table>>);

impl Exchanges {
    /// Reserve a slot for an exchange about to open. Dropping the
    /// reservation unfulfilled frees the slot.
    pub(super) fn reserve(&self, retained: usize) -> Result<Reservation, Refusal> {
        let mut table = self.0.borrow_mut();
        let slots = &mut table.slots;
        if slots.len() >= MAX_EXCHANGES {
            return Err(Refusal::busy(format!(
                "this connection already holds {MAX_EXCHANGES} service exchanges; close one"
            )));
        }
        let held: usize = slots.values().map(|slot| slot.retained).sum();
        if held.saturating_add(retained) > MAX_RETAINED_BODY_BYTES {
            return Err(Refusal::busy(format!(
                "opening request bodies on this connection would exceed \
                 {MAX_RETAINED_BODY_BYTES} bytes; wait for an open to finish"
            )));
        }
        let exchange = NEXT_EXCHANGE.fetch_add(1, Ordering::Relaxed);
        slots.insert(
            exchange,
            Slot {
                upstream: None,
                last_used: Instant::now(),
                read: false,
                retained,
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
        let mut table = self.0.borrow_mut();
        let slots = &mut table.slots;
        let slot = slots.get_mut(&exchange).ok_or_else(|| not_open(exchange))?;
        let upstream = slot.upstream.take().ok_or_else(|| {
            Refusal::busy(format!(
                "exchange {exchange} is still opening or already has a read in flight"
            ))
        })?;
        slot.read = true;
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
        let slot = self.0.borrow_mut().slots.remove(&exchange)?;
        slot.closed.cancel();
        slot.upstream
    }

    /// Close every exchange (the connection is ending).
    pub(super) fn close_all(&self) -> Vec<Upstream> {
        self.0
            .borrow_mut()
            .slots
            .drain()
            .filter_map(|(_, slot)| {
                slot.closed.cancel();
                slot.upstream
            })
            .collect()
    }

    /// Close every exchange whose reader went away (see the module docs).
    pub(super) fn reap(&self, now: Instant) -> Vec<Upstream> {
        let mut reaped = Vec::new();
        self.0.borrow_mut().slots.retain(|_, slot| {
            if !slot.reapable(now) {
                return true;
            }
            reaped.extend(slot.upstream.take());
            false
        });
        reaped
    }

    /// Whether nothing is open, opening, or reading.
    pub(super) fn is_empty(&self) -> bool {
        self.0.borrow().slots.is_empty()
    }

    /// Claim the table's one reaper; `false` when one already runs.
    pub(super) fn claim_reaper(&self) -> bool {
        !self.0.borrow().reaping.replace(true)
    }

    /// The reaper is ending; the next open starts another.
    pub(super) fn release_reaper(&self) {
        self.0.borrow().reaping.set(false);
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.borrow().slots.len()
    }

    /// Put `upstream` back in its slot. `Err` hands it back when the slot
    /// was closed meanwhile.
    fn restore(&self, exchange: u64, upstream: Upstream) -> Result<(), Upstream> {
        let mut table = self.0.borrow_mut();
        let slots = &mut table.slots;
        let Some(slot) = slots.get_mut(&exchange) else {
            return Err(upstream);
        };
        slot.upstream = Some(upstream);
        slot.last_used = Instant::now();
        slot.retained = 0;
        Ok(())
    }

    fn remove(&self, exchange: u64) {
        self.0.borrow_mut().slots.remove(&exchange);
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
