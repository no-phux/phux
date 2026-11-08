//! The `PixelSurface` resource kind over the production dispatch loop
//! (`docs/resource-kinds.md`, `phux-phiio.7`): spawn under a Terminal,
//! publish whole PNG or JPEG images, and bootstrap a late viewer onto the newest.

use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnResource, SpawnResult,
};
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::{Spawn, command, send_frame, spawn_resource};
use tokio::net::UnixStream;

use crate::common::{Server, attach, sh, spawned, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";

/// A PNG's signature and `IHDR` chunk for a `width` x `height` image, plus
/// one byte of `tag` so two images of one size differ.
fn png(width: u32, height: u32, tag: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, tag]);
    bytes
}

async fn publish(
    stream: &mut UnixStream,
    request_id: u32,
    surface: &ResourceId,
    image: Vec<u8>,
) -> CommandResult {
    command(
        stream,
        request_id,
        Command::PublishResourceState {
            terminal_id: surface.clone(),
            bytes: image,
        },
    )
    .await
}

/// The viewer's bootstrap of `surface`: its profile, `base_seq`, and the
/// image its chunks carry.
async fn bootstrap(
    stream: &mut UnixStream,
    surface: &ResourceId,
) -> (BootstrapStreamProfile, u64, Vec<u8>) {
    let mut begin = None;
    let mut chunks = Vec::new();
    wait_frame(stream, "surface bootstrap", |frame| match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            profile,
            base_seq,
            cols,
            rows,
            ..
        } if &terminal_id == surface => {
            assert_eq!((cols, rows), (0, 0), "a surface's geometry is its image's");
            begin = Some((profile, base_seq));
            None
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            ..
        } if &terminal_id == surface => {
            chunks.extend_from_slice(&payload);
            None
        }
        FrameKind::BootstrapReady { terminal_id, .. } if &terminal_id == surface => Some(()),
        _ => None,
    })
    .await;
    let (profile, base_seq) = begin.expect("BOOTSTRAP_BEGIN");
    (profile, base_seq, chunks)
}

#[test]
fn a_surface_takes_only_images_and_a_late_viewer_sees_the_newest() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let spawn = Spawn {
            resource: Some(Box::new(SpawnResource::pixel_surface(
                parent.clone(),
                "window:Safari",
            ))),
            ..Spawn::default()
        };
        let SpawnResult::Ok(surface) = spawn_resource(&mut owner, 2, spawn).await else {
            panic!("spawn surface failed");
        };
        let (profile, base_seq, image) = bootstrap(&mut owner, &surface).await;
        assert_eq!(profile, BootstrapStreamProfile::PixelSurfacePngV1);
        assert_eq!((base_seq, image.len()), (0, 0));

        // JSON is a document, but not an image.
        let refused = publish(&mut owner, 10, &surface, b"{\"role\":\"window\"}".to_vec()).await;
        assert!(
            matches!(
                refused,
                CommandResult::Error {
                    code: ErrorCode::RecordInvalid,
                    ..
                }
            ),
            "{refused:?}"
        );
        for (request_id, tag) in [(11, 1), (12, 2)] {
            let taken = publish(&mut owner, request_id, &surface, png(640, 480, tag)).await;
            assert!(matches!(taken, CommandResult::OkWith(_)), "{taken:?}");
        }

        let mut viewer = server.connect().await;
        send_frame(
            &mut viewer,
            &FrameKind::Command {
                request_id: 20,
                command: Command::attach_terminal(surface.clone(), None),
            },
        )
        .await;
        let (profile, base_seq, image) = bootstrap(&mut viewer, &surface).await;
        assert_eq!(profile, BootstrapStreamProfile::PixelSurfacePngV1);
        assert_eq!(base_seq, 2);
        assert_eq!(image, png(640, 480, 2));
        assert_eq!(
            phux_tree::image_size(&image).map(|s| (s.1, s.2)),
            Ok((640, 480))
        );

        // A JPEG frame header for 800 x 600: the surface's newer image.
        let jpeg = vec![
            0xff, 0xd8, 0xff, 0xc0, 0, 11, 8, 0x02, 0x58, 0x03, 0x20, 1, 1, 0x11, 0,
        ];
        let taken = publish(&mut owner, 13, &surface, jpeg).await;
        assert!(matches!(taken, CommandResult::OkWith(_)), "{taken:?}");
        let (seq, bytes) = wait_frame(&mut viewer, "live image", |frame| match frame {
            FrameKind::ResourceOutput {
                terminal_id,
                seq,
                bytes,
                ..
            } if terminal_id == surface => Some((seq, bytes.to_vec())),
            _ => None,
        })
        .await;
        assert_eq!(seq, 3);
        assert_eq!(
            phux_tree::image_size(&bytes),
            Ok((phux_tree::ImageFormat::Jpeg, 800, 600))
        );

        drop((owner, viewer));
        server.stop().await;
    });
}
