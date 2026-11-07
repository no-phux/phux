//! Denials at the guard: rate limits, the input lane, relays, and
//! refusals that must not leak which check failed.

use phux_protocol::ids::{InputOperationId, ResourceId as WireResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, MoveError, MoveResult, SpawnError, SpawnResult,
};
use tokio::task::LocalSet;

use crate::runtime::apps::terminal::input_lane::spawn_input_lane;
use crate::runtime::test_support::hello;

use super::{
    Engine, command, exchange, fixture, get_screen, has_result, is_permission_denied, paired,
    paste, script, spawn_owned_by,
};

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
