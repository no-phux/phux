//! The organization store at automation scale (CONTRACTS §08): 5,000
//! resources in 200 sessions plus 500 in Intake, a full receipt window,
//! a real SQLite file. Pages and batches stay bounded, a commit writes only
//! the pages it touches, nothing is dropped, and receipt churn stays at the
//! cap while observers follow every event.
//!
//! Wall-clock commit latency is printed for triage; the gate is the
//! `org_commit` instruction count in `benches/icount.rs`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    reason = "scale test prints its measurements"
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use phux_core::{Actor, Command as CoreCommand, Op, OperationId, SessionKey, SourcePin};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, OrgFilter, OrganizationPage,
};
use rusqlite::Connection;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::organization::{MAX_PAGE_SIZE, handle};
use crate::org_scale::{ScaleSpec, members, seed};
use crate::org_store::RECEIPT_CAP;
use crate::state::{ClientId, Outbound, ServerState, SharedState};

const CLIENT: ClientId = ClientId(3);

/// Most WAL pages one single-record commit may write (measured 19). A
/// full-store rewrite at this scale is thousands of pages.
const PAGES_PER_COMMIT: i64 = 32;

/// Most WAL pages a 256-resource batch may write (measured 63).
const PAGES_PER_BATCH: i64 = 128;

fn page(
    state: &SharedState,
    filter: OrgFilter,
    size: u32,
    cursor: Option<Vec<u8>>,
) -> OrganizationPage {
    let command = Command::GetOrganization {
        filter,
        page_size: size,
        cursor,
    };
    match handle(state, CLIENT, command, &CancellationToken::new()) {
        CommandResult::OkWith(CommandValue::OrganizationPage(p)) => *p,
        other => panic!("expected a page, got {other:?}"),
    }
}

/// Every page of `filter`, checking each against `bound` records.
fn all_pages(
    state: &SharedState,
    filter: OrgFilter,
    size: u32,
    bound: usize,
) -> Vec<OrganizationPage> {
    let mut pages = Vec::new();
    let mut cursor = None;
    loop {
        let p = page(state, filter, size, cursor.take());
        let records = p.sessions.len() + p.resources.len();
        assert!(records <= bound, "page of {records} records over {bound}");
        cursor = p.next_cursor.clone();
        pages.push(p);
        if cursor.is_none() {
            return pages;
        }
    }
}

/// WAL frames the last commit appended, read by checkpointing from a side
/// connection: after a complete checkpoint the next writer restarts the
/// log, so the log length is one commit's pages.
fn wal_pages(side: &Connection) -> i64 {
    side.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| row.get(1))
        .unwrap()
}

fn count(side: &Connection, table: &str) -> usize {
    let n: i64 = side
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap();
    usize::try_from(n).unwrap()
}

struct Probe {
    side: Connection,
    latencies: Vec<Duration>,
    max_pages: i64,
}

impl Probe {
    fn new(path: &Path) -> Self {
        let side = Connection::open(path).unwrap();
        let _ = wal_pages(&side);
        Self {
            side,
            latencies: Vec::new(),
            max_pages: 0,
        }
    }

    fn commit(&mut self, s: &mut ServerState, operation: OperationId, op: Op) {
        let started = Instant::now();
        s.commit_command(&CoreCommand { operation, op }, &Actor::owner())
            .unwrap();
        self.latencies.push(started.elapsed());
        self.max_pages = self.max_pages.max(wal_pages(&self.side));
    }

    fn report(&mut self, what: &str, pages: i64) {
        self.latencies.sort_unstable();
        let at = |q: usize| self.latencies[(self.latencies.len() - 1) * q / 100];
        eprintln!(
            "org scale {what}: {} commits p50 {:?} p99 {:?} max {} WAL pages",
            self.latencies.len(),
            at(50),
            at(99),
            self.max_pages,
        );
        assert!(
            self.max_pages <= pages,
            "{what}: a commit wrote {} pages",
            self.max_pages
        );
        self.latencies.clear();
        self.max_pages = 0;
    }
}

fn rev(s: &ServerState, session: SessionKey) -> phux_core::Revision {
    s.org().session(&session).unwrap().revision
}

fn op_id(n: u32) -> OperationId {
    let mut bytes = [0x5c; 16];
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    OperationId::from_bytes(bytes)
}

fn drain(rx: &mut mpsc::Receiver<Outbound>, seen: &mut Vec<u64>) {
    while let Ok(out) = rx.try_recv() {
        if let Outbound::Frame(FrameKind::Event {
            stamp: Some(stamp), ..
        }) = out
        {
            seen.push(stamp.seq);
        }
    }
}

#[test]
fn the_org_store_holds_the_scale_contract() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("org").join("v1.sqlite3");
    let spec = ScaleSpec::CONTRACT;
    let started = Instant::now();
    let scale = seed(&path, spec, crate::state::now_stamp()).unwrap();
    eprintln!("org scale seed: {:?}", started.elapsed());
    let (sessions, inbox) = (scale.sessions.clone(), scale.inbox);
    let state = SharedState::new();
    state.with_mut(|s| *s = scale.state);
    let mut probe = Probe::new(&path);
    let total_pages: i64 = probe
        .side
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    assert!(
        total_pages > 10 * PAGES_PER_BATCH,
        "store is {total_pages} pages"
    );
    eprintln!("org scale store: {total_pages} pages");

    // Paged inventory: every record exactly once, every page bounded.
    let started = Instant::now();
    let pages = all_pages(&state, OrgFilter::All, 0, 100);
    eprintln!(
        "org scale inventory: {} pages in {:?}",
        pages.len(),
        started.elapsed()
    );
    let mut seen_sessions = BTreeSet::new();
    let mut seen_resources = BTreeSet::new();
    for p in &pages {
        for s in &p.sessions {
            assert!(seen_sessions.insert(s.key), "session twice");
        }
        for r in &p.resources {
            assert!(seen_resources.insert(r.key), "resource twice");
        }
    }
    let records = spec.resources + spec.inbox_resources;
    assert_eq!(seen_sessions.len(), spec.sessions + 1);
    assert_eq!(seen_resources.len(), records);
    let first = &pages[0];
    assert_eq!(first.total_sessions as usize, spec.sessions + 1);
    assert_eq!(first.total_resources as usize, records);

    // Intake at the largest page.
    let intake = all_pages(
        &state,
        OrgFilter::Intake,
        MAX_PAGE_SIZE,
        MAX_PAGE_SIZE as usize,
    );
    let intake_resources: usize = intake.iter().map(|p| p.resources.len()).sum();
    assert_eq!(intake_resources, spec.inbox_resources);

    // Concurrent observers follow every commit below.
    let mut observers: Vec<_> = (0..3)
        .map(|_| {
            let (tx, rx) = mpsc::channel(4096);
            let client = state.with_mut(|s| {
                let client = s.new_client_id();
                s.subscribe_events(client, None, tx);
                client
            });
            (client, rx, Vec::new())
        })
        .collect();
    let journal_start = state.with(ServerState::journal_head);
    let mut next_op = 0_u32;
    let mut fresh = || {
        next_op += 1;
        op_id(next_op)
    };

    // Single moves between ordinary sessions.
    state.with_mut(|s| {
        for n in 0..200 {
            let (from, to) = (sessions[n], sessions[(n + 1) % sessions.len()]);
            let resource = members(s, from)[0];
            let op = Op::ResourceMove {
                resource,
                from,
                from_expected: rev(s, from),
                to,
                to_expected: rev(s, to),
            };
            probe.commit(s, fresh(), op);
        }
    });
    probe.report("move", PAGES_PER_COMMIT);

    // A bounded batch out of Intake, then its undo.
    let batch = phux_core::command::MAX_MOVE_BATCH;
    let move_many = fresh();
    state.with_mut(|s| {
        let picked: Vec<_> = members(s, inbox).into_iter().take(batch).collect();
        let op = Op::ResourceMoveMany {
            resources: picked,
            sources: vec![SourcePin {
                session: inbox,
                expected: rev(s, inbox),
            }],
            to: sessions[0],
            to_expected: rev(s, sessions[0]),
        };
        probe.commit(s, move_many, op);
        assert_eq!(members(s, inbox).len(), spec.inbox_resources - batch);
        let op = Op::OrganizationUndo {
            original: move_many,
            expected: Vec::new(),
        };
        probe.commit(s, fresh(), op);
        assert_eq!(members(s, inbox).len(), spec.inbox_resources);
        let refused = s.commit_command(
            &CoreCommand {
                operation: fresh(),
                op: Op::ResourceMoveMany {
                    resources: members(s, inbox).into_iter().take(batch + 1).collect(),
                    sources: Vec::new(),
                    to: sessions[0],
                    to_expected: rev(s, sessions[0]),
                },
            },
            &Actor::owner(),
        );
        assert!(refused.is_err(), "a batch over the bound is refused");
    });
    probe.report("move-many+undo", PAGES_PER_BATCH);

    // Receipt churn: sustained renames past the receipt cap.
    for n in 0..1_500 {
        state.with_mut(|s| {
            let session = sessions[n % sessions.len()];
            let op = Op::SessionRename {
                session,
                name: format!("churn-{n:05}"),
            };
            probe.commit(s, fresh(), op);
            assert!(s.org().receipts().count() <= RECEIPT_CAP);
            for (_, rx, seen) in &mut observers {
                drain(rx, seen);
            }
        });
    }
    probe.report("rename churn", PAGES_PER_COMMIT);
    assert_eq!(count(&probe.side, "receipts"), RECEIPT_CAP);
    state.with(|s| assert_eq!(s.org().receipts().count(), RECEIPT_CAP));

    // Nothing dropped: the store holds every record, and every observer
    // saw every journaled event in order.
    assert_eq!(count(&probe.side, "resources"), records);
    assert_eq!(count(&probe.side, "sessions"), spec.sessions + 1);
    let head = state.with(ServerState::journal_head);
    let expected: Vec<u64> = (journal_start + 1..=head).collect();
    for (client, rx, seen) in &mut observers {
        drain(rx, seen);
        assert_eq!(*seen, expected, "observer {client:?} missed events");
    }
}
