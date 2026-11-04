use super::*;
use phux_protocol::caps::ImageProtocolSet;

/// 1024 base64 characters of noise (768 pixel bytes), so the snapshot's
/// zlib payload cannot squeeze under a small budget.
fn noise_base64() -> String {
    const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut state = 0x9e37_79b9_u32;
    (0..1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            char::from(BASE64[(state % 64) as usize])
        })
        .collect()
}

fn image_actor() -> TerminalActorBundle {
    let bundle = TerminalActor::new(10, 5).expect("actor");
    let mut canonical = bundle.actor.terminal.borrow_mut();
    canonical.vt_write(b"visible text");
    let payload = noise_base64();
    canonical.vt_write(format!("\x1b_Ga=t,f=32,s=16,v=12,i=9,q=2;{payload}\x1b\\").as_bytes());
    assert!(
        canonical
            .try_terminal()
            .unwrap()
            .kitty_graphics()
            .unwrap()
            .image(9)
            .is_some()
    );
    drop(canonical);
    bundle
}

fn assert_text_snapshot(snapshot: &SnapshotBytes) {
    let mut receiver = GhosttyTerminal::new(10, 5).unwrap();
    receiver.vt_write(&snapshot.bytes);
    assert_eq!(&render_viewport(&receiver)[..2], &["visible te", "xt"]);
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_snapshot_request_omits_unsupported_resident_images() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let bundle = image_actor();
            let terminal = bundle.handle.terminal().unwrap().clone();
            let token = bundle.token.clone();
            let run = tokio::task::spawn_local(bundle.actor.run());
            for image_protocols in [ImageProtocolSet::all(), ImageProtocolSet::new()] {
                let (reply, receive) = oneshot::channel();
                terminal
                    .snapshot
                    .send(SnapshotRequest {
                        scrollback: None,
                        image_protocols,
                        max_bytes: 512,
                        max_frames: 16,
                        chunk_bytes: 512,
                        reply,
                    })
                    .await
                    .unwrap();
                let result = receive.await.unwrap();
                if image_protocols == ImageProtocolSet::all() {
                    assert!(matches!(
                        result,
                        Err(crate::grid::SynthesisError::LimitExceeded)
                    ));
                } else {
                    assert_text_snapshot(&result.expect("text fits independently of images").0);
                }
            }
            token.cancel();
            run.await.unwrap();
        })
        .await;
}

#[test]
fn atomic_state_sync_registration_omits_unsupported_resident_images() {
    let mut actor = image_actor().actor;
    for image_protocols in [ImageProtocolSet::all(), ImageProtocolSet::new()] {
        let (outbound, _output) = dummy_outbound();
        let (reply, mut receive) = oneshot::channel();
        actor.handle_consumer_attach(ConsumerAttachRequest {
            client_id: ClientId(1),
            outbound,
            wire_terminal_id: 1,
            stream_id: phux_protocol::ids::StreamId::new(1).unwrap(),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).unwrap(),
            wants_state_sync: true,
            state_sync_scrollback: None,
            image_protocols,
            bootstrap_max_bytes: 512,
            bootstrap_max_frames: 16,
            bootstrap_chunk_bytes: 512,
            loss_tolerant: false,
            live_gate: watch::channel(false).1,
            reply,
        });
        let result = receive.try_recv().unwrap();
        if image_protocols == ImageProtocolSet::all() {
            assert!(result.is_err(), "supported image replay must be charged");
        } else {
            let bootstrap = result.unwrap().state_sync_bootstrap.unwrap();
            assert_text_snapshot(&bootstrap.snapshot);
        }
    }
}
