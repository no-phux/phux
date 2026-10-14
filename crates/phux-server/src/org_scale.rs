//! A seeded organization at automation scale: thousands of resource
//! records across sessions, a full receipt window, all in a real
//! [`SqliteOrgStore`] behind a [`ServerState`].
//!
//! The org-store scale test and the `icount` commit scenarios both start
//! from [`seed`], so the gate and the proof measure the same shape
//! (CONTRACTS §08 "Scale contract").

use std::path::Path;

use phux_core::{
    Actor, Attrs, AuthorityId, Command, ExitPolicy, FreshKeys as _, InstanceId, LaunchMode,
    LaunchSpec, NewResource, Op, OperationId, Org, Receipt, Registry, ResourceKey, ResourceKind,
    SequentialKeys, SessionKey, SessionLifetime, Stamp, TerminalLaunch,
};

use crate::org_store::{OrgStore as _, SqliteOrgStore, StoreError};
use crate::state::ServerState;

/// The shape [`seed`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaleSpec {
    /// Ordinary sessions besides Intake.
    pub sessions: usize,
    /// Resources spread round-robin over those sessions.
    pub resources: usize,
    /// Resources left in Intake, awaiting organization (legacy fixture field name).
    pub inbox_resources: usize,
    /// Stored receipts after seeding, at least one per seeded record. The
    /// extra ones are spread over the last day, a few already past the
    /// horizon, so later commits cross both the horizon and the cap.
    pub receipts: usize,
}

impl ScaleSpec {
    /// The scale the contract names: 5,000 resources in 200 sessions, 500
    /// more in Intake, and a receipt window at its cap.
    pub const CONTRACT: Self = Self {
        sessions: 200,
        resources: 5_000,
        inbox_resources: 500,
        receipts: crate::org_store::RECEIPT_CAP,
    };
}

/// A seeded server and the keys a caller needs to drive it.
#[derive(Debug)]
pub struct ScaleOrg {
    /// The server, with the store installed as already loaded.
    pub state: ServerState,
    /// The ordinary sessions, in creation order.
    pub sessions: Vec<SessionKey>,
    /// The designated Intake (legacy fixture field name).
    pub inbox: SessionKey,
}

fn dormant() -> NewResource {
    NewResource {
        kind: ResourceKind::Terminal,
        name: None,
        launch: Some(LaunchSpec::Terminal(TerminalLaunch::default())),
        on_exit: ExitPolicy::Retain,
        parent: None,
        launch_mode: LaunchMode::Deferred,
    }
}

fn run(
    registry: &mut Registry,
    keys: &mut SequentialKeys,
    op: Op,
    at: Stamp,
) -> Result<(), StoreError> {
    let cmd = Command {
        operation: OperationId::from_bytes(keys.fresh()),
        op,
    };
    registry
        .execute(&cmd, &Actor::owner(), at, keys)
        .map(|_| ())
        .map_err(|refusal| StoreError::Corrupt(format!("seed refused: {refusal}")))
}

/// Seed `spec` at `now` into a fresh store at `path`, installed in a new
/// [`ServerState`].
///
/// Resources are dormant (`LaunchMode::Deferred`), so nothing spawns. Keys
/// are deterministic.
///
/// # Errors
///
/// [`StoreError`] when the store cannot be written, or a seeding command
/// is refused.
pub fn seed(path: &Path, spec: ScaleSpec, now: Stamp) -> Result<ScaleOrg, StoreError> {
    let mut keys = SequentialKeys::new(*b"orgscale");
    let inbox = SessionKey::from_bytes(keys.fresh());
    let mut org = Org::new(
        AuthorityId::from_bytes(keys.fresh()),
        inbox,
        InstanceId::from_bytes(keys.fresh()),
        now,
    );
    org.ensure_arrangements(&mut keys);
    let mut registry = Registry::new(org);
    // Seeded commands land well inside the receipt horizon.
    let seeded_at = Stamp(
        now.as_millis()
            .saturating_sub(crate::org_store::RECEIPT_HORIZON_MS / 2),
    );
    let mut sessions = Vec::with_capacity(spec.sessions);
    for n in 0..spec.sessions {
        run(
            &mut registry,
            &mut keys,
            Op::SessionCreate {
                name: format!("scale-{n:04}"),
                lifetime: SessionLifetime::Persistent,
                attrs: Attrs::default(),
                resource: None,
            },
            seeded_at,
        )?;
        let key = registry
            .org()
            .session_by_name(&format!("scale-{n:04}"))
            .map(|s| s.key)
            .ok_or_else(|| StoreError::Corrupt("seeded session missing".to_owned()))?;
        sessions.push(key);
    }
    for n in 0..spec.resources {
        let session = sessions.get(n % spec.sessions.max(1)).copied();
        run(
            &mut registry,
            &mut keys,
            Op::ResourceCreate {
                session: session.or(Some(inbox)),
                resource: dormant(),
            },
            seeded_at,
        )?;
    }
    for _ in 0..spec.inbox_resources {
        run(
            &mut registry,
            &mut keys,
            Op::ResourceCreate {
                session: None,
                resource: dormant(),
            },
            seeded_at,
        )?;
    }
    let org = pad_receipts(registry.org(), &mut keys, spec.receipts, now)?;
    let mut store = SqliteOrgStore::open(path)?;
    store.import(&org, None, now)?;
    let mut state = ServerState::new();
    state.install_org(Box::new(store), org, true);
    Ok(ScaleOrg {
        state,
        sessions,
        inbox,
    })
}

/// `org` with synthetic receipts added until it holds `target`, stamped
/// evenly over the day before `now`.
fn pad_receipts(
    org: &Org,
    keys: &mut SequentialKeys,
    target: usize,
    now: Stamp,
) -> Result<Org, StoreError> {
    let mut receipts: Vec<Receipt> = org.receipts().cloned().collect();
    let template = receipts
        .first()
        .cloned()
        .ok_or_else(|| StoreError::Corrupt("no receipt to pad from".to_owned()))?;
    let extra = target.saturating_sub(receipts.len());
    // The oldest few sit just past the horizon, so the first commit
    // prunes by age as well as by count.
    let span = crate::org_store::RECEIPT_HORIZON_MS;
    let oldest = now.as_millis().saturating_sub(span + 60_000);
    for n in 0..extra {
        let step = span / u64::try_from(extra.max(1)).unwrap_or(1);
        receipts.push(Receipt {
            operation: OperationId::from_bytes(keys.fresh()),
            at: Stamp(oldest + step * u64::try_from(n).unwrap_or(0)),
            ..template.clone()
        });
    }
    Org::restore(
        *org.authority(),
        org.instance(),
        org.sessions().cloned(),
        org.resources().cloned(),
        org.arrangements().cloned(),
        receipts,
    )
    .map_err(|violation| StoreError::Corrupt(violation.to_string()))
}

/// The resource keys of `session`, in membership order.
#[must_use]
pub fn members(state: &ServerState, session: SessionKey) -> Vec<ResourceKey> {
    state
        .org()
        .session(&session)
        .map(|s| s.members.clone())
        .unwrap_or_default()
}
