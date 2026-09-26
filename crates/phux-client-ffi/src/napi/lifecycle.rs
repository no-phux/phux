//! Explicit, incarnation-qualified terminal lifecycle commands. Validation and
//! dispatch share one control-plane lock, so reconnect cannot retarget a
//! numeric session or resource between checking it and queueing the command.
//!
//! Integration: retire the legacy unchecked `spawnTerminal(sessionId)` in
//! `mod.rs` when wiring this module. Desktop callers use the qualified options
//! surface here. An empty-session create needs a runtime-owned operation:
//! nonce-bound `SESSION_CREATE_KEY` set, exact receipt read, ordered state
//! confirmation, and Created/Refused/Unknown settlement on close/reconnect.
//! The existing C ABI owns that state privately in `c/session_create.rs`;
//! duplicating it here would introduce a second lifecycle coordinator.
//!
//! Only the attached home session can be addressed exactly by today's spawn
//! protocol. An owner pane can move to another session before the server runs
//! a foreign spawn. Satellite termination also needs a stronger runtime seam:
//! the hub identity does not fence a satellite daemon's reused resource IDs.

use ::napi::{Error, Result};
use napi_derive::napi;
use phux_client_runtime::control::{ControlPlane, SpawnRequest, Status, Topology};
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::RolePolicy;

use super::connection::{DesktopConnectionIdentity, require_identity};
use super::{DesktopClient, dimension, protocol_integer, terminal_id};

// Match the existing C encoder's admission bounds. These protect the binding
// before allocating an encoded command, not the server's authorization policy.
const MAX_ARGS: usize = 256;
const MAX_TEXT: usize = 64 * 1024;

#[napi(object)]
#[derive(Debug)]
pub struct DesktopEnvironmentEntry {
    pub name: String,
    pub value: String,
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopInitialSize {
    pub cols: f64,
    pub rows: f64,
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopSpawnOptions {
    pub identity: DesktopConnectionIdentity,
    pub session_id: f64,
    /// None uses the server default shell. Some(argv) is passed unchanged,
    /// without shell evaluation; an empty argv/executable is invalid.
    pub command: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Option<Vec<DesktopEnvironmentEntry>>,
    pub initial_size: Option<DesktopInitialSize>,
}

impl DesktopSpawnOptions {
    fn into_request(self) -> Result<(DesktopConnectionIdentity, SpawnRequest)> {
        let session_id = protocol_integer(self.session_id, u32::MAX, "InvalidSessionId")?;
        let initial_size = self
            .initial_size
            .map(|size| -> Result<_> { Ok((dimension(size.cols)?, dimension(size.rows)?)) })
            .transpose()?;
        validate_text(
            self.command.as_deref(),
            self.cwd.as_deref(),
            self.env.as_deref(),
        )?;
        Ok((
            self.identity,
            SpawnRequest {
                command: self.command,
                cwd: self.cwd,
                env: self.env.map(|entries| {
                    entries
                        .into_iter()
                        .map(|entry| (entry.name, entry.value))
                        .collect()
                }),
                session_id: Some(session_id),
                initial_size,
            },
        ))
    }
}

fn count_text(total: &mut usize, text: &str) -> Result<()> {
    if text.contains('\0') || text.len() > MAX_TEXT.saturating_sub(*total) {
        return Err(Error::from_reason("InvalidSpawnText"));
    }
    *total += text.len();
    Ok(())
}

fn validate_command(command: Option<&[String]>, total: &mut usize) -> Result<()> {
    let Some(argv) = command else { return Ok(()) };
    if argv.is_empty() || argv.len() > MAX_ARGS || argv[0].is_empty() {
        return Err(Error::from_reason("InvalidSpawnCommand"));
    }
    for arg in argv {
        count_text(total, arg)?;
    }
    Ok(())
}

fn validate_environment(entries: &[DesktopEnvironmentEntry], total: &mut usize) -> Result<()> {
    if entries.len() > MAX_ARGS {
        return Err(Error::from_reason("InvalidSpawnEnvironment"));
    }
    let mut names = std::collections::HashSet::new();
    for entry in entries {
        if entry.name.is_empty() || entry.name.contains('=') || !names.insert(&entry.name) {
            return Err(Error::from_reason("InvalidSpawnEnvironment"));
        }
        count_text(total, &entry.name)?;
        count_text(total, &entry.value)?;
    }
    Ok(())
}

fn validate_text(
    command: Option<&[String]>,
    cwd: Option<&str>,
    entries: Option<&[DesktopEnvironmentEntry]>,
) -> Result<()> {
    let mut total = 0;
    validate_command(command, &mut total)?;
    if let Some(cwd) = cwd {
        if cwd.is_empty() {
            return Err(Error::from_reason("InvalidSpawnDirectory"));
        }
        count_text(&mut total, cwd)?;
    }
    validate_environment(entries.unwrap_or_default(), &mut total)
}

fn require_mutation(control: &ControlPlane, identity: &DesktopConnectionIdentity) -> Result<()> {
    require_identity(control, identity)?;
    if control.status() != Status::Attached {
        return Err(Error::from_reason("NotAttached"));
    }
    if control
        .options()
        .attach_role
        .is_some_and(RolePolicy::is_viewer)
    {
        return Err(Error::from_reason("ObserveOnly"));
    }
    Ok(())
}

fn require_spawn_target(topology: Option<&Topology>, home: Option<u32>, target: u32) -> Result<()> {
    let topology = topology.ok_or_else(|| Error::from_reason("TopologyUnavailable"))?;
    if !topology.sessions.iter().any(|session| session.id == target) {
        return Err(Error::from_reason("SessionNotFound"));
    }
    if home != Some(target) {
        return Err(Error::from_reason("UnsupportedForeignSession"));
    }
    Ok(())
}

fn require_spawn_size(control: &ControlPlane, request: &SpawnRequest) -> Result<()> {
    if request.initial_size.is_some()
        && !control
            .server()
            .is_some_and(|server| server.has(ServerFeature::SpawnInitialSize))
    {
        return Err(Error::from_reason("UnsupportedSpawnInitialSize"));
    }
    Ok(())
}

fn queue_spawn(
    control: &mut ControlPlane,
    identity: &DesktopConnectionIdentity,
    request: SpawnRequest,
) -> Result<u32> {
    require_mutation(control, identity)?;
    let target = request
        .session_id
        .ok_or_else(|| Error::from_reason("InvalidSessionId"))?;
    require_spawn_target(control.topology(), control.attached_session(), target)?;
    require_spawn_size(control, &request)?;
    Ok(control.spawn_terminal(request))
}

fn queue_termination(
    control: &mut ControlPlane,
    identity: &DesktopConnectionIdentity,
    id: &ResourceId,
) -> Result<u32> {
    require_mutation(control, identity)?;
    if matches!(id, ResourceId::Satellite { .. }) {
        return Err(Error::from_reason("UnsupportedSatelliteTermination"));
    }
    // Membership is a target-existence check, not creator ownership or
    // authorization. The server still decides whether this caller may kill it.
    let known = control
        .topology()
        .is_some_and(|topology| topology.panes.iter().any(|pane| &pane.terminal_id == id));
    if !known {
        return Err(Error::from_reason("TerminalNotFound"));
    }
    Ok(control.kill_terminal(id))
}

#[napi]
#[allow(
    clippy::needless_pass_by_value,
    reason = "NAPI decodes owned JS values"
)]
impl DesktopClient {
    /// Spawn into the attached home session only. Foreign sessions require a
    /// server-side expected-session primitive, not a movable owner-pane hint.
    /// Explicit initial geometry requires the advertised `SpawnInitialSize` feature.
    #[napi]
    pub fn spawn_terminal_with_options(&self, options: DesktopSpawnOptions) -> Result<u32> {
        let (identity, request) = options.into_request()?;
        self.client()?
            .with_control(|control| queue_spawn(control, &identity, request))
    }

    /// Explicitly terminate the named resource's process. This is not detach
    /// or view disposal. `TerminalKilled` answers the request; Closed reports
    /// the separate authoritative resource-lifecycle event. Never auto-retry.
    /// Satellite targets are unsupported until the runtime accepts an exact
    /// resource-instance identity; the hub's incarnation alone is insufficient.
    #[napi]
    pub fn terminate_terminal(
        &self,
        identity: DesktopConnectionIdentity,
        resource_id: String,
    ) -> Result<u32> {
        let id = terminal_id(&resource_id)?;
        self.client()?
            .with_control(|control| queue_termination(control, &identity, &id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor};

    fn attached_control(role: RolePolicy, features: &[ServerFeature]) -> ControlPlane {
        use phux_client_runtime::control::ControlOptions;
        use phux_protocol::caps::{
            BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureSet,
        };
        use phux_protocol::wire::frame::{AttachTarget, FrameKind};
        use phux_protocol::wire::info::SessionSnapshot;
        use phux_protocol::{ClientId, PROTOCOL_VERSION, SessionId, WindowId};

        let mut control = ControlPlane::new(ControlOptions {
            attach: Some(AttachTarget::ByName("empty".into())),
            attach_role: Some(role),
            ..ControlOptions::default()
        });
        control.connection_opened();
        control
            .feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_features(ServerFeatureSet::with(features)),
                server_id: vec![0xab],
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            })
            .expect("HELLO_OK");
        let attach_id = control
            .take_outbound()
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
                FrameKind::Attach { attach_id, .. } => Some(attach_id),
                _ => None,
            })
            .expect("attach command");
        control
            .feed(FrameKind::Attached {
                attach_id,
                initial_client_id: ClientId::new(1),
                snapshot: SessionSnapshot::new(
                    SessionId::new(1),
                    WindowId::new(1),
                    ResourceId::local(1),
                ),
            })
            .expect("ATTACHED");
        control
            .feed(FrameKind::AttachReady { attach_id })
            .expect("ATTACH_READY");
        assert_eq!(control.status(), Status::Attached);
        control
    }

    #[test]
    fn mutation_gate_checks_role_and_current_connection_readiness() {
        let identity = DesktopConnectionIdentity {
            server_id: "ab".into(),
            connection_epoch: "1".into(),
        };
        let mut primary = attached_control(RolePolicy::PRIMARY, &[]);
        assert!(require_mutation(&primary, &identity).is_ok());
        let viewer = attached_control(RolePolicy::VIEWER, &[]);
        assert_eq!(
            require_mutation(&viewer, &identity)
                .expect_err("viewer")
                .reason,
            "ObserveOnly"
        );
        primary.connection_lost(None);
        assert_eq!(
            require_mutation(&primary, &identity)
                .expect_err("disconnected")
                .reason,
            "NotNegotiated"
        );
        primary.connection_opened();
        assert!(
            primary.server().is_some(),
            "old metadata survives until next HELLO_OK"
        );
        assert_eq!(
            require_mutation(&primary, &identity)
                .expect_err("new epoch not negotiated")
                .reason,
            "NotNegotiated"
        );
    }

    fn topology() -> Topology {
        Topology {
            sessions: vec![SessionDescriptor {
                id: 7,
                name: "exact target".into(),
                window_count: 0,
                attached_client_count: 0,
                keep_empty: true,
                created_at_unix_secs: 0,
            }],
            windows: vec![],
            panes: vec![],
            agent_sessions: vec![],
            focused_session: 7,
            focused_pane: ResourceId::local(12),
        }
    }

    fn pane(id: ResourceId) -> PaneDescriptor {
        PaneDescriptor {
            terminal_id: id,
            session_id: 7,
            session_name: "exact target".into(),
            window_id: 1,
            window_index: 0,
            window_name: String::new(),
            cols: 80,
            rows: 24,
            title: None,
            cwd: None,
            is_focused: false,
        }
    }

    #[test]
    fn missing_or_empty_foreign_target_never_falls_back_to_home() {
        let mut graph = topology();
        assert_eq!(
            require_spawn_target(Some(&graph), Some(1), 999)
                .expect_err("missing")
                .reason,
            "SessionNotFound"
        );
        assert_eq!(
            require_spawn_target(Some(&graph), Some(1), 7)
                .expect_err("empty foreign")
                .reason,
            "UnsupportedForeignSession"
        );
        assert!(
            require_spawn_target(Some(&graph), Some(7), 7).is_ok(),
            "empty home has an exact owning connection"
        );
        graph.panes.push(pane(ResourceId::local(12)));
        assert_eq!(
            require_spawn_target(Some(&graph), Some(1), 7)
                .expect_err("populated foreign")
                .reason,
            "UnsupportedForeignSession"
        );
        graph.sessions.clear();
        assert!(
            require_spawn_target(Some(&graph), Some(1), 7).is_err(),
            "orphan pane cannot establish session identity"
        );
    }

    #[test]
    fn a_satellite_pane_does_not_make_a_foreign_session_routable() {
        let mut graph = topology();
        graph.panes.push(pane(ResourceId::satellite("remote", 12)));
        assert_eq!(
            require_spawn_target(Some(&graph), Some(1), 7)
                .expect_err("no route")
                .reason,
            "UnsupportedForeignSession"
        );
    }

    #[test]
    fn optional_argv_is_distinct_from_empty_and_budget_is_utf8_bytes() {
        assert!(validate_text(None, None, None).is_ok());
        assert!(validate_text(Some(&[]), None, None).is_err());
        assert!(validate_text(Some(&["cmd".into(), String::new()]), None, None).is_ok());
        assert!(validate_text(Some(&["é".repeat(MAX_TEXT / 2)]), None, None).is_ok());
        assert!(validate_text(Some(&["é".repeat(MAX_TEXT / 2)]), Some("x"), None).is_err());
        let entries: Vec<_> = (0..=MAX_ARGS)
            .map(|i| DesktopEnvironmentEntry {
                name: format!("KEY{i}"),
                value: String::new(),
            })
            .collect();
        assert!(validate_text(None, None, Some(&entries)).is_err());
    }

    fn identity() -> DesktopConnectionIdentity {
        DesktopConnectionIdentity {
            server_id: "ab".into(),
            connection_epoch: "1".into(),
        }
    }

    /// A requester snapshot before/after `MOVE_RESOURCE` reparents the same
    /// resource from session 7's window to home session 1's window.
    fn refresh_owner(control: &mut ControlPlane, owner: ResourceId, owner_window: u32) {
        use phux_protocol::wire::frame::{CommandResult, CommandValue, FrameKind};
        use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
        use phux_protocol::{SessionId, WindowId};

        let request_id = control.refresh_topology().expect("refresh command");
        let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), owner.clone())
            .with_sessions(vec![
                SessionInfo::new(SessionId::new(1), "home"),
                SessionInfo::new(SessionId::new(7), "foreign"),
            ])
            .with_windows(vec![
                WindowInfo::new(WindowId::new(1), SessionId::new(1), "home"),
                WindowInfo::new(WindowId::new(7), SessionId::new(7), "foreign"),
            ])
            .with_resources(vec![ResourceInfo::new(
                owner,
                WindowId::new(owner_window),
                80,
                24,
            )]);
        control
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::State(snapshot)),
            })
            .expect("state reply");
        let _ = control.take_outbound();
    }

    fn spawn_request(session_id: u32, initial_size: Option<(u16, u16)>) -> SpawnRequest {
        SpawnRequest {
            session_id: Some(session_id),
            initial_size,
            ..SpawnRequest::default()
        }
    }

    fn assert_not_queued(control: &mut ControlPlane, before: u32) {
        assert!(
            control.take_outbound().is_empty(),
            "refusal must not queue a wire command"
        );
        assert_eq!(
            control.next_request_id(),
            before + 1,
            "refusal must not allocate a correlation"
        );
    }

    #[test]
    fn foreign_spawn_is_not_queued_before_or_after_owner_reparent() {
        let mut control = attached_control(RolePolicy::PRIMARY, &[]);
        let owner = ResourceId::local(12);
        for owner_window in [7, 1] {
            refresh_owner(&mut control, owner.clone(), owner_window);
            let graph = control.topology().expect("graph");
            assert_eq!(graph.sessions.len(), 2);
            assert_eq!(graph.panes[0].terminal_id, owner);
            assert_eq!(graph.panes[0].session_id, owner_window);
            let before = control.next_request_id();
            assert_eq!(
                queue_spawn(&mut control, &identity(), spawn_request(7, None))
                    .expect_err("foreign")
                    .reason,
                "UnsupportedForeignSession"
            );
            assert_not_queued(&mut control, before);
        }
        // Exact home routing remains supported and deliberately sends no
        // movable owner-pane hint, even when that pane currently lives at home.
        queue_spawn(&mut control, &identity(), spawn_request(1, None)).expect("home spawn");
        let frames = control.take_outbound();
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            phux_protocol::wire::frame::FrameKind::decode(&frames[0])
                .expect("decode")
                .0,
            phux_protocol::wire::frame::FrameKind::SpawnResource {
                owner_terminal: None,
                ..
            }
        ));
    }

    #[test]
    fn satellite_reused_id_is_not_killed_on_an_unchanged_hub_connection() {
        let mut control = attached_control(RolePolicy::PRIMARY, &[]);
        let satellite = ResourceId::satellite("remote", 7);
        for _ in 0..2 {
            // Both satellite incarnations can report the identical hub graph;
            // the runtime has no satellite-instance fence to distinguish them.
            refresh_owner(&mut control, satellite.clone(), 1);
            assert_eq!(control.connection_epoch(), 1);
            assert_eq!(control.server().expect("server").id, vec![0xab]);
            assert!(
                control
                    .topology()
                    .expect("graph")
                    .panes
                    .iter()
                    .any(|pane| pane.terminal_id == satellite)
            );
            let before = control.next_request_id();
            assert_eq!(
                queue_termination(&mut control, &identity(), &satellite)
                    .expect_err("satellite")
                    .reason,
                "UnsupportedSatelliteTermination"
            );
            assert_not_queued(&mut control, before);
        }
        // A known local target is allowed without a creator-ownership claim.
        let local = ResourceId::local(99);
        refresh_owner(&mut control, local.clone(), 1);
        queue_termination(&mut control, &identity(), &local).expect("known local target");
        assert_eq!(control.take_outbound().len(), 1);
    }

    #[test]
    fn explicit_spawn_size_requires_negotiated_feature_before_queueing() {
        let mut old = attached_control(RolePolicy::PRIMARY, &[]);
        refresh_owner(&mut old, ResourceId::local(12), 1);
        let before = old.next_request_id();
        assert_eq!(
            queue_spawn(&mut old, &identity(), spawn_request(1, Some((93, 31))))
                .expect_err("missing feature")
                .reason,
            "UnsupportedSpawnInitialSize"
        );
        assert_not_queued(&mut old, before);
        queue_spawn(&mut old, &identity(), spawn_request(1, None))
            .expect("unspecified size is supported");

        let mut capable = attached_control(RolePolicy::PRIMARY, &[ServerFeature::SpawnInitialSize]);
        refresh_owner(&mut capable, ResourceId::local(12), 1);
        queue_spawn(&mut capable, &identity(), spawn_request(1, Some((93, 31))))
            .expect("feature supported");
        let frames = capable.take_outbound();
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            phux_protocol::wire::frame::FrameKind::decode(&frames[0])
                .expect("decode")
                .0,
            phux_protocol::wire::frame::FrameKind::SpawnResource {
                initial_size: Some((93, 31)),
                ..
            }
        ));
    }
}
