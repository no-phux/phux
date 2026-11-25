//! ADR-0162 §5: a server that dies without a shutdown leaves its records
//! in the store, and the next start reconciles them. A recorded process
//! that still runs on this boot with its recorded start time is `Lost`; one
//! that is gone ended `Unobserved`. Nothing is relaunched, and a second
//! restart changes nothing.

use std::path::Path;
use std::process::{Child, Command, Stdio};

use phux_core::{
    Actor, ExecState, ExitPolicy, Outcome, ResourceKey, SessionLifetime, TerminalLaunch,
};
use phux_server::OrgStoreLocation;
use phux_server::org_store::{OrgStore, SqliteOrgStore, StoredOrg};
use phux_server::state::{ServerState, TerminalTarget, fresh_org, now_stamp};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, join_after_shutdown, run_local, spawn_server_with, wait_for_socket,
};
use tempfile::TempDir;

/// A child that outlives the "crashed" server, killed on drop.
struct Orphan(Child);

impl Drop for Orphan {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn pid(child: &Child) -> i32 {
    i32::try_from(child.id()).unwrap()
}

/// The previous run: a persistent session with two Terminals whose
/// processes it recorded, one still running and one already gone, then a
/// crash (the state is dropped with nothing torn down).
fn crashed_run(store_path: &Path) -> (Orphan, ResourceKey, ResourceKey) {
    let orphan = Orphan(
        Command::new("sleep")
            .arg("300")
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut gone = Command::new("sleep").arg("300").spawn().unwrap();
    let gone_pid = pid(&gone);
    // Read its identity while it exists, then let it end.
    let gone_identity =
        Some(phux_server::reconcile::proc_identity(gone_pid).expect("live child identity"));
    gone.kill().unwrap();
    gone.wait().unwrap();

    let mut state = ServerState::new();
    let store = SqliteOrgStore::open(store_path).unwrap();
    let org = fresh_org(state.instance_id(), now_stamp());
    state.install_org(Box::new(store), org, false);
    state.persist_first_start().unwrap();
    let (session, alive) = state
        .create_terminal(
            TerminalTarget::NewSession {
                name: "work",
                lifetime: SessionLifetime::Persistent,
            },
            TerminalLaunch::default(),
            ExitPolicy::Remove,
            &Actor::owner(),
        )
        .unwrap();
    state.activation_started(
        state.activation_token(alive).unwrap(),
        phux_server::reconcile::proc_identity(pid(&orphan.0)),
    );
    let (_, dead) = state
        .create_terminal(
            TerminalTarget::Session(session),
            TerminalLaunch::default(),
            ExitPolicy::Remove,
            &Actor::owner(),
        )
        .unwrap();
    state.activation_started(state.activation_token(dead).unwrap(), gone_identity);
    let keys = (
        state.registry().resource_key(alive).unwrap(),
        state.registry().resource_key(dead).unwrap(),
    );
    drop(state);
    (orphan, keys.0, keys.1)
}

fn load(store_path: &Path) -> StoredOrg {
    SqliteOrgStore::open(store_path)
        .unwrap()
        .load()
        .unwrap()
        .expect("the store holds the records")
}

fn state_of(stored: &StoredOrg, key: ResourceKey) -> (ExecState, Option<Outcome>) {
    let record = stored
        .resources
        .iter()
        .find(|r| r.key == key)
        .expect("the record survives the crash");
    let activation = record.activation.as_ref().expect("activation");
    assert_eq!(
        activation.id,
        phux_core::ActivationId::FIRST,
        "nothing relaunched"
    );
    (activation.state, activation.outcome)
}

/// Start the server on the store, wait until it serves, and stop it.
async fn restart(dir: &Path, store_path: &Path) {
    let socket = dir.join("phux.sock");
    let location = OrgStoreLocation::Path(store_path.to_path_buf());
    let (shutdown, handle) = spawn_server_with(socket.clone(), None, move |cfg| {
        cfg.org_store = location;
    });
    let probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    drop(probe);
    join_after_shutdown(shutdown, handle).await;
}

#[test]
fn a_restart_marks_survivors_lost_and_the_gone_unobserved_and_relaunches_nothing() {
    run_local(async {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("org").join("v1.sqlite3");
        let (orphan, alive, dead) = crashed_run(&store_path);
        let before = load(&store_path);
        assert_eq!(state_of(&before, alive).0, ExecState::Running);

        restart(dir.path(), &store_path).await;
        let after = load(&store_path);
        assert_eq!(
            state_of(&after, alive),
            (ExecState::Lost, None),
            "the process outlived its server"
        );
        assert_eq!(
            state_of(&after, dead),
            (ExecState::Completed, Some(Outcome::Unobserved)),
            "an ended process is unobserved, and its record stays"
        );
        assert_eq!(after.sessions.len(), before.sessions.len());
        assert_eq!(after.resources.len(), before.resources.len());
        assert!(
            after
                .resources
                .iter()
                .filter_map(|r| r.activation.as_ref())
                .all(|a| !matches!(a.state, ExecState::Starting | ExecState::Running)),
            "no activation is current after reconciliation"
        );

        // A second restart finds nothing to change.
        restart(dir.path(), &store_path).await;
        let again = load(&store_path);
        let revisions = |stored: &StoredOrg| {
            let mut revs: Vec<_> = stored
                .resources
                .iter()
                .map(|r| (r.key, r.revision))
                .collect();
            revs.sort();
            revs
        };
        assert_eq!(
            revisions(&again),
            revisions(&after),
            "reconciliation is idempotent"
        );
        drop(orphan);
    });
}

#[test]
fn restarted_projections_refuse_wire_relaunch_without_starting_a_default_shell() {
    use phux_protocol::ids::{OperationId, Revision};
    use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, Refusal};

    run_local(async {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("org.sqlite3");
        let mut state = ServerState::new();
        let store = SqliteOrgStore::open(&store_path).unwrap();
        let org = fresh_org(state.instance_id(), now_stamp());
        state.install_org(Box::new(store), org, false);
        state.persist_first_start().unwrap();
        let (_, parent) = state.seed_session("work");
        let actor = Actor::owner();
        let agent = state
            .create_child(
                parent,
                phux_core::NewResource::child(
                    phux_core::LaunchSpec::Agent(phux_core::resource::AgentLaunch {
                        provider: "test-producer".into(),
                        native_id: None,
                    }),
                    None,
                ),
                &actor,
            )
            .unwrap();
        let tree = state
            .create_document(
                parent,
                phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                    target: "test-app".into(),
                }),
                &actor,
            )
            .unwrap();
        let surface = state
            .create_document(
                parent,
                phux_core::LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
                    target: "test-window".into(),
                }),
                &actor,
            )
            .unwrap();
        let keys = [agent, tree, surface].map(|id| state.registry().resource_key(id).unwrap());
        drop(state); // Simulate a crash: no graceful teardown of records.

        let location = OrgStoreLocation::Path(store_path.clone());
        let socket = dir.path().join("phux.sock");
        let (shutdown, handle) = spawn_server_with(socket.clone(), None, move |cfg| {
            cfg.org_store = location;
            // The vulnerable path would create a real default-shell PTY.
            cfg.seed_with_pty = true;
        });
        let mut client = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let reconciled = load(&store_path);
        for (index, key) in keys.into_iter().enumerate() {
            let before = reconciled
                .resources
                .iter()
                .find(|record| record.key == key)
                .unwrap();
            assert_eq!(
                state_of(&reconciled, key),
                (ExecState::Completed, Some(Outcome::Unobserved))
            );
            let result = phux_server_testkit::command(
                &mut client,
                u32::try_from(index + 1).unwrap(),
                WireCommand::ResourceRelaunch {
                    operation: OperationId::new([u8::try_from(index + 1).unwrap(); 16]).unwrap(),
                    resource: phux_server::org_wire::resource_key(key).unwrap(),
                    expected: Revision::new(before.revision.get()),
                },
            )
            .await;
            assert!(
                matches!(result, CommandResult::Refused(Refusal::Unsupported { .. })),
                "{result:?}"
            );
            let after = load(&store_path);
            assert_eq!(
                after.resources.iter().find(|record| record.key == key),
                Some(before)
            );
            assert_eq!(after.resources.len(), reconciled.resources.len());
        }
        drop(client);
        join_after_shutdown(shutdown, handle).await;
    });
}

#[test]
fn a_crash_losing_start_evidence_cannot_prove_the_process_was_never_launched() {
    run_local(async {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("org.sqlite3");
        let mut orphan = Orphan(
            Command::new("sleep")
                .arg("300")
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut state = ServerState::new();
        let store = SqliteOrgStore::open(&store_path).unwrap();
        // Refuse only the later start transition; admission inserts the
        // row as `starting` and so still commits.
        let db = rusqlite::Connection::open(&store_path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER reject_start BEFORE UPDATE ON resources WHEN NEW.exec_state <> 'starting' BEGIN SELECT RAISE(FAIL, 'test lost start'); END;",
        ).unwrap();
        state.install_org(
            Box::new(store),
            fresh_org(state.instance_id(), now_stamp()),
            false,
        );
        state.persist_first_start().unwrap();
        let (_, pane) = state
            .create_terminal(
                TerminalTarget::NewSession {
                    name: "ambiguous",
                    lifetime: SessionLifetime::Persistent,
                },
                TerminalLaunch::default(),
                ExitPolicy::Retain,
                &Actor::owner(),
            )
            .unwrap();
        let producer = state.activation_token(pane).unwrap();
        let proc =
            phux_server::reconcile::proc_identity(pid(&orphan.0)).expect("live process identity");
        state.activation_started(producer, Some(proc));
        assert!(state.activation_persistence_pending(producer.resource));
        assert_eq!(
            state_of(&load(&store_path), producer.resource),
            (ExecState::Starting, None)
        );
        drop(state); // Hard crash: the queued process identity is genuinely lost.
        db.execute_batch("DROP TRIGGER reject_start;").unwrap();
        drop(db);

        restart(dir.path(), &store_path).await;
        let after = load(&store_path);
        assert_eq!(state_of(&after, producer.resource), (ExecState::Lost, None));
        let record = after
            .resources
            .iter()
            .find(|r| r.key == producer.resource)
            .unwrap();
        assert!(record.activation.as_ref().unwrap().host_proc.is_none());
        let revision = record.revision;
        assert!(
            orphan.0.try_wait().unwrap().is_none(),
            "the unrecorded process can still be alive"
        );
        restart(dir.path(), &store_path).await;
        let again = load(&store_path);
        assert_eq!(state_of(&again, producer.resource), (ExecState::Lost, None));
        assert_eq!(
            again
                .resources
                .iter()
                .find(|r| r.key == producer.resource)
                .unwrap()
                .revision,
            revision
        );
    });
}
