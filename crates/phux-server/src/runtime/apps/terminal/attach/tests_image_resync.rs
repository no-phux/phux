use super::*;
use phux_terminal::actor::TerminalResource as _;

#[tokio::test(flavor = "current_thread")]
async fn image_resync_recaptures_resident_payload_and_its_own_geometry_and_cut() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let seed = format!(
                "visible\x1b_Ga=t,f=32,s=16,v=12,i=9,q=2;{}\x1b\\",
                "AAAA".repeat(256)
            );
            let bundle =
                phux_terminal::TerminalActor::new_with_seed(10, 5, seed.as_bytes()).unwrap();
            let terminal = bundle.handle.terminal().unwrap().clone();
            let token = bundle.token.clone();
            let mut output = bundle.handle.output.subscribe();
            let run = tokio::task::spawn_local(bundle.actor.run());
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(16);
            let ctx = OutputPumpContext {
                out_tx,
                resize: terminal.resize.clone(),
                wire_terminal_id: phux_protocol::ids::ResourceId::local(1),
                stream_id: StreamId::new(1).unwrap(),
                initial_bootstrap_id: BootstrapId::new(1).unwrap(),
                client_id: ClientId(1),
                client_caps: ClientCapabilities::default()
                    .with_image_protocols(phux_protocol::caps::ImageProtocolSet::all()),
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                limits: BootstrapLimits::default(),
                lag_label: "image resync test",
                stale_skip: true,
                cancel: None,
                drain: None,
                snapshot: Some(terminal.snapshot.clone()),
                last_seq: None,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                terminal,
            };
            let mut generation = PumpGeneration::opened_at(0, BootstrapId::new(1).unwrap());
            let old = PaneResync {
                cols: 1,
                rows: 1,
                bytes: bytes::Bytes::from_static(b"old cut without images"),
                base_seq: 999,
                reason: crate::resource::ResyncReason::Resize,
            };
            assert!(
                ctx.republish_generation(&mut generation, &mut output, &old)
                    .await
                    .is_continue()
            );
            let mut receiver = libghostty_vt::Terminal::new(10, 5).unwrap();
            phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut receiver)
                .unwrap();
            let mut began = false;
            let mut ready = false;
            while let Ok(Outbound::Frame(frame)) = out_rx.try_recv() {
                match frame {
                    FrameKind::BootstrapBegin {
                        cols,
                        rows,
                        base_seq,
                        ..
                    } => {
                        assert_eq!((cols, rows, base_seq), (10, 5, 0));
                        began = true;
                    }
                    FrameKind::BootstrapChunk { payload, .. } => receiver.vt_write(&payload),
                    FrameKind::BootstrapReady { .. } => ready = true,
                    _ => {}
                }
            }
            assert!(began && ready);
            assert_eq!(
                receiver
                    .kitty_graphics()
                    .unwrap()
                    .image(9)
                    .unwrap()
                    .data()
                    .unwrap()
                    .unwrap(),
                &[0; 768],
            );
            assert!(!generation.forwards(0));
            assert!(
                generation.forwards(1),
                "forwarding uses the recaptured cut, not stale trigger cut"
            );
            token.cancel();
            run.await.unwrap();
        })
        .await;
}
