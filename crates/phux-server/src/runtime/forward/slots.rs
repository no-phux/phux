//! One connection's forwards. A slot is reserved before `FORWARD_OPEN` dials;
//! reads and writes each lease one half, so a direction has at most one
//! command in flight. Ids are process-wide, never reused across connections.
//! [`Forwards::reap`] closes forwards unused within [`UNUSED_REAP`] or idle
//! for [`IDLE_REAP`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Most forwards one connection holds dialing or open.
pub(super) const MAX_FORWARDS: usize = 32;
/// A forward never read or written this long after its dial is closed.
pub(super) const UNUSED_REAP: Duration = Duration::from_mins(1);
/// A forward no read or write finished on for this long is closed.
pub(super) const IDLE_REAP: Duration = Duration::from_mins(5);

static NEXT_FORWARD: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct Slot {
    /// `host:port`, as the audit log names it.
    target: String,
    /// `None` while the forward dials or a read holds it.
    rd: Option<OwnedReadHalf>,
    /// `None` while the forward dials or a write holds it.
    wr: Option<OwnedWriteHalf>,
    /// The dial finished; the forward was announced in the audit log.
    open: bool,
    /// A read or write has finished on it at least once.
    used: bool,
    last_used: Instant,
    /// Cancelled by close; ends a read or write in flight.
    closed: CancellationToken,
    /// Bytes written to the target and read from it.
    sent: u64,
    received: u64,
    /// The client finished its side; further writes are refused.
    fin: bool,
}

impl Slot {
    fn reapable(&self, now: Instant) -> bool {
        let quiet = now.saturating_duration_since(self.last_used);
        self.open && quiet >= if self.used { IDLE_REAP } else { UNUSED_REAP }
    }

    /// End everything in flight and say what the forward carried; `None`
    /// for one that never finished dialing.
    fn end(self, id: u64) -> Option<Closed> {
        self.closed.cancel();
        self.open.then_some(Closed {
            id,
            target: self.target,
            sent: self.sent,
            received: self.received,
        })
    }
}

/// What a closed forward carried, for its `forward.close` audit record.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Closed {
    pub(super) id: u64,
    pub(super) target: String,
    pub(super) sent: u64,
    pub(super) received: u64,
}

#[derive(Debug, Default)]
struct Table {
    slots: HashMap<u64, Slot>,
    /// A reaper task is running for this table.
    reaping: Cell<bool>,
}

/// A connection's forward table. Cloning shares it.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "the connection dispatcher owns one per connection"
)]
pub(crate) struct Forwards(Rc<RefCell<Table>>);

impl Forwards {
    /// Reserve a slot for a forward about to dial `target`. Dropping the
    /// reservation unfulfilled frees the slot.
    pub(super) fn reserve(&self, target: String) -> Result<Reservation, CommandResult> {
        let slots = &mut self.0.borrow_mut().slots;
        if slots.len() >= MAX_FORWARDS {
            return Err(CommandResult::error(
                ErrorCode::ResourceExhausted,
                format!("this connection already holds {MAX_FORWARDS} forwards; close one"),
            ));
        }
        let id = NEXT_FORWARD.fetch_add(1, Ordering::Relaxed);
        slots.insert(
            id,
            Slot {
                target,
                rd: None,
                wr: None,
                open: false,
                used: false,
                last_used: Instant::now(),
                closed: CancellationToken::new(),
                sent: 0,
                received: 0,
                fin: false,
            },
        );
        Ok(Reservation {
            forwards: self.clone(),
            id,
            fulfilled: false,
        })
    }

    /// Lease `id`'s read half for one read.
    pub(super) fn lease_read(&self, id: u64) -> Result<Lease<OwnedReadHalf>, CommandResult> {
        self.lease(id, "read", |slot| slot.rd.take())
    }

    /// Lease `id`'s write half for one write.
    pub(super) fn lease_write(&self, id: u64) -> Result<Lease<OwnedWriteHalf>, CommandResult> {
        self.lease(
            id,
            "write",
            |slot| {
                if slot.fin { None } else { slot.wr.take() }
            },
        )
    }

    fn lease<H>(
        &self,
        id: u64,
        what: &str,
        take: impl FnOnce(&mut Slot) -> Option<H>,
    ) -> Result<Lease<H>, CommandResult> {
        let mut table = self.0.borrow_mut();
        let slot = table.slots.get_mut(&id).ok_or_else(|| not_open(id))?;
        let fin = slot.fin;
        let half = take(slot).ok_or_else(|| {
            if fin && what == "write" {
                return CommandResult::error(
                    ErrorCode::InvalidCommand,
                    format!("forward {id} already carried its last write"),
                );
            }
            CommandResult::error(
                ErrorCode::ResourceExhausted,
                format!("forward {id} is still dialing or already has a {what} in flight"),
            )
        })?;
        Ok(Lease {
            forwards: self.clone(),
            id,
            half,
            closed: slot.closed.clone(),
        })
    }

    /// Close `id`: its slot is gone at once and anything in flight ends.
    pub(super) fn close(&self, id: u64) -> Option<Closed> {
        let slot = self.0.borrow_mut().slots.remove(&id)?;
        slot.end(id)
    }

    /// Close every forward (the connection is ending).
    pub(super) fn close_all(&self) -> Vec<Closed> {
        let slots = std::mem::take(&mut self.0.borrow_mut().slots);
        slots
            .into_iter()
            .filter_map(|(id, slot)| slot.end(id))
            .collect()
    }

    /// Close every forward whose client went away (see the module docs).
    pub(super) fn reap(&self, now: Instant) -> Vec<Closed> {
        let slots = &mut self.0.borrow_mut().slots;
        slots
            .extract_if(|_, slot| slot.reapable(now))
            .filter_map(|(id, slot)| slot.end(id))
            .collect()
    }

    /// Whether nothing is dialing or open.
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

    /// Update `id`'s slot after a dial, read, or write. `false` when the
    /// slot was closed meanwhile, and whatever `update` owned is dropped.
    fn restore(&self, id: u64, update: impl FnOnce(&mut Slot)) -> bool {
        let mut table = self.0.borrow_mut();
        let Some(slot) = table.slots.get_mut(&id) else {
            return false;
        };
        update(slot);
        slot.last_used = Instant::now();
        true
    }
}

pub(super) fn not_open(id: u64) -> CommandResult {
    CommandResult::error(ErrorCode::NotFound, format!("no open forward {id}"))
}

/// A reserved slot, freed on drop unless fulfilled.
#[derive(Debug)]
pub(super) struct Reservation {
    forwards: Forwards,
    id: u64,
    fulfilled: bool,
}

impl Reservation {
    pub(super) const fn id(&self) -> u64 {
        self.id
    }

    /// Store the dialed connection. `false` when the forward was closed
    /// while it dialed; the connection is dropped.
    pub(super) fn fulfill(mut self, stream: TcpStream) -> bool {
        self.fulfilled = true;
        let (rd, wr) = stream.into_split();
        self.forwards.restore(self.id, |slot| {
            slot.rd = Some(rd);
            slot.wr = Some(wr);
            slot.open = true;
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.fulfilled {
            self.forwards.0.borrow_mut().slots.remove(&self.id);
        }
    }
}

/// One command's hold on a half of a forward. The command always runs to
/// its end (the close token or its own deadline ends it) and then finishes
/// the lease, so the half is never lost in between.
#[derive(Debug)]
pub(super) struct Lease<H> {
    forwards: Forwards,
    id: u64,
    pub(super) half: H,
    closed: CancellationToken,
}

impl<H> Lease<H> {
    pub(super) const fn id(&self) -> u64 {
        self.id
    }

    /// Cancelled when the forward is closed during the command.
    pub(super) fn closed(&self) -> CancellationToken {
        self.closed.clone()
    }

    /// The command failed on the target's side: the forward is over.
    pub(super) fn fail(self) -> Option<Closed> {
        self.forwards.close(self.id)
    }
}

impl Lease<OwnedReadHalf> {
    /// End a read that returned `received` bytes.
    pub(super) fn finish(self, received: usize) {
        let Self {
            forwards, id, half, ..
        } = self;
        forwards.restore(id, |slot| {
            slot.rd = Some(half);
            slot.received += received as u64;
            slot.used = true;
        });
    }
}

impl Lease<OwnedWriteHalf> {
    /// End a write that sent `sent` bytes and, with `fin`, the last ones.
    pub(super) fn finish(self, sent: usize, fin: bool) {
        let Self {
            forwards, id, half, ..
        } = self;
        forwards.restore(id, |slot| {
            slot.wr = Some(half);
            slot.sent += sent as u64;
            slot.fin |= fin;
            slot.used = true;
        });
    }
}
