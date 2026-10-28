//! Which session a window opens when none is named.
//!
//! A server with `ORGANIZATION` always has the owner's Intake, so an untargeted
//! launch opens it. A server without it opens its most recent session. Neither
//! path creates a session. A session the user names explicitly and that does
//! not exist is created persistent: through `SESSION_CREATE` when the server
//! has `ORGANIZATION`, else through `phux new`, which makes a named session
//! persistent.
//!
//! The answer comes from a short-lived browsing connection (no attach) that
//! waits on the runtime's activity listener, not a poll.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use phux_client_runtime::control::{ControlOptions, ControlPlane, Event, Status, Topology};
use phux_client_runtime::{Client, ClientOptions, Listener, Runtime, Target};
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{OperationId, SessionKey};

use super::org::outcome_sentence;
use super::{home_entry, session_create};

/// How long the browsing connection may take to name the session.
const WAIT: Duration = Duration::from_secs(8);

/// What the server offers to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// Attach this existing session.
    Existing(String),
    /// The explicitly requested session does not exist yet.
    Missing(String),
    /// No session was named and the server has neither an Intake nor a session.
    Nothing,
}

/// The explicit request when it exists or is missing; else the Intake (named
/// by `inbox`); else the most recently created session.
#[must_use]
pub fn choose(requested: Option<&str>, topology: &Topology, inbox: Option<&str>) -> Home {
    if let Some(name) = requested.map(str::trim).filter(|name| !name.is_empty()) {
        return if topology.session_named(name).is_some() {
            Home::Existing(name.to_owned())
        } else {
            Home::Missing(name.to_owned())
        };
    }
    if let Some(inbox) = inbox.filter(|name| !name.is_empty()) {
        return Home::Existing(inbox.to_owned());
    }
    topology
        .sessions
        .iter()
        .max_by_key(|session| (session.created_at_unix_secs, session.id))
        .map_or(Home::Nothing, |session| {
            Home::Existing(session.name.clone())
        })
}

/// The Intake session's name: from the topology when it lists the Intake, else
/// from the organization replica's record.
fn inbox_name(control: &ControlPlane, topology: &Topology) -> Option<String> {
    let key = topology.inbox?;
    if let Some(session) = topology.session_keyed(&key) {
        return Some(session.name.clone());
    }
    control
        .organization()
        .session(&key)
        .map(|session| session.name.clone())
}

/// The session a window on `socket` opens: `requested` (created persistent
/// when missing), else the Intake, else the most recent session.
///
/// # Errors
///
/// A sentence for the window when the server cannot be read, the create is
/// refused, or there is nothing to open.
pub fn open_home(phux: &Path, socket: &Path, requested: Option<&str>) -> Result<String, String> {
    Probe::connect(crate::platform::local_target(socket.to_path_buf()))?
        .resolve(phux, socket, requested)
}

/// The session a window on registered host `name` opens: its Intake, else its
/// most recent session. Nothing is created on the remote.
///
/// # Errors
///
/// A sentence for the notification when the host is not registered, cannot
/// be reached, or has nothing to open.
pub fn open_remote_home(name: &str) -> Result<String, String> {
    let probe = Probe::connect(Target::resolve(name, None)?)?;
    let (topology, inbox) = probe.ready()?;
    match choose(None, &topology, inbox.as_deref()) {
        Home::Existing(name) => Ok(name),
        Home::Missing(_) | Home::Nothing => Err(format!(
            "{name} has no sessions yet. Run `phux attach {name}` once to start one."
        )),
    }
}

/// Reconnect to an existing home only. A persistent key wins over a reused name.
pub(crate) fn existing_home(
    socket: &Path,
    name: &str,
    key: Option<SessionKey>,
) -> Result<String, String> {
    existing_target_home(
        crate::platform::local_target(socket.to_path_buf()),
        name,
        key,
    )
}

pub(crate) fn existing_remote_home(
    host: &str,
    name: &str,
    key: Option<SessionKey>,
) -> Result<String, String> {
    existing_target_home(Target::resolve(host, None)?, name, key)
}

fn existing_target_home(
    target: Target,
    name: &str,
    key: Option<SessionKey>,
) -> Result<String, String> {
    let (topology, _) = Probe::connect(target)?.ready()?;
    home_entry(&topology, name, key)
        .map(|session| session.name.clone())
        .ok_or_else(|| {
            "The home session no longer exists. Open an existing session or create a new one explicitly."
                .to_owned()
        })
}

struct Wake(SyncSender<()>);

impl Listener for Wake {
    fn on_activity(&self) {
        let _ = self.0.try_send(());
    }
}

/// A browsing client that closes when dropped.
struct Probe {
    client: Client,
    wake: Receiver<()>,
    deadline: Instant,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.client.close();
    }
}

impl Probe {
    fn connect(target: Target) -> Result<Self, String> {
        let client = Runtime::connect(
            target,
            ClientOptions {
                control: ControlOptions {
                    client_name: "phux-desktop".to_owned(),
                    viewport: (0, 0),
                    attach: None,
                    auto_attach_foreign_spawns: false,
                    ..ControlOptions::default()
                },
                ..ClientOptions::default()
            },
        )
        .map_err(|error| format!("Could not reach the phux server: {error:#}"))?;
        let (tx, wake) = sync_channel(1);
        client.set_listener(Arc::new(Wake(tx)));
        Ok(Self {
            client,
            wake,
            deadline: Instant::now() + WAIT,
        })
    }

    fn resolve(
        &self,
        phux: &Path,
        socket: &Path,
        requested: Option<&str>,
    ) -> Result<String, String> {
        let (topology, inbox) = self.ready()?;
        match choose(requested, &topology, inbox.as_deref()) {
            Home::Existing(name) => Ok(name),
            Home::Missing(name) if self.organization() => self.create(&name).map(|()| name),
            Home::Missing(name) => create_with_cli(phux, socket, &name).map(|()| name),
            Home::Nothing => Err(
                "This server has no sessions and no Intake. Create one with `phux new -s NAME`, then reconnect."
                    .to_owned(),
            ),
        }
    }

    fn organization(&self) -> bool {
        self.client
            .server()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::Organization))
    }

    /// Wait for the topology and, from an `ORGANIZATION` server, the seeded
    /// replica (or its refusal, which leaves the topology to choose from).
    fn ready(&self) -> Result<(Topology, Option<String>), String> {
        let mut unavailable = None;
        loop {
            if let Some(home) = self.sample_ready(&mut unavailable)? {
                return Ok(home);
            }
            self.wait("The phux server did not list its sessions in time.")?;
        }
    }

    fn sample_ready(
        &self,
        unavailable: &mut Option<u64>,
    ) -> Result<Option<(Topology, Option<String>)>, String> {
        // This Client operation rearms the activity wake; a direct control-plane
        // drain would leave the next organization publication without a wake.
        let observation = self.client.take_observation();
        for event in observation.events {
            match event {
                Event::ConnectionOpened { .. } => *unavailable = None,
                Event::OrganizationUnavailable { .. } => {
                    *unavailable = Some(observation.connection_epoch)
                }
                _ => {}
            }
        }
        // Never return a pre-seed topology after a separate read observes the
        // replica as live. Status, Intake and readiness share a control lock.
        self.client
            .with_control(|control| ready_home(control, *unavailable))
    }

    fn create(&self, name: &str) -> Result<(), String> {
        // Windows HOME names a different filesystem. Let the WSL server choose
        // its own home instead of sending a Windows profile path as a cwd.
        let home = if cfg!(windows) {
            None
        } else {
            std::env::var("HOME").ok().filter(|home| !home.is_empty())
        };
        self.client
            .submit_organization(session_create(name, home))
            .map_err(|error| error.to_string())
            .and_then(|operation| self.await_operation(operation))
            .map_err(|why| format!("Could not create session {name:?}: {why}"))
    }

    fn await_operation(&self, operation: OperationId) -> Result<(), String> {
        loop {
            let observation = self.client.take_observation();
            for event in observation.events {
                if let Event::OrganizationOperation {
                    operation: done,
                    outcome,
                } = event
                    && done == operation
                {
                    return if outcome.succeeded() {
                        Ok(())
                    } else {
                        Err(outcome_sentence(&outcome))
                    };
                }
            }
            if observation.status.is_terminal() {
                return Err(observation
                    .last_error
                    .unwrap_or_else(|| "the connection closed".to_owned()));
            }
            self.wait("the server did not answer in time")?;
        }
    }

    fn wait(&self, timeout: &str) -> Result<(), String> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timeout.to_owned());
        }
        self.wake.recv_timeout(left).map_err(|_| timeout.to_owned())
    }
}

fn ready_home(
    control: &ControlPlane,
    unavailable: Option<u64>,
) -> Result<Option<(Topology, Option<String>)>, String> {
    if control.status().is_terminal() {
        return Err(control
            .last_error()
            .unwrap_or("The phux server closed the connection.")
            .to_owned());
    }
    let negotiated = matches!(control.status(), Status::Negotiated | Status::Attached);
    let seeded = !control.organization_supported()
        || unavailable == Some(control.connection_epoch())
        || control.organization().is_live();
    let Some(topology) = control.topology().filter(|_| negotiated && seeded) else {
        return Ok(None);
    };
    Ok(Some((topology.clone(), inbox_name(control, topology))))
}

#[cfg(test)]
#[path = "home_probe_tests.rs"]
mod probe_tests;

/// `phux new` on a server without `ORGANIZATION`: a session the CLI is given
/// a name for is persistent.
fn create_with_cli(phux: &Path, socket: &Path, name: &str) -> Result<(), String> {
    if phux.as_os_str().is_empty() {
        return Err(
            "Install the phux CLI from https://phux.sh/install so the desktop can create sessions."
                .to_owned(),
        );
    }
    let output = crate::platform::phux_command(phux)
        .arg("--socket")
        .arg(socket)
        .args(["new", "--json", "-s", name])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("Could not run {}: {error}", phux.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "Could not create session {name:?}: {}",
        stderr.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_core::layout::wire::HandleMap;
    use phux_client_runtime::control::SessionDescriptor;
    use phux_protocol::ResourceId;
    use phux_protocol::wire::frame::SessionLifetime;

    use crate::connection::{designated_intake_session, sync_home_name};

    fn session(id: u32, name: &str, created: i64) -> SessionDescriptor {
        SessionDescriptor {
            id,
            name: name.to_owned(),
            attached_client_count: 0,
            keep_empty: true,
            created_at_unix_secs: created,
            key: None,
            lifetime: Some(SessionLifetime::Persistent),
            revision: None,
            members: 1,
            arrangement: None,
            awaiting_organization: false,
        }
    }

    fn topology(sessions: Vec<SessionDescriptor>) -> Topology {
        Topology {
            sessions,
            panes: Vec::new(),
            agent_sessions: Vec::new(),
            focused_session: 0,
            focused_pane: ResourceId::local(0),
            authority: None,
            inbox: None,
            handles: HandleMap::new(),
        }
    }

    #[test]
    fn an_explicit_name_wins_and_is_never_replaced() {
        let graph = topology(vec![session(1, "work", 10)]);
        assert_eq!(
            choose(Some("work"), &graph, Some("inbox")),
            Home::Existing("work".into())
        );
        assert_eq!(
            choose(Some("new"), &graph, Some("inbox")),
            Home::Missing("new".into())
        );
    }

    #[test]
    fn home_identity_follows_renames_without_retargeting_to_the_old_name() {
        let home_key = SessionKey::new([1; 16]).unwrap();
        let other_key = SessionKey::new([2; 16]).unwrap();
        let mut home = session(1, "Intake", 10);
        home.key = Some(home_key);
        let mut name = "Intake".to_owned();
        let mut key = None;
        sync_home_name(&topology(vec![home.clone()]), &mut name, &mut key);
        assert_eq!(key, Some(home_key));
        home.name = "Unorganized".to_owned();
        let mut other = session(2, "Intake", 11);
        other.key = Some(other_key);
        sync_home_name(&topology(vec![other, home]), &mut name, &mut key);
        assert_eq!(name, "Unorganized");
        assert_eq!(key, Some(home_key));
    }

    #[test]
    fn a_missing_home_key_does_not_adopt_a_different_session_with_the_old_name() {
        let mut replacement = session(2, "Intake", 11);
        replacement.key = Some(SessionKey::new([2; 16]).unwrap());
        let home_key = SessionKey::new([1; 16]).unwrap();
        let mut key = Some(home_key);
        let mut name = "Intake".to_owned();
        sync_home_name(&topology(vec![replacement]), &mut name, &mut key);
        assert_eq!(key, Some(home_key));
    }

    #[test]
    fn intake_kill_protection_follows_the_designated_key_not_names_or_import_flags() {
        let intake_key = SessionKey::new([1; 16]).unwrap();
        let mut intake = session(1, "Renamed intake", 10);
        intake.key = Some(intake_key);
        let mut imported = session(2, "Intake", 11);
        imported.key = Some(SessionKey::new([2; 16]).unwrap());
        imported.awaiting_organization = true;
        let mut graph = topology(vec![intake, imported]);
        graph.inbox = Some(intake_key);
        assert!(designated_intake_session(&graph, 1));
        assert!(!designated_intake_session(&graph, 2));
        assert!(!designated_intake_session(&graph, 3));
        graph.inbox = None;
        assert!(!designated_intake_session(&graph, 1));
    }

    #[test]
    fn untargeted_opens_the_inbox_then_the_most_recent_session() {
        let graph = topology(vec![
            session(1, "old", 10),
            session(2, "recent", 30),
            session(3, "middle", 20),
        ]);
        assert_eq!(
            choose(None, &graph, Some("inbox")),
            Home::Existing("inbox".into())
        );
        assert_eq!(choose(None, &graph, None), Home::Existing("recent".into()));
        assert_eq!(
            choose(Some("  "), &graph, None),
            Home::Existing("recent".into())
        );
    }

    #[test]
    fn nothing_to_open_never_invents_a_default() {
        assert_eq!(choose(None, &topology(Vec::new()), None), Home::Nothing);
    }
}
