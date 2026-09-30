//! The dispatch guard as a paired client meets it on the wire
//! (`docs/spec/workload-auth.md` §6, §7).
//!
//! Each test runs the real client loop (`handle_client`) over a scripted
//! transport, with a workload identity stamped the way the QUIC acceptor
//! stamps it. The credential the transport cached always claims `*@global`;
//! every test passes only if the grant came from the engine instead.
//!
//! Two engines mint the grant. The `paired` [`ScopedPolicy`] reads a real
//! registry file, which may hold only `global` and `host` selectors
//! (§5.1: session and Terminal ids restart with the server). Group and
//! Terminal grants, which a future HELLO-requested attenuation carries, are
//! minted by a fixed engine so the guard's handling of them stays covered.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities, Compression};
use phux_protocol::ids::{GroupId, InputOperationId, ResourceId as WireResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, ErrorCode, FrameKind, MoveError, MoveResult, Scope,
    SpawnError, SpawnResult, TYPE_FRAME_COMPRESSED, WHOAMI_KEY,
};
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;

use super::client::handle_client;
use super::input_lane::{InputLaneHandle, spawn_input_lane};
use super::test_support::{Feed, Wire, encode, hello, owner_peer, workload_identity};
use crate::auth::AuthenticatedCredential;
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError, ScopedPolicy};
use crate::state::{ClientId, SharedState};
use crate::workload::{ReloadingWorkloadRegistry, WorkloadRegistry};

const CLIENT: ClientId = ClientId(21);

/// Mints the same scoped grant for every connection: the shape a
/// HELLO-requested attenuation would produce, including Group and Terminal
/// selectors no registry record may hold.
#[derive(Debug)]
struct FixedGrant(TerminalScopeSet);

impl PolicyEngine for FixedGrant {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let grant = ConnectionGrant::scoped(self.0.clone(), credential.map(|c| c.id.clone()))
            .map_err(|err| PolicyError::Internal(err.to_string()));
        Box::pin(async move { grant })
    }
}

/// The ids a test's scopes may need to name.
struct Topology {
    alpha: u32,
    alpha_group: u32,
}

/// Which engine mints the grant.
#[derive(Clone, Copy)]
enum Engine {
    /// The `paired` engine over a real registry file.
    Registry,
    /// [`FixedGrant`].
    Fixed,
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: SharedState,
    credential_id: String,
    alpha: WireResourceId,
    beta: WireResourceId,
}

fn paired(scopes: &[&str]) -> Fixture {
    let scopes: Vec<String> = scopes.iter().map(|scope| (*scope).to_owned()).collect();
    fixture(Engine::Registry, |_| scopes)
}

/// Two sessions with one Terminal each; an engine whose grant is the scopes
/// `ceiling` names; and `CLIENT` stamped as a workload's QUIC connection.
fn fixture(engine: Engine, ceiling: impl FnOnce(&Topology) -> Vec<String>) -> Fixture {
    let state = SharedState::new();
    let (alpha, beta, alpha_group) = state.with_mut(|s| {
        let (alpha_session, _, alpha_core) = s.seed_session("alpha");
        let (_, _, beta_core) = s.seed_session("beta");
        let alpha = s.intern_terminal_wire(alpha_core);
        let beta = s.intern_terminal_wire(beta_core);
        (alpha, beta, s.idspace.intern_session(alpha_session).get())
    });
    let WireResourceId::Local { id: alpha_id } = alpha else {
        unreachable!("a seeded pane is local");
    };
    let scopes = ceiling(&Topology {
        alpha: alpha_id,
        alpha_group,
    });
    let dir = tempfile::tempdir().unwrap();
    let (id, policy): (String, Arc<dyn PolicyEngine>) = match engine {
        Engine::Registry => {
            let path = dir.path().join("workload-keys");
            let id = WorkloadRegistry::register(&path, b"scope-matrix-key", scopes, None)
                .unwrap()
                .id;
            let registry = Arc::new(ReloadingWorkloadRegistry::load(path).unwrap());
            (id, Arc::new(ScopedPolicy::paired(registry)))
        }
        Engine::Fixed => {
            let set = TerminalScopeSet::parse_all(&scopes).unwrap();
            let id = format!("sha256:{}", "f".repeat(64));
            (id, Arc::new(FixedGrant(set)))
        }
    };
    state.with_mut(|s| {
        s.set_policy_engine(policy);
        s.set_connection_identity(CLIENT, workload_identity(&id, &["*@global"]));
    });
    Fixture {
        _dir: dir,
        state,
        credential_id: id,
        alpha,
        beta,
    }
}

fn script(frames: &[FrameKind]) -> Vec<BytesMut> {
    frames.iter().map(encode).collect()
}

fn command(request_id: u32, command: Command) -> FrameKind {
    FrameKind::Command {
        request_id,
        command,
    }
}

fn get_screen(request_id: u32, terminal_id: WireResourceId) -> FrameKind {
    command(
        request_id,
        Command::GetScreen {
            terminal_id,
            request_scrollback: None,
            cells: false,
            format: 0,
        },
    )
}

fn paste(terminal_id: &WireResourceId, data: Vec<u8>) -> FrameKind {
    FrameKind::InputPaste {
        terminal_id: terminal_id.clone(),
        event: PasteEvent {
            trust: PasteTrust::Trusted,
            data,
        },
    }
}

fn spawn_owned_by(request_id: u32, group: u32, owner: &WireResourceId) -> FrameKind {
    FrameKind::SpawnResource {
        request_id,
        group: GroupId::new(group),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: Some(owner.clone()),
        agent_session: None,
        initial_size: None,
        resource: None,
    }
}

fn whoami(request_id: u32) -> FrameKind {
    FrameKind::GetMetadata {
        request_id,
        scope: Scope::Global,
        key: WHOAMI_KEY.to_owned(),
    }
}

struct Outcome {
    frames: Vec<FrameKind>,
    closed: bool,
}

impl Outcome {
    fn result(&self, id: u32) -> Option<&CommandResult> {
        self.frames.iter().find_map(|frame| match frame {
            FrameKind::CommandResult { request_id, result } if *request_id == id => Some(result),
            _ => None,
        })
    }

    fn spawned(&self, id: u32) -> Option<&SpawnResult> {
        self.frames.iter().find_map(|frame| match frame {
            FrameKind::ResourceSpawned { request_id, result } if *request_id == id => Some(result),
            _ => None,
        })
    }

    fn whoami(&self, id: u32) -> serde_json::Value {
        let value = self
            .frames
            .iter()
            .find_map(|frame| match frame {
                FrameKind::MetadataValue { request_id, value } if *request_id == id => {
                    value.clone()
                }
                _ => None,
            })
            .expect("a whoami record");
        serde_json::from_slice(&value).unwrap()
    }

    fn denials(&self, request_id: Option<u32>) -> usize {
        self.frames
            .iter()
            .filter(|frame| is_denial(frame, request_id))
            .count()
    }

    fn ponged(&self, nonce: u64) -> bool {
        self.frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Pong { nonce: n } if *n == nonce))
    }

    fn detached(&self) -> bool {
        self.frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::Detached { .. }))
    }
}

fn is_denial(frame: &FrameKind, request: Option<u32>) -> bool {
    matches!(
        frame,
        FrameKind::Error {
            request_id,
            code: ErrorCode::PermissionDenied,
            ..
        } if *request_id == request
    )
}

fn is_permission_denied(result: Option<&CommandResult>) -> bool {
    matches!(
        result,
        Some(CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            ..
        })
    )
}

/// Run the client loop over `frames` until `done` holds for what the server
/// wrote, the loop ends, or five seconds pass. The outcome is snapshotted
/// before the connection is cancelled, so teardown frames never count.
async fn exchange(
    state: &SharedState,
    frames: Vec<BytesMut>,
    lane: Option<InputLaneHandle>,
    done: impl Fn(&[FrameKind]) -> bool,
) -> Outcome {
    let wire = Wire::default();
    let root = CancellationToken::new();
    let connection = root.child_token();
    let task = tokio::task::spawn_local(handle_client(
        Feed::scripted(frames),
        wire.clone(),
        state.clone(),
        CLIENT,
        connection.clone(),
        root.clone(),
        lane,
        TransportType::Quic,
        false,
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&wire.frames.borrow()) && !task.is_finished() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let outcome = Outcome {
        frames: wire.frames.borrow().clone(),
        closed: wire.closed.get(),
    };
    connection.cancel();
    let _ = task.await;
    outcome
}

fn has_result(id: u32) -> impl Fn(&[FrameKind]) -> bool {
    move |frames| {
        frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::CommandResult { request_id, .. } if *request_id == id))
    }
}

fn has_metadata(id: u32) -> impl Fn(&[FrameKind]) -> bool {
    move |frames| {
        frames.iter().any(
            |frame| matches!(frame, FrameKind::MetadataValue { request_id, .. } if *request_id == id),
        )
    }
}

#[tokio::test(flavor = "current_thread")]
async fn denials_are_rate_limited_and_never_close_the_connection() {
    LocalSet::new()
        .run_until(async {
            let fixture = paired(&["observe@global"]);
            let mut frames = vec![hello("scope-matrix")];
            frames.extend((0..5).map(|_| paste(&fixture.alpha, b"x".to_vec())));
            frames.push(FrameKind::Ping { nonce: 9 });
            let outcome = exchange(&fixture.state, script(&frames), None, |frames| {
                frames
                    .iter()
                    .any(|frame| matches!(frame, FrameKind::Pong { nonce: 9 }))
            })
            .await;
            assert_eq!(
                outcome.denials(None),
                1,
                "five refused pastes inside a second earn one uncorrelated error: {:?}",
                outcome.frames
            );
            assert!(outcome.ponged(9), "the connection kept serving");
            assert!(
                !outcome.closed && !outcome.detached(),
                "{:?}",
                outcome.frames
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn apply_input_on_the_input_lane_is_guarded() {
    LocalSet::new()
        .run_until(async {
            let apply = |terminal_id: WireResourceId| {
                command(
                    3,
                    Command::ApplyInput {
                        operation_id: InputOperationId::new([4; 16]).unwrap(),
                        terminal_id,
                        events: vec![InputEvent::Focus(FocusEvent::Gained)],
                    },
                )
            };

            let refused = paired(&["observe@global"]);
            let lane = spawn_input_lane(refused.state.clone()).unwrap();
            let outcome = exchange(
                &refused.state,
                script(&[hello("scope-matrix"), apply(refused.alpha.clone())]),
                Some(lane.handle()),
                has_result(3),
            )
            .await;
            assert!(
                is_permission_denied(outcome.result(3)),
                "the lane route ran without INPUT: {:?}",
                outcome.frames
            );
            drop(lane);

            let admitted = paired(&["input@global"]);
            let lane = spawn_input_lane(admitted.state.clone()).unwrap();
            let outcome = exchange(
                &admitted.state,
                script(&[hello("scope-matrix"), apply(admitted.alpha.clone())]),
                Some(lane.handle()),
                has_result(3),
            )
            .await;
            let result = outcome.result(3);
            assert!(result.is_some(), "{:?}", outcome.frames);
            assert!(
                !is_permission_denied(result),
                "INPUT reaches the lane, which answers for the Terminal itself: {result:?}"
            );
            drop(lane);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn relayed_command_is_authorized_on_the_hub_before_forwarding() {
    LocalSet::new()
        .run_until(async {
            let kill = command(
                5,
                Command::KillResource {
                    terminal_id: WireResourceId::satellite("devbox", 5),
                    operation_id: None,
                },
            );
            for (scope, want) in [
                ("observe@global", ErrorCode::PermissionDenied),
                // Authorized, the command reaches the relay branch, which a
                // server that is not a hub for `devbox` refuses.
                ("signal@host:devbox", ErrorCode::UnsupportedSatelliteRoute),
            ] {
                let fixture = paired(&[scope]);
                let outcome = exchange(
                    &fixture.state,
                    script(&[hello("scope-matrix"), kill.clone()]),
                    None,
                    has_result(5),
                )
                .await;
                assert!(
                    matches!(outcome.result(5), Some(CommandResult::Error { code, .. }) if *code == want),
                    "{scope}: {:?}",
                    outcome.frames
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn owner_addressed_spawn_with_disagreeing_payload_group_is_spawn_failed_after_authorization()
{
    LocalSet::new()
        .run_until(async {
            let fixture = fixture(Engine::Fixed, |t| {
                vec![format!("create,bind@group:{}", t.alpha_group)]
            });
            let before = fixture.state.with(|s| s.registry().terminal_count());
            let frames = [
                hello("scope-matrix"),
                spawn_owned_by(4, 7, &fixture.alpha),
                spawn_owned_by(5, 1, &fixture.beta),
            ];
            let outcome = exchange(&fixture.state, script(&frames), None, |frames| {
                let answered = |id: u32| {
                    frames.iter().any(|frame| {
                        matches!(frame, FrameKind::ResourceSpawned { request_id, .. } if *request_id == id)
                    })
                };
                answered(4) && answered(5)
            })
            .await;
            assert!(
                matches!(
                    outcome.spawned(4),
                    Some(SpawnResult::Err(SpawnError::GroupNotFound))
                ),
                "authorized on the owner's Group, then refused for the payload Group: {:?}",
                outcome.frames
            );
            assert!(
                matches!(
                    outcome.spawned(5),
                    Some(SpawnResult::Err(SpawnError::SpawnFailed(reason))) if reason == "permission denied"
                ),
                "an owner outside the grant is refused with the spawn's own reply: {:?}",
                outcome.frames
            );
            assert_eq!(
                fixture.state.with(|s| s.registry().terminal_count()),
                before,
                "nothing was created"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn denied_move_answers_with_its_own_refusal() {
    LocalSet::new()
        .run_until(async {
            let fixture = paired(&["observe@global"]);
            let moving = FrameKind::MoveResource {
                request_id: 6,
                terminal: fixture.alpha.clone(),
                owner_terminal: fixture.beta.clone(),
            };
            let outcome = exchange(&fixture.state, script(&[hello("scope-matrix"), moving]), None, |frames| {
                frames
                    .iter()
                    .any(|frame| matches!(frame, FrameKind::ResourceMoved { request_id: 6, .. }))
            })
            .await;
            let moved = outcome.frames.iter().find_map(|frame| match frame {
                FrameKind::ResourceMoved {
                    request_id: 6,
                    result,
                } => Some(result),
                _ => None,
            });
            assert!(
                matches!(
                    moved,
                    Some(MoveResult::Err(MoveError::MoveFailed(reason))) if reason == "permission denied"
                ),
                "{:?}",
                outcome.frames
            );
            assert_eq!(outcome.denials(Some(6)), 0, "no second, generic refusal");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn absent_and_unauthorized_targets_get_the_same_denial() {
    LocalSet::new()
        .run_until(async {
            let fixture = fixture(Engine::Fixed, |t| {
                vec![format!("*@group:{}", t.alpha_group)]
            });
            let frames = [
                hello("scope-matrix"),
                get_screen(6, fixture.beta.clone()),
                get_screen(7, WireResourceId::local(999)),
            ];
            let outcome = exchange(&fixture.state, script(&frames), None, |frames| {
                has_result(6)(frames) && has_result(7)(frames)
            })
            .await;
            let unauthorized = outcome.result(6);
            let absent = outcome.result(7);
            assert!(is_permission_denied(unauthorized), "{:?}", outcome.frames);
            assert_eq!(
                format!("{unauthorized:?}"),
                format!("{absent:?}"),
                "the reply must not tell an absent Terminal from a foreign one"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn handler_bypass_canary_unclassified_frame_is_denied() {
    LocalSet::new()
        .run_until(async {
            // Even every verb at Global does not open a default-deny row.
            let fixture = paired(&["*@global"]);
            let frames = [
                hello("scope-matrix"),
                FrameKind::Pong { nonce: 1 },
                FrameKind::Ping { nonce: 2 },
            ];
            let outcome = exchange(&fixture.state, script(&frames), None, |frames| {
                frames
                    .iter()
                    .any(|frame| matches!(frame, FrameKind::Pong { nonce: 2 }))
            })
            .await;
            assert_eq!(outcome.denials(None), 1, "{:?}", outcome.frames);
            assert!(outcome.ponged(2));
            assert!(!outcome.closed);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_is_denied_before_its_handler() {
    LocalSet::new()
        .run_until(async {
            let fixture = paired(&["signal@global"]);
            let outcome = exchange(
                &fixture.state,
                script(&[hello("scope-matrix"), command(8, Command::Shutdown)]),
                None,
                has_result(8),
            )
            .await;
            assert!(
                matches!(
                    outcome.result(8),
                    Some(CommandResult::Error {
                        code: ErrorCode::PermissionDenied,
                        message,
                    }) if message == "permission denied"
                ),
                "the guard, not the handler's own owner-socket check, refused it: {:?}",
                outcome.frames
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn whoami_reports_the_effective_grant() {
    LocalSet::new()
        .run_until(async {
            let fixture = paired(&["observe@global", "input@host:devbox"]);
            let outcome = exchange(
                &fixture.state,
                script(&[hello("scope-matrix"), whoami(9)]),
                None,
                has_metadata(9),
            )
            .await;
            let record = outcome.whoami(9);
            assert_eq!(record["credential_id"], fixture.credential_id.as_str());
            assert_eq!(
                record["grant"],
                serde_json::json!([
                    { "verbs": ["observe"], "selector": "global" },
                    { "verbs": ["input"], "selector": "host:devbox" },
                ]),
                "the registry ceiling, not the cached `*@global`"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_narrowly_scoped_workload_reads_its_own_whoami() {
    LocalSet::new()
        .run_until(async {
            for (engine, ceiling) in [(Engine::Registry, None), (Engine::Fixed, Some(()))] {
                let fixture = fixture(engine, |t| match ceiling {
                    None => vec!["observe@host:x".to_owned()],
                    Some(()) => vec![format!("input@terminal:{}", t.alpha)],
                });
                let outcome = exchange(
                    &fixture.state,
                    script(&[hello("scope-matrix"), whoami(10)]),
                    None,
                    has_metadata(10),
                )
                .await;
                let record = outcome.whoami(10);
                assert_eq!(
                    record["grant"].as_array().map(Vec::len),
                    Some(1),
                    "{record}"
                );
                assert_eq!(outcome.denials(Some(10)), 0);
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_hello_on_a_paired_connection_still_closes() {
    LocalSet::new()
        .run_until(async {
            let fixture = paired(&["*@global"]);
            let outcome = exchange(
                &fixture.state,
                script(&[hello("scope-matrix"), hello("scope-matrix")]),
                None,
                |_| false,
            )
            .await;
            assert!(outcome.closed, "HELLO is valid only in PRE_HELLO");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn client_sent_frame_compressed_is_refused_at_decode() {
    LocalSet::new()
        .run_until(async {
            let fixture = paired(&["*@global"]);
            let inner = paste(&fixture.alpha, vec![b'a'; 16 * 1024]);
            let mut scratch = BytesMut::new();
            let mut compressed = BytesMut::new();
            inner.encode_compressed(Compression::Deflate, &mut scratch, &mut compressed);
            assert_eq!(
                compressed[4], TYPE_FRAME_COMPRESSED,
                "the envelope is on the wire"
            );
            let outcome = exchange(
                &fixture.state,
                vec![encode(&hello("scope-matrix")), compressed],
                None,
                |_| false,
            )
            .await;
            assert!(outcome.closed, "{:?}", outcome.frames);
            assert!(outcome.frames.iter().any(|frame| matches!(
                frame,
                FrameKind::Error {
                    code: ErrorCode::MalformedMessage,
                    ..
                }
            )));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_owner_socket_keeps_todays_behaviour() {
    LocalSet::new()
        .run_until(async {
            // Local and transitional servers give every connection the
            // owner's grant: a wrong-direction frame still closes the
            // connection, as it did before the guard existed.
            let state = SharedState::new();
            state.with_mut(|s| {
                s.set_peer_identity(
                    CLIENT,
                    PeerIdentity {
                        uid: 501,
                        ..owner_peer()
                    },
                );
            });
            let outcome = exchange(
                &state,
                script(&[hello("scope-matrix"), FrameKind::Pong { nonce: 1 }]),
                None,
                |_| false,
            )
            .await;
            assert!(outcome.closed);
            assert_eq!(outcome.denials(None), 0, "{:?}", outcome.frames);
        })
        .await;
}

/// workload-auth §6 `GET_STATE { SERVER }` over the client loop: a Group
/// grant holding INVENTORY is admitted and answered with its own session
/// and Terminal only; a grant without INVENTORY is refused.
#[tokio::test(flavor = "current_thread")]
async fn get_state_answers_with_only_what_the_grant_inventories() {
    LocalSet::new()
        .run_until(async {
            let fixture = fixture(Engine::Fixed, |topology| {
                vec![format!("inventory@group:{}", topology.alpha_group)]
            });
            let get_state = command(
                1,
                Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            );
            let outcome = exchange(
                &fixture.state,
                script(&[hello("scope-matrix"), get_state]),
                None,
                has_result(1),
            )
            .await;
            let Some(CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(
                snapshot,
            ))) = outcome.result(1)
            else {
                panic!("GET_STATE answered: {:?}", outcome.frames);
            };
            let ids: Vec<_> = snapshot.resources.iter().map(|r| r.id.clone()).collect();
            assert_eq!(ids, vec![fixture.alpha.clone()], "never beta");
            assert_eq!(snapshot.sessions.len(), 1, "{:?}", snapshot.sessions);
            assert!(!ids.contains(&fixture.beta));

            let blind = fixture_with(Engine::Fixed, "observe@global");
            let get_state = command(
                2,
                Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            );
            let outcome = exchange(
                &blind.state,
                script(&[hello("scope-matrix"), get_state]),
                None,
                has_result(2),
            )
            .await;
            assert!(
                is_permission_denied(outcome.result(2)),
                "{:?}",
                outcome.frames
            );
        })
        .await;
}

fn fixture_with(engine: Engine, scope: &str) -> Fixture {
    let scope = scope.to_owned();
    fixture(engine, |_| vec![scope])
}

/// workload-auth §6 `ATTACH`: the `ATTACHED` snapshot a Group grant receives
/// holds its own session and Terminal, never another session's.
#[test]
fn an_attach_snapshot_holds_only_observable_terminals() {
    let mut scope = String::new();
    let fixture = fixture(Engine::Fixed, |topology| {
        scope = format!("bind,observe@group:{}", topology.alpha_group);
        vec![scope.clone()]
    });
    // The grant HELLO would mint, set directly: this drives the attach
    // below the guard.
    let grant =
        ConnectionGrant::scoped(TerminalScopeSet::parse_all(&[scope]).unwrap(), None).unwrap();
    fixture
        .state
        .with_mut(|s| s.set_connection_grant(CLIENT, grant));
    let alpha = fixture
        .state
        .with(|s| s.find_session_by_name("alpha"))
        .unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let (snapshot, ..) = super::commands::prepare_attach(
        &fixture.state,
        CLIENT,
        alpha,
        &tx,
        ClientCapabilities::default(),
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    )
    .expect("attached");
    let ids: Vec<_> = snapshot.resources.iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids, vec![fixture.alpha]);
    assert!(
        snapshot
            .sessions
            .iter()
            .all(|session| session.name == "alpha"),
        "{:?}",
        snapshot.sessions
    );
}

/// The race workload-auth §5 forbids: the guard authorizes session `work`,
/// and while the attach awaits, `work` is renamed away and another session
/// takes its name. The attach follows the id the guard pinned.
#[test]
fn a_pinned_attach_follows_the_session_id_through_a_rename_race() {
    let state = SharedState::new();
    let (work, secret) = state.with_mut(|s| (s.seed_session("work").0, s.seed_session("secret").0));
    let pinned = state
        .with(|s| {
            crate::policy::resolve_attach_session(s, &AttachTarget::ByName("work".to_owned()))
        })
        .expect("the guard resolves `work`");
    assert_eq!(pinned, work);

    state.with_mut(|s| {
        let _ = s.rename_session("work", "tmp");
        let _ = s.rename_session("secret", "work");
    });
    assert_eq!(state.with(|s| s.find_session_by_name("work")), Some(secret));

    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    super::commands::prepare_attach(
        &state,
        CLIENT,
        pinned,
        &tx,
        ClientCapabilities::default(),
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    )
    .expect("the pinned session is still live");
    let attached = state.with(|s| s.attached().get(&CLIENT).map(|client| client.session));
    assert_eq!(
        attached,
        Some(work),
        "the session the guard authorized, now named `tmp`, not the one now called `work`"
    );
}
