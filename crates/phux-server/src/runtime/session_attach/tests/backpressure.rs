//! A slow aggregate viewer uses the document app's existing recut path.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn aggregate_document_backpressure_recuts_to_a_fresh_generation_then_live() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, false);
            let (_, handle) = tree(&state, parent, session);
            publish::<TreeDocument>(&handle, Bytes::from_static(b"{\"initial\":true}")).await;
            let mut connection = Connection::start(&state, false).await;
            connection.attach(1).await;
            prefix(&mut connection, 1, 1).await;

            // Leave the peer's bounded writer mailbox unread. The producer must
            // keep accepting documents while its viewer blocks and falls behind
            // the broadcast, not just when a synthetic Lagged error is injected.
            let published = phux_resource::output_broadcast_capacity() * 2
                + crate::state::DEFAULT_CLIENT_MAILBOX
                + 1024;
            for _ in 0..published {
                publish::<TreeDocument>(&handle, Bytes::from_static(b"{\"newest\":true}")).await;
            }
            let mut retired = false;
            for _ in 0..=published {
                match connection.next().await {
                    FrameKind::ResourceOutput { bootstrap_id, .. } => {
                        assert_eq!(bootstrap_id.get(), 1);
                    }
                    FrameKind::BootstrapTombstone {
                        bootstrap_id,
                        reason,
                        ..
                    } => {
                        assert_eq!(bootstrap_id.get(), 1);
                        assert_eq!(
                            reason,
                            phux_protocol::wire::frame::TombstoneReason::OutboundGap
                        );
                        retired = true;
                        break;
                    }
                    frame => panic!("unexpected slow-viewer frame: {frame:?}"),
                }
            }
            assert!(
                retired,
                "a lapped document viewer must fence its old generation"
            );
            let FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                base_seq,
                ..
            } = connection.next().await
            else {
                panic!("replacement BEGIN");
            };
            assert_eq!(stream_id, crate::runtime::stream_id_from(1));
            assert_eq!(
                bootstrap_id.get(),
                2,
                "aggregate generation 1 must not be reused"
            );
            assert_eq!(base_seq, u64::try_from(published).unwrap() + 1);
            let FrameKind::BootstrapChunk {
                payload,
                bootstrap_id: chunk_id,
                ..
            } = connection.next().await
            else {
                panic!("replacement document");
            };
            assert_eq!(chunk_id, bootstrap_id);
            assert_eq!(payload.as_ref(), b"{\"newest\":true}");
            assert!(matches!(connection.next().await,
            FrameKind::BootstrapReady { bootstrap_id: ready, .. } if ready == bootstrap_id));
            publish::<TreeDocument>(&handle, Bytes::from_static(b"{\"live_again\":true}")).await;
            assert!(matches!(connection.next().await,
            FrameKind::ResourceOutput { bootstrap_id: live, seq, .. }
                if live == bootstrap_id && seq == base_seq + 1));
            connection.finish().await;
        })
        .await;
}
