use super::*;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::caps::BootstrapProfile;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::caps::Compression;
use phux_protocol::caps::ServerFeatureSet;
use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use std::sync::Arc;

fn paste(data: &[u8]) -> InputEvent {
    InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: data.to_vec(),
    })
}

#[tokio::test(flavor = "current_thread")]
async fn receipt_refusal_precedes_operation_cache_admission() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let lane_owner = crate::runtime::input_lane::spawn_input_lane(state.clone()).unwrap();
            let lane = lane_owner.handle();
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
            let token = CancellationToken::new();
            let mut command_tasks = crate::runtime::command_tasks::CommandTasks::new(token.clone());
            let mut receipts = JoinSet::new();
            let slots = Arc::new(tokio::sync::Semaphore::new(0));
            let operation_id = InputOperationId::new([0x44; 16]).unwrap();
            CommandDispatch {
                state: &state,
                client_id: ClientId(1),
                out_tx: &out_tx,
                input_lane: Some(&lane),
                token: &token,
                root_token: &token,
                selection: NegotiatedConnection {
                    client_caps: ClientCapabilities::default(),
                    profile: BootstrapProfile::SynthesizedVtRaw,
                    limits: BootstrapLimits::default(),
                    server_features: ServerFeatureSet::new(),
                    compression: Compression::None,
                },
                command_tasks: &mut command_tasks,
                input_receipts: &mut receipts,
                input_receipt_slots: &slots,
                input_credits: &mut InputCredits::default(),
                held_commands: &mut JoinSet::new(),
            }
            .submit_input(
                &lane,
                41,
                Command::ApplyInput {
                    operation_id,
                    terminal_id: WireResourceId::local(1),
                    events: vec![paste(b"original")],
                },
            )
            .await;
            assert!(matches!(
                out_rx.recv().await,
                Some(Outbound::Frame(FrameKind::CommandResult {
                    request_id: 41,
                    result: CommandResult::Error {
                        code: ErrorCode::ResourceExhausted,
                        ..
                    },
                }))
            ));
            assert!(receipts.is_empty());
            // A different payload with the refused id must reach destination
            // validation, rather than conflict with a prematurely claimed id.
            let result = lane
                .begin_apply(
                    &state,
                    ClientId(1),
                    operation_id,
                    WireResourceId::local(2),
                    vec![paste(b"replacement")],
                )
                .await;
            assert!(
                matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::TerminalNotFound,
                        ..
                    }
                ),
                "{result:?}"
            );
            drop(lane);
            drop(lane_owner);
            command_tasks.shutdown().await;
            drop(command_tasks);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_blocked_receipt_releases_completion_capacity() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let lane_owner = crate::runtime::input_lane::spawn_input_lane(state.clone()).unwrap();
            let lane = lane_owner.handle();
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
            out_tx
                .try_send(Outbound::Frame(FrameKind::Pong { nonce: 7 }))
                .unwrap();
            let slots = Arc::new(tokio::sync::Semaphore::new(1));
            let receipt = lane.begin_apply(
                &state,
                ClientId(1),
                InputOperationId::new([0x45; 16]).unwrap(),
                WireResourceId::local(1),
                vec![],
            );
            let mut receipts = JoinSet::new();
            spawn_input_receipt(
                &mut receipts,
                slots.clone().try_acquire_owned().unwrap(),
                out_tx.clone(),
                42,
                receipt,
            );
            tokio::task::yield_now().await;
            assert_eq!(slots.available_permits(), 0);
            receipts.shutdown().await;
            assert_eq!(slots.available_permits(), 1);
            assert!(matches!(
                out_rx.try_recv(),
                Ok(Outbound::Frame(FrameKind::Pong { nonce: 7 }))
            ));
            assert!(
                out_rx.try_recv().is_err(),
                "aborted receipt must not publish later"
            );
            drop(lane);
            drop(lane_owner);
        })
        .await;
}
