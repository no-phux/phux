use std::path::Path;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{
    AuthorityId, OperationId, ResourceId, ResourceKey, ResourceKind, SessionKey,
};
use phux_protocol::wire::frame::{
    ArrangementNode, ArrangementOwner, Command, Committed, CreatePlacement, EntryTarget, ExecState,
    ExitPolicy, FrameKind, LaunchMode, LaunchSpec, OrgFilter, Refusal, ResourceRecord, SpawnResult,
    SplitDirection,
};

use crate::agent_session_record::AgentSessionRecord;
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::contract::{self, codes};
use crate::layout::SplitDir;
use crate::org::{self, OrgError, OrgInventory};
use crate::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
use crate::selector::{self, AgentResolveError, Selector};
use crate::state::{self, Degradation};

/// Launch intent for the compatibility `spawn --target` and `launch --target` verbs.
#[allow(
    missing_debug_implementations,
    reason = "launch intent can contain credentials"
)]
pub struct PlacedSpawn {
    launch: LaunchSpec,
    operation: OperationId,
    dir: SplitDirection,
    ratio: f32,
}

/// Why placed spawn could not return a live Terminal handle.
#[derive(Debug, thiserror::Error)]
pub enum PlacedSpawnError {
    /// Unsupported or invalid legacy request, before sending any mutation.
    #[error("{0}")]
    Invalid(&'static str),
    /// A transport or decode failure before the mutation.
    #[error(transparent)]
    Attach(#[from] AttachError),
    /// A named agent did not resolve.
    #[error(transparent)]
    Agent(#[from] AgentResolveError),
    /// The selector matched nothing in this view.
    #[error("no such target")]
    Miss(Degradation),
    /// The selector names a session whose panes wait for `phux restore`.
    #[error("session {0} is waiting for restore")]
    AwaitingRestore(String),
    /// This server cannot place a Terminal beside another authority's pane.
    #[error("explicit spawn placement is local to the executing authority")]
    Satellite,
    /// Organization reads or a known refusal.
    #[error(transparent)]
    Organization(#[from] OrgError),
    /// The server may have committed; retain the original receipt lookup identity.
    #[error("placed spawn outcome unknown; reconcile operation {key} in session {session}", key = .operation.to_hex())]
    Unknown {
        /// Authority that received the request.
        authority: AuthorityId,
        /// Session used for receipt lookup.
        session: SessionKey,
        /// Exact mutation identity.
        operation: OperationId,
    },
    /// Membership and placement committed, but no usable live handle was reported.
    #[error("placed spawn committed, but {reason}; reconcile operation {key} in session {session}", key = .operation.to_hex())]
    NoHandle {
        /// Authority whose arrangement was changed.
        authority: AuthorityId,
        /// Session used for receipt lookup.
        session: SessionKey,
        /// Exact mutation identity.
        operation: OperationId,
        /// Why the receipt could not yield a live pane.
        reason: &'static str,
    },
}

impl PlacedSpawn {
    /// Adapt the launch options shared with the original unplaced spawn.
    ///
    /// # Errors
    /// Refuses named projections and timed retention rather than pretending
    /// that the canonical arrangement or a durable exit result implements them.
    pub fn from_spawn_frame(
        frame: FrameKind,
        dir: SplitDir,
        ratio: f32,
        projection: Option<&str>,
        provenance: Option<&AgentSessionRecord>,
    ) -> Result<Self, PlacedSpawnError> {
        if projection.is_some() {
            return Err(PlacedSpawnError::Invalid(
                "placed spawn does not support named projections",
            ));
        }
        let FrameKind::SpawnResource {
            command,
            cwd,
            env,
            term: None,
            agent_session,
            resource,
            satellite: None,
            owner_terminal: None,
            initial_size: None,
            ..
        } = frame
        else {
            return Err(PlacedSpawnError::Invalid("expected a Terminal spawn"));
        };
        if resource.as_ref().is_some_and(|r| r.retain_secs.is_some()) {
            return Err(PlacedSpawnError::Invalid(
                "placed spawn does not support timed screen retention",
            ));
        }
        if resource.as_ref().is_some_and(|r| {
            r.kind != ResourceKind::Terminal || r.parent.is_some() || r.bind_instance
        }) {
            return Err(PlacedSpawnError::Invalid(
                "legacy resource fields are not supported for placed spawn",
            ));
        }
        if !(0.0 < ratio && ratio < 1.0) {
            return Err(PlacedSpawnError::Invalid(
                "split ratio must be strictly between 0 and 1",
            ));
        }
        let launch = launch_spec(command, cwd, env, agent_session, provenance);
        let operation = resource
            .and_then(|r| r.idempotency_key)
            .unwrap_or_else(org::mint_operation);
        Ok(Self {
            launch,
            operation,
            dir: match dir {
                SplitDir::Horizontal => SplitDirection::Horizontal,
                SplitDir::Vertical => SplitDirection::Vertical,
            },
            ratio,
        })
    }

    /// Resolve the anchor on the same connection that commits the creation.
    ///
    /// # Errors
    /// Refuses unsupported servers, stale/missing anchors, and reports unknown
    /// commit outcomes without resubmitting, publishing metadata, or killing.
    pub async fn run(
        self,
        socket: &Path,
        selector: &Selector,
        request_id: u32,
    ) -> Result<(SpawnResult, Degradation), PlacedSpawnError> {
        let mut conn = Connection::connect(socket).await?;
        let (live, degradation) = resolve_local(&mut conn, selector).await?;
        if !conn
            .server_features_ext()
            .contains(ServerFeatureExt::CreatePlacement)
        {
            return Err(OrgError::Unsupported.into());
        }
        let inventory = org::fetch(&mut conn, OrgFilter::All, 0x5300)
            .await
            .map_err(read_error)?;
        let record = inventory
            .resource_by_live(&live)
            .ok_or(OrgError::Refused(Refusal::Stale { current: None }))?;
        let placement = self
            .beside(&mut conn, &inventory, record)
            .await
            .map_err(read_error)?;
        let result = self
            .commit(
                &mut conn,
                request_id,
                inventory.authority,
                record.session,
                placement,
            )
            .await?;
        Ok((result, degradation.and(inventory.degradation)))
    }

    async fn beside(
        &self,
        conn: &mut Connection,
        inventory: &OrgInventory,
        record: &ResourceRecord,
    ) -> Result<CreatePlacement, OrgError> {
        let target =
            org::live_target(record).ok_or(OrgError::Refused(Refusal::Stale { current: None }))?;
        let session = record.session;
        let key = inventory
            .session(&session)
            .and_then(|s| s.arrangement)
            .ok_or_else(|| OrgError::NotFound("target session arrangement".into()))?;
        let layout = org::arrangement(conn, 0x5400, key).await?;
        if layout.key != key || layout.owner != ArrangementOwner::Session(session) {
            return Err(OrgError::Unexpected(
                "arrangement did not match the target session".into(),
            ));
        }
        Ok(CreatePlacement::Beside {
            entry: anchor_entry(&layout.document, record.key)?,
            resource: record.key,
            instance: target.instance,
            activation: target.activation,
            dir: self.dir,
            ratio: self.ratio,
        })
    }

    async fn commit(
        self,
        conn: &mut Connection,
        request_id: u32,
        authority: AuthorityId,
        session: SessionKey,
        placement: CreatePlacement,
    ) -> Result<SpawnResult, PlacedSpawnError> {
        let operation = self.operation;
        let command = Command::ResourceCreate {
            operation,
            session,
            kind: ResourceKind::Terminal,
            launch: Some(Box::new(self.launch)),
            name: None,
            on_exit: ExitPolicy::Remove,
            launch_mode: LaunchMode::Immediate,
            placement: Some(Box::new(placement)),
        };
        match org::commit(conn, request_id, command).await {
            Ok(receipt) if receipt.authority == Some(authority) => {
                legacy_result(receipt, authority, session)
            }
            Ok(_)
            | Err(
                OrgError::OutcomeUnknown { .. } | OrgError::Unexpected(_) | OrgError::Server { .. },
            ) => Err(PlacedSpawnError::Unknown {
                authority,
                session,
                operation,
            }),
            Err(err) => Err(err.into()),
        }
    }
}

fn read_error(error: OrgError) -> PlacedSpawnError {
    match error {
        OrgError::Attach(error) => error.into(),
        // The read failed before sending any creation intent.
        OrgError::OutcomeUnknown { .. } => AttachError::Disconnected.into(),
        other => other.into(),
    }
}

async fn resolve_local(
    conn: &mut Connection,
    selector: &Selector,
) -> Result<(ResourceId, Degradation), PlacedSpawnError> {
    let (snapshot, degradation) = state::get_state_on(conn).await?.into_parts();
    let candidates = state::resolve_targets_on(conn, selector, &snapshot).await?;
    match selector::pick_target_pane(&candidates, &snapshot.focused_resource) {
        Some(id @ ResourceId::Local { .. }) => Ok((id, degradation)),
        Some(_) => Err(PlacedSpawnError::Satellite),
        None => Err(awaiting_restore(conn, selector).await.map_or(
            PlacedSpawnError::Miss(degradation),
            PlacedSpawnError::AwaitingRestore,
        )),
    }
}

/// The session a missed `selector` names, when it is waiting for restore.
async fn awaiting_restore(conn: &mut Connection, selector: &Selector) -> Option<String> {
    let inventory = org::fetch(conn, OrgFilter::All, 0x5310).await.ok()?;
    inventory.awaiting_restore(selector).map(str::to_owned)
}

// Native provider env keys are validated integration inputs; these belong to
// the launched program, not to the daemon's PHUX_* environment. Pass the one
// provenance-matching value as argv to env, not as a server environment override.
fn launch_spec(
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
    restore: Option<Vec<u8>>,
    provenance: Option<&AgentSessionRecord>,
) -> LaunchSpec {
    let mut argv = command.unwrap_or_default();
    let mut env = env.unwrap_or_default();
    if let Some(record) = provenance
        && let Some(index) = env.iter().position(|(key, value)| {
            key.starts_with("PHUX_")
                && key.ends_with("_SESSION_ID")
                && !key.contains('=')
                && *value == record.native_id
        })
    {
        let (key, value) = env.remove(index);
        argv.splice(..0, ["/usr/bin/env".to_owned(), format!("{key}={value}")]);
    }
    LaunchSpec::new(argv)
        .with_cwd(cwd)
        .with_env(env)
        .with_restore(restore)
}

fn anchor_entry(
    layout: &phux_protocol::wire::frame::ArrangementDoc,
    resource: ResourceKey,
) -> Result<[u8; 16], OrgError> {
    fn visit(tree: &ArrangementNode, target: &EntryTarget, entries: &mut Vec<[u8; 16]>) {
        match tree {
            ArrangementNode::Leaf(entry) if entry.target == *target => entries.push(entry.entry),
            ArrangementNode::Leaf(_) => (),
            ArrangementNode::Split { first, second, .. } => {
                visit(first, target, entries);
                visit(second, target, entries);
            }
        }
    }
    let mut entries = Vec::new();
    for group in &layout.groups {
        visit(&group.tree, &EntryTarget::Member(resource), &mut entries);
    }
    match entries.as_slice() {
        [entry] => Ok(*entry),
        [] => Err(OrgError::NotFound(
            "target is not a split leaf in the shared arrangement".into(),
        )),
        _ => Err(OrgError::Ambiguous {
            prefix: resource.to_string(),
            candidates: entries.iter().map(phux_protocol::api::hex16).collect(),
        }),
    }
}

fn legacy_result(
    receipt: Committed,
    authority: AuthorityId,
    session: SessionKey,
) -> Result<SpawnResult, PlacedSpawnError> {
    if receipt.resource.is_none() || receipt.arrangement.is_none() || receipt.entry.is_none() {
        return Err(PlacedSpawnError::NoHandle {
            authority,
            session,
            operation: receipt.operation,
            reason: "the receipt did not identify the pane and its placement",
        });
    }
    let target = receipt
        .target
        .filter(|target| target.resource.local_id().is_some())
        .ok_or_else(|| PlacedSpawnError::NoHandle {
            authority,
            session,
            operation: receipt.operation,
            reason: if receipt
                .activation
                .is_some_and(|a| a.state == ExecState::Failed)
            {
                "the terminal could not start"
            } else {
                "its first execution has no live terminal handle"
            },
        })?;
    if receipt.replayed {
        Ok(SpawnResult::Replayed {
            id: target.resource,
            instance: Some(target.instance),
        })
    } else {
        Ok(SpawnResult::Ok(target.resource))
    }
}

impl PlacedSpawnError {
    /// Project placement failures consistently on the CLI and MCP surfaces.
    #[must_use]
    pub fn refusal(&self, target: &str) -> (contract::Refusal, u8) {
        if let Self::AwaitingRestore(session) = self {
            return (contract::awaiting_restore(target, session), 1);
        }
        let (code, remedy, exit) = match self {
            Self::Invalid(_) => (
                codes::USAGE_ERROR,
                "omit that option or spawn without a target",
                2,
            ),
            Self::Attach(_) => (codes::TRANSPORT, "check the selected server", 1),
            Self::Agent(err) => (err.code(), err.remedy(), err.exit_code()),
            Self::Miss(view) if !view.is_complete() => (
                codes::PARTIAL_VIEW,
                "check the server and its satellite connections",
                3,
            ),
            // `AwaitingRestore` returned above with its own refusal.
            Self::Miss(_) | Self::AwaitingRestore(_) => (
                codes::NO_SUCH_TARGET,
                "run `phux ls` to see live sessions and panes",
                1,
            ),
            Self::Satellite => (
                codes::SATELLITE_TARGET,
                "run the verb against the authority that owns the target",
                1,
            ),
            Self::Organization(OrgError::Unsupported) => (
                codes::UNSUPPORTED_SERVER,
                "use a server that advertises CREATE_PLACEMENT; no legacy fallback was attempted",
                1,
            ),
            Self::Organization(OrgError::Refused(Refusal::DuplicateOperation)) => (
                codes::IDEMPOTENCY_CONFLICT,
                "use a fresh key only for a new intention",
                2,
            ),
            Self::Organization(err) => (
                err.code(),
                "inspect the current arrangement with `phux ls` before retrying",
                1,
            ),
            Self::Unknown { .. } => (
                codes::SPAWN_OUTCOME_UNKNOWN,
                "look up the original receipt with `phux terminal operation` or inspect the current arrangement; do not blindly retry",
                1,
            ),
            Self::NoHandle { .. } => (
                codes::SPAWN_FAILED,
                "inspect the original receipt with `phux terminal operation`; do not blindly retry",
                1,
            ),
        };
        let message = match self {
            Self::Miss(view) if !view.is_complete() => format!(
                "could not resolve {target}: this server's view of the fleet is incomplete ({})",
                view.notices().join("; ")
            ),
            Self::Miss(_) => format!("no such target: {target}"),
            Self::Organization(OrgError::Unsupported) => {
                "this server does not support atomic placement".to_owned()
            }
            _ => self.to_string(),
        };
        let mut error = contract::Refusal::new(code, message, remedy);
        let recovery = match self {
            Self::Unknown {
                authority,
                operation,
                ..
            } => Some((authority, operation, EffectCertainty::Unknown)),
            Self::NoHandle {
                authority,
                operation,
                ..
            } => Some((authority, operation, EffectCertainty::Applied)),
            _ => None,
        };
        if let Some((authority, operation, certainty)) = recovery {
            error = error.with_recovery(
                Recovery::new(
                    ScopeKind::OrganizationAuthority,
                    Some(operation.to_hex()),
                    certainty,
                    NextAction::Reconcile,
                )
                .with_scope_id(Some(&authority.to_string())),
            );
        }
        (error, exit)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture construction")]
mod tests {
    use super::*;

    #[test]
    fn agent_launch_keeps_real_provenance_and_native_provider_env() {
        let record = AgentSessionRecord::new("tools", "claude", "native-42").unwrap();
        let encoded = record.encode().unwrap();
        let launch = launch_spec(
            Some(vec!["agent".into(), "--resume".into(), "native-42".into()]),
            Some("/workspace".into()),
            Some(vec![
                ("PHUX_CLAUDE_SESSION_ID".into(), "native-42".into()),
                ("CARGO_TARGET_DIR".into(), "/build".into()),
            ]),
            Some(encoded.clone()),
            Some(&record),
        );
        assert_eq!(
            launch.command,
            [
                "/usr/bin/env",
                "PHUX_CLAUDE_SESSION_ID=native-42",
                "agent",
                "--resume",
                "native-42",
            ]
        );
        assert_eq!(
            launch.env,
            [("CARGO_TARGET_DIR".to_owned(), "/build".to_owned())]
        );
        assert_eq!(launch.cwd.as_deref(), Some("/workspace"));
        assert_eq!(launch.restore, Some(encoded));
    }
}
