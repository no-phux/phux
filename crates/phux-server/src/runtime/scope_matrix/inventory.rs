//! What a grant reports and inventories: `whoami`, `GET_STATE`, and
//! `ATTACHED` snapshots.

use bytes::BytesMut;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities, Compression};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, ErrorCode, FrameKind, TYPE_FRAME_COMPRESSED,
};
use tokio::task::LocalSet;

use crate::policy::ConnectionGrant;
use crate::runtime::test_support::{encode, hello, owner_peer};
use crate::state::SharedState;

use super::{
    CLIENT, Engine, Fixture, command, exchange, fixture, has_metadata, has_result,
    is_permission_denied, paired, paste, script, whoami,
};

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
            // The default (`local`) engine gives the owner's socket the
            // owner's grant: a wrong-direction frame still closes the
            // connection, as it did before the guard existed.
            let state = SharedState::new();
            state.with_mut(|s| s.set_peer_identity(CLIENT, owner_peer()));
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
    let (snapshot, ..) = super::super::commands::prepare_attach(
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
    super::super::commands::prepare_attach(
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
