//! A kind outside the client's `HELLO` `RESOURCE_KINDS` is never streamed
//! to it: session attach leaves it in the snapshot without a bootstrap, an
//! explicit `ATTACH_RESOURCE` is refused, and the connection stays.

use super::*;
use phux_protocol::caps::ResourceKindSet;
use phux_protocol::ids::ResourceKind;
use phux_protocol::wire::frame::{Command, CommandResult};

/// Drop the trailing `RESOURCE_KINDS` field the encoder always writes last,
/// leaving the `HELLO` a client from before the field sends.
pub(super) fn strip_resource_kinds(frame: &mut BytesMut) {
    const FIELD: [u8; 3] = [11, 4, 8];
    let tail = frame.len() - 11;
    assert_eq!(
        frame[tail..tail + 3],
        FIELD,
        "RESOURCE_KINDS is the last field"
    );
    frame.truncate(tail);
    let body = u32::try_from(frame.len() - 4).unwrap();
    frame[..4].copy_from_slice(&body.to_be_bytes());
}

fn surface(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    session: phux_core::ids::SessionId,
) -> (phux_core::ids::ResourceId, ResourceHandle) {
    document::<SurfaceDocument>(
        state,
        parent,
        session,
        LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
            target: "pixels".to_owned(),
        }),
    )
}

fn png() -> Bytes {
    let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    png.extend_from_slice(&1_u32.to_be_bytes());
    png.extend_from_slice(&1_u32.to_be_bytes());
    Bytes::from(png)
}

#[tokio::test(flavor = "current_thread")]
async fn an_undecodable_kind_is_listed_but_never_streamed_or_attached() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, false);
            let (tree, tree_handle) = tree(&state, parent, session);
            publish::<TreeDocument>(&tree_handle, Bytes::from_static(b"{}")).await;
            let (surface, surface_handle) = surface(&state, parent, session);
            publish::<SurfaceDocument>(&surface_handle, png()).await;
            let (tree_wire, surface_wire) = state.with_mut(|s| {
                (
                    s.intern_resource_wire(tree),
                    s.intern_resource_wire(surface),
                )
            });
            let kinds = ResourceKindSet::with(&[ResourceKind::AccessibilityTree]);
            let mut connection = Connection::start_with(&state, false, 64, Some(kinds)).await;
            connection.attach(1).await;

            let FrameKind::Attached { snapshot, .. } = connection.next().await else {
                panic!("ATTACHED")
            };
            assert!(
                snapshot.resources.iter().any(|r| r.id == surface_wire),
                "the surface is still listed"
            );
            let mut bootstrapped = Vec::new();
            loop {
                match connection.next().await {
                    FrameKind::BootstrapBegin { terminal_id, .. } => bootstrapped.push(terminal_id),
                    FrameKind::BootstrapChunk { terminal_id, .. }
                    | FrameKind::BootstrapReady { terminal_id, .. } => {
                        assert_eq!(terminal_id, tree_wire);
                    }
                    FrameKind::AttachReady { attach_id: 1 } => break,
                    frame => panic!("unexpected aggregate frame: {frame:?}"),
                }
            }
            assert_eq!(bootstrapped, std::slice::from_ref(&tree_wire));
            state.with(|s| {
                assert!(
                    !s.subscribers_for_resource(surface)
                        .contains(&connection.client)
                );
                assert!(
                    s.subscribers_for_resource(tree)
                        .contains(&connection.client)
                );
            });

            publish::<SurfaceDocument>(&surface_handle, png()).await;
            publish::<TreeDocument>(&tree_handle, Bytes::from_static(b"{\"live\":1}")).await;
            assert!(matches!(
                connection.next().await,
                FrameKind::ResourceOutput { terminal_id, .. } if terminal_id == tree_wire
            ));

            connection
                .send(FrameKind::Command {
                    request_id: 5,
                    command: Command::attach_terminal(surface_wire, None),
                })
                .await;
            assert!(matches!(
                connection.next().await,
                FrameKind::CommandResult {
                    request_id: 5,
                    result: CommandResult::Error {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    },
                }
            ));
            assert!(!state.with(|s| {
                s.subscribers_for_resource(surface)
                    .contains(&connection.client)
            }));
            connection.send(FrameKind::Ping { nonce: 3 }).await;
            assert!(matches!(
                connection.next().await,
                FrameKind::Pong { nonce: 3 }
            ));
            connection.finish().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_hello_without_resource_kinds_streams_every_legacy_kind() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, false);
            let (_, tree_handle) = tree(&state, parent, session);
            publish::<TreeDocument>(&tree_handle, Bytes::from_static(b"{}")).await;
            let (_, surface_handle) = surface(&state, parent, session);
            publish::<SurfaceDocument>(&surface_handle, png()).await;
            let mut connection = Connection::start_with(&state, false, 64, None).await;
            connection.attach(1).await;
            let profiles = prefix(&mut connection, 2, 1).await;
            assert!(profiles.contains(&BootstrapStreamProfile::AccessibilityTreeJsonV1));
            assert!(profiles.contains(&BootstrapStreamProfile::PixelSurfacePngV1));
            assert_eq!(
                state.with(|s| s.attached()[&connection.client].client_caps.resource_kinds),
                ResourceKindSet::LEGACY
            );
            connection.finish().await;
        })
        .await;
}
