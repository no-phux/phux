//! Metadata reads: refusal versus unset, key round trips, and pipelining.

use super::*;

fn metadata_value(request_id: u32, value: Option<&[u8]>) -> FrameKind {
    FrameKind::MetadataValue {
        request_id,
        value: value.map(<[u8]>::to_vec),
    }
}

fn metadata_against(script: Vec<FrameKind>) -> Reply<Answer<Option<Vec<u8>>>> {
    block_on(async {
        let mut client = scripted(script);
        tokio::time::timeout(
            WEDGE_TIMEOUT,
            client.request_metadata(7, Scope::Resource(ResourceId::local(1)), "k".to_owned()),
        )
        .await
        .expect("the read must resolve")
        .expect("metadata reply")
    })
}

/// A correlated refusal answers a metadata read, and stays distinct from an
/// unset key; an uncorrelated notice is kept.
#[test]
fn metadata_refusal_differs_from_unset_and_notices_are_kept() {
    let reply = metadata_against(vec![refusal(7)]);
    let refused = reply.result().as_ref().expect_err("refused");
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    let unset = metadata_against(vec![metadata_value(7, None)]);
    assert_eq!(unset.result().as_ref().ok(), Some(&None));
    let notice = FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "satellite build-box is unreachable".to_owned(),
    };
    let noted = metadata_against(vec![notice, metadata_value(7, Some(b"v"))]);
    assert_eq!(noted.result().as_ref().ok(), Some(&Some(b"v".to_vec())));
    assert!(matches!(
        noted.interleaved(),
        [FrameKind::Error {
            request_id: None,
            ..
        }]
    ));
}

#[test]
fn metadata_keys_round_trip() {
    let reply = block_on(async {
        let mut client = scripted(vec![FrameKind::MetadataKeys {
            request_id: 4,
            keys: vec!["a".to_owned(), "b".to_owned()],
        }]);
        client
            .request_metadata_keys(4, Scope::Resource(ResourceId::local(1)))
            .await
            .expect("keys")
    });
    assert_eq!(
        reply.into_result_ignoring_interleaved().expect("answered"),
        vec!["a".to_owned(), "b".to_owned()]
    );
}

/// A swarm's metadata reads cost one round trip per [`METADATA_WINDOW`].
/// The server answers nothing until a whole window has arrived, backwards,
/// refusing every seventh read and pushing a notice per window.
#[test]
fn metadata_reads_pipeline_one_round_trip_per_window() {
    const READS: usize = 300;
    let (answers, interleaved, round_trips) = block_on(async {
        let (client, server) = UnixStream::pair().expect("pair");
        let mut server = Session::from_stream(server);
        let server = tokio::task::spawn_local(async move {
            let mut round_trips = 0;
            let mut received = 0;
            while received < READS {
                let window = METADATA_WINDOW.min(READS - received);
                let mut ids = Vec::with_capacity(window);
                while ids.len() < window {
                    match server.recv().await.expect("read") {
                        FrameKind::GetMetadata { request_id, .. } => ids.push(request_id),
                        other => panic!("expected GET_METADATA, got {other:?}"),
                    }
                }
                received += window;
                round_trips += 1;
                server
                    .send(&FrameKind::Error {
                        request_id: None,
                        code: ErrorCode::UnsupportedSatelliteRoute,
                        message: "edge is unreachable".to_owned(),
                    })
                    .await
                    .expect("notice");
                for request_id in ids.into_iter().rev() {
                    let reply = if request_id % 7 == 0 {
                        refusal(request_id)
                    } else {
                        metadata_value(request_id, Some(&request_id.to_le_bytes()))
                    };
                    server.send(&reply).await.expect("answer");
                }
            }
            round_trips
        });
        let mut client = Session::from_stream(client);
        let reads = (0..READS)
            .map(|n| {
                let pane = ResourceId::local(u32::try_from(n).expect("small"));
                (Scope::Resource(pane), "phux.agent/v1".to_owned())
            })
            .collect();
        let (answers, interleaved) =
            tokio::time::timeout(WEDGE_TIMEOUT, client.request_metadata_many(100, reads))
                .await
                .expect("a timeout here is a read per round trip")
                .expect("metadata replies")
                .into_parts();
        drop(client);
        (answers, interleaved, server.await.expect("server"))
    });
    assert_eq!(round_trips, READS.div_ceil(METADATA_WINDOW));
    assert_eq!(answers.len(), READS);
    for (index, answer) in answers.iter().enumerate() {
        let request_id = 100 + u32::try_from(index).expect("small");
        if request_id % 7 == 0 {
            assert!(answer.is_err(), "read {index} was refused");
        } else {
            assert_eq!(
                answer.as_ref().expect("answered").as_deref(),
                Some(&request_id.to_le_bytes()[..])
            );
        }
    }
    assert_eq!(interleaved.len(), round_trips, "one notice per window");
}
