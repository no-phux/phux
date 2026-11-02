//! Shared test helpers for the upgrade-orchestration test suite.
#![allow(clippy::unwrap_used, reason = "tests")]

use phux_resource::agent_session::AgentSessionResource as _;
use std::os::fd::AsRawFd;

use super::super::handoff::*;
use super::super::validate::*;
use super::super::*;
use crate::resource::agent_session::AgentSessionBootstrap;
use tokio::sync::oneshot;

pub(super) fn args_as_strings(flags: RuntimeFlags) -> Vec<String> {
    resume_args(7, Path::new("/run/phux/phux.sock"), flags)
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect()
}

pub(super) fn fd_flags(fd: RawFd) -> rustix::io::FdFlags {
    // SAFETY: test callers keep the backing file open for this borrow.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    rustix::io::fcntl_getfd(borrowed).unwrap()
}

pub(super) fn set_fd_flags(fd: RawFd, flags: rustix::io::FdFlags) {
    // SAFETY: test callers keep the backing file open for this borrow.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    rustix::io::fcntl_setfd(borrowed, flags).unwrap();
}

pub(super) fn no_pty_handoff() -> PaneUpgradeHandle {
    PaneUpgradeHandle {
        master_fd: None,
        child_pid: None,
        cols: 80,
        rows: 24,
        cell_px: None,
        osc_title: None,
        cwd: None,
        vt_replay_bytes: Vec::new(),
        scrollback_bytes: Vec::new(),
    }
}

pub(super) fn satellite(name: &str, endpoint: &str) -> phux_config::SatelliteConfigEntry {
    phux_config::SatelliteConfigEntry {
        name: name.to_owned(),
        endpoint: endpoint.to_owned(),
        enabled: true,
        token_file: None,
        cert_fingerprint: None,
    }
}

pub(super) fn install(from: &str, to: &Path) {
    std::fs::copy(from, to).unwrap();
    std::fs::set_permissions(to, std::fs::metadata(from).unwrap().permissions()).unwrap();
}

pub(super) fn write_stub_phux(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("phux");
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Append `records` to an agent session's engine, as
/// `APPEND_RESOURCE_OUTPUT`, returning the engine's verdict.
pub(super) async fn try_append(
    append: &mpsc::Sender<crate::resource::agent_session::AppendRequest>,
    records: &str,
) -> Result<
    crate::resource::agent_session::AppendAccepted,
    crate::resource::agent_session::AppendRejection,
> {
    let (reply, rx) = oneshot::channel();
    append
        .send(crate::resource::agent_session::AppendRequest {
            bytes: bytes::Bytes::copy_from_slice(records.as_bytes()),
            reply,
            publication: None,
        })
        .await
        .unwrap();
    rx.await.unwrap()
}

pub(super) async fn append_records(
    append: &mpsc::Sender<crate::resource::agent_session::AppendRequest>,
    records: &str,
) -> crate::resource::agent_session::AppendAccepted {
    try_append(append, records).await.unwrap()
}

pub(super) fn empty_agent_cut() -> AgentSessionBootstrap {
    AgentSessionBootstrap {
        base_seq: 0,
        resume_after_seq: None,
        records: Vec::new(),
        dropped: 0,
        ended: false,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    }
}

/// The source is an owned non-executable file, so the actual exec syscall
/// fails safely without replacing this test process or touching an install.
pub(super) fn unexecutable_plan(
    seal: AgentSessionSeal,
) -> (UpgradePlan, std::fs::File, rustix::io::FdFlags) {
    use std::os::unix::fs::PermissionsExt as _;

    let listener = tempfile::tempfile().unwrap();
    let original = fd_flags(listener.as_raw_fd());
    let blob_file = tempfile::tempfile().unwrap();
    let mut guard = FdFlagsGuard::new();
    guard.clear_cloexec(blob_file.as_raw_fd()).unwrap();
    guard.clear_cloexec(listener.as_raw_fd()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("non-executable");
    std::fs::write(&path, b"not an executable\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let plan = UpgradePlan {
        executable: PinnedExecutable {
            source_path: path.clone(),
            path,
            dir,
        },
        blob_fd: blob_file.as_raw_fd(),
        socket_path: PathBuf::from("/unused-test-socket"),
        flags: RuntimeFlags::default(),
        fd_flags: guard,
        _blob_file: blob_file,
        _listener_fd: dup_listener(listener.as_raw_fd()).unwrap(),
        _delegated_listeners: Vec::new(),
        _handoffs: HashMap::new(),
        agent_seal: seal,
        _native_seal: None,
    };
    (plan, listener, original)
}

/// Spawn an engine for an `AgentSession` under `parent`, returning its
/// wire id and append channel.
pub(super) fn spawn_agent_session(
    s: &mut crate::state::ServerState,
    parent: phux_core::ids::ResourceId,
    provider: &str,
    native_id: Option<&str>,
) -> (
    phux_protocol::ids::ResourceId,
    mpsc::Sender<crate::resource::agent_session::AppendRequest>,
) {
    let facet = phux_core::resource::AgentFacet {
        provider: provider.to_owned(),
        native_id: native_id.map(str::to_owned),
        state: Some("working".to_owned()),
    };
    let core = s.new_agent_session(parent, &facet).unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    let bundle = crate::resource::agent_session::AgentSessionActor::build(
        parent,
        provider,
        native_id,
        token.clone(),
        4096,
    );
    let append = bundle.handle.agent_session().unwrap().append.clone();
    let wire = s.spawn_resource_actor(core, bundle.handle, token, bundle.actor.run());
    (wire, append)
}

/// phux-1x9s.3: an agent session under a carried pane crosses the
/// upgrade under the same wire id with its provenance, state, and record
/// tail, and the resumed engine continues the same stream bound to the
/// rebuilt pane. A session under a pane that does not cross live (here a
/// retained one) is dropped with it.
/// An old image with a live pane hosting an agent session, and a retained
/// pane hosting another. Returns the live pane's and its session's wire
/// ids and the session's append channel.
pub(super) fn old_image_with_agent_sessions(
    s: &mut crate::state::ServerState,
) -> (
    phux_protocol::ids::ResourceId,
    phux_protocol::ids::ResourceId,
    mpsc::Sender<crate::resource::agent_session::AppendRequest>,
) {
    let (sid, live_pane) = s.seed_session("main");
    let live = crate::runtime::apps::terminal::test_support::seeded_terminal(20, 5, b"live");
    let live_token = live.token.clone();
    let pane_wire = s.spawn_resource_actor(live_pane, live.handle, live_token, live.actor.run());
    let (agent_wire, append) = spawn_agent_session(s, live_pane, "claude", Some("native-1"));

    let exited = s.add_pane_to_session(sid).unwrap();
    let dead = crate::runtime::apps::terminal::test_support::seeded_terminal(20, 5, b"");
    let dead_token = dead.token.clone();
    let _ = s.spawn_resource_actor(exited, dead.handle, dead_token, dead.actor.run());
    let _ = spawn_agent_session(s, exited, "pi", None);
    s.note_retain_request(exited, 60);
    let _ = s
        .retain_exited(exited, phux_core::process::ExitOutcome::exited(1), 1)
        .unwrap();

    let _ = s.build_session_snapshot(sid);
    let _ = s.intern_window_wire(sid);
    s.set_upgrade_context(
        7,
        PathBuf::from("/nonexistent/phux.sock"),
        RuntimeFlags::default(),
    );
    (pane_wire, agent_wire, append)
}

/// The reversible half of `prepare_upgrade`, down to the staged blob and
/// the seal it holds on the captured sessions.
pub(super) async fn capture_blob(state: &SharedState) -> (StateBlob, AgentSessionSeal) {
    let context = capture_upgrade_context(state).unwrap();
    assert_eq!(
        context.agent_session_senders.len(),
        1,
        "only the live pane's session is cut"
    );
    let panes = collect_pane_handoffs(context.pane_senders, Duration::from_secs(2))
        .await
        .unwrap();
    let (agent_sessions, seal) =
        collect_agent_session_cuts(context.agent_session_senders, Duration::from_secs(2)).await;
    let blob = reassemble_unchanged_tree(
        state,
        context.listener_fd,
        context.listener_fd,
        &context.tree_identity,
        &UpgradeHandoffs {
            panes,
            agent_sessions,
        },
    )
    .expect("a busy agent session does not make the tree look changed");
    (blob, seal)
}
