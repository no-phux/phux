//! Document kinds over the production dispatch loop (`docs/resource-kinds.md`):
//! `AccessibilityTree`, `PixelSurface`, and typed `Document` spawn under a
//! Terminal, publish whole documents, bootstrap a late viewer onto the
//! newest, publish their labels as names, and report producer liveness.

use phux_core::image::{ImageFormat, image_size};
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::typed_document::split_bootstrap;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, SpawnError, SpawnResource, SpawnResult,
};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server_testkit::{Spawn, assert_command_error, spawn_resource};

use crate::common::{
    bootstrap, demo_parent, find, next_output, publish, send_command, spawn_kind, wait_for_state,
};

/// A publish the kind accepted (it takes a sequence).
fn assert_taken(result: &CommandResult) {
    assert!(matches!(result, CommandResult::OkWith(_)), "{result:?}");
}

/// A publish the kind refused as an invalid record.
fn assert_invalid(result: &CommandResult) {
    assert_command_error(result, ErrorCode::RecordInvalid);
}

#[test]
fn a_tree_is_spawned_published_and_bootstrapped_at_its_newest_document() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, parent) = demo_parent().await;
        let tree = spawn_kind(
            &mut owner,
            2,
            SpawnResource::accessibility_tree(parent, "Safari"),
        )
        .await;
        // The spawner's own bootstrap: no document yet.
        let (grid, profile, base_seq, document) = bootstrap(&mut owner, &tree).await;
        assert_eq!(grid, (0, 0), "a tree has no grid");
        assert_eq!(profile, BootstrapStreamProfile::AccessibilityTreeJsonV1);
        assert_eq!((base_seq, document.len()), (0, 0));

        // A broken document changes nothing; good ones take sequences.
        assert_invalid(&publish(&mut owner, 10, &tree, "{not json").await);
        for (request_id, doc) in [
            (11, r#"{"role":"window","v":1}"#),
            (12, r#"{"role":"window","v":2}"#),
        ] {
            assert_taken(&publish(&mut owner, request_id, &tree, doc).await);
        }

        // A late viewer gets the newest document only, at its sequence.
        let mut viewer = server.connect().await;
        let attach = Command::attach_agent_session(tree.clone(), None, None);
        send_command(&mut viewer, 20, attach).await;
        let (_, profile, base_seq, document) = bootstrap(&mut viewer, &tree).await;
        assert_eq!(profile, BootstrapStreamProfile::AccessibilityTreeJsonV1);
        assert_eq!(base_seq, 2);
        assert_eq!(document, br#"{"role":"window","v":2}"#);

        // The next document reaches the viewer live.
        let third = publish(&mut owner, 13, &tree, r#"{"role":"window","v":3}"#).await;
        assert!(matches!(third, CommandResult::OkWith(_)), "{third:?}");
        let live = next_output(&mut viewer, &tree).await;
        assert_eq!(live, (3, br#"{"role":"window","v":3}"#.to_vec()));

        // A viewer is not a producer.
        let denied = publish(&mut viewer, 21, &tree, "{}").await;
        assert!(
            matches!(
                denied,
                CommandResult::Error {
                    code: ErrorCode::NotProducer,
                    ..
                } | CommandResult::OkWith(_)
            ),
            "{denied:?}"
        );

        drop((owner, viewer));
        server.stop().await;
    });
}

/// A PNG's signature and `IHDR` chunk for a `width` x `height` image, plus
/// one byte of `tag` so two images of one size differ.
fn png(width: u32, height: u32, tag: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, tag]);
    bytes
}

#[test]
fn a_surface_takes_only_images_and_a_late_viewer_sees_the_newest() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, parent) = demo_parent().await;
        let surface = spawn_kind(
            &mut owner,
            2,
            SpawnResource::pixel_surface(parent, "window:Safari"),
        )
        .await;
        let (grid, profile, base_seq, image) = bootstrap(&mut owner, &surface).await;
        assert_eq!(grid, (0, 0), "a surface's geometry is its image's");
        assert_eq!(profile, BootstrapStreamProfile::PixelSurfacePngV1);
        assert_eq!((base_seq, image.len()), (0, 0));

        // JSON is a document, but not an image.
        assert_invalid(&publish(&mut owner, 10, &surface, b"{\"role\":\"window\"}".to_vec()).await);
        for (request_id, tag) in [(11, 1), (12, 2)] {
            assert_taken(&publish(&mut owner, request_id, &surface, png(640, 480, tag)).await);
        }

        let mut viewer = server.connect().await;
        send_command(
            &mut viewer,
            20,
            Command::attach_terminal(surface.clone(), None),
        )
        .await;
        let (_, profile, base_seq, image) = bootstrap(&mut viewer, &surface).await;
        assert_eq!(profile, BootstrapStreamProfile::PixelSurfacePngV1);
        assert_eq!((base_seq, image.clone()), (2, png(640, 480, 2)));
        assert_eq!(image_size(&image).map(|s| (s.1, s.2)), Ok((640, 480)));

        // A JPEG frame header for 800 x 600: the surface's newer image.
        let jpeg = vec![
            0xff, 0xd8, 0xff, 0xc0, 0, 11, 8, 0x02, 0x58, 0x03, 0x20, 1, 1, 0x11, 0,
        ];
        assert_taken(&publish(&mut owner, 13, &surface, jpeg).await);
        let (seq, bytes) = next_output(&mut viewer, &surface).await;
        assert_eq!(seq, 3);
        assert_eq!(image_size(&bytes), Ok((ImageFormat::Jpeg, 800, 600)));

        drop((owner, viewer));
        server.stop().await;
    });
}

fn document(parent: &ResourceId, content_type: &str) -> SpawnResource {
    SpawnResource::document(parent.clone(), content_type, Some("notes".to_owned()))
}

/// A typed bootstrap: (profile, `base_seq`, content type, document).
async fn typed_bootstrap(
    stream: &mut tokio::net::UnixStream,
    doc: &ResourceId,
) -> (BootstrapStreamProfile, u64, String, Vec<u8>) {
    let (_, profile, base_seq, chunks) = bootstrap(stream, doc).await;
    let (content_type, document) = split_bootstrap(&chunks).expect("a typed bootstrap");
    (
        profile,
        base_seq,
        content_type.to_owned(),
        document.to_vec(),
    )
}

#[test]
fn a_typed_document_carries_its_content_type_to_a_late_viewer() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, parent) = demo_parent().await;
        let doc = spawn_kind(&mut owner, 2, document(&parent, "text/markdown")).await;
        let typed = |base_seq, document: &[u8]| {
            (
                BootstrapStreamProfile::DocumentTypedV1,
                base_seq,
                "text/markdown".to_owned(),
                document.to_vec(),
            )
        };
        assert_eq!(typed_bootstrap(&mut owner, &doc).await, typed(0, b""));
        for (request_id, text) in [(10, "# one"), (11, "# two")] {
            assert_taken(&publish(&mut owner, request_id, &doc, text).await);
        }
        // Markdown is text: bytes that are not UTF-8 are refused.
        assert_invalid(&publish(&mut owner, 12, &doc, b"\xff\xfe".to_vec()).await);

        let mut viewer = server.connect().await;
        send_command(&mut viewer, 20, Command::attach_terminal(doc.clone(), None)).await;
        assert_eq!(typed_bootstrap(&mut viewer, &doc).await, typed(2, b"# two"));
        assert_taken(&publish(&mut owner, 13, &doc, "# three").await);
        assert_eq!(
            next_output(&mut viewer, &doc).await,
            (3, b"# three".to_vec())
        );

        // A JSON document must parse.
        let json = spawn_kind(&mut owner, 3, document(&parent, "application/json")).await;
        let _ = bootstrap(&mut owner, &json).await;
        assert_invalid(&publish(&mut owner, 30, &json, "{not json").await);
        assert_taken(&publish(&mut owner, 31, &json, "{\"ok\":true}").await);

        drop((owner, viewer));
        server.stop().await;
    });
}

/// A document binds under a parent that supervises its producer: an unknown
/// parent is not found, and another document, which supervises nothing, is
/// the wrong kind of parent.
#[test]
fn a_document_under_an_unfit_parent_is_refused() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, parent) = demo_parent().await;
        let doc = spawn_kind(&mut owner, 2, document(&parent, "text/markdown")).await;
        for (request_id, under, refusal) in [
            (3, doc, SpawnError::ParentKindMismatch),
            (4, ResourceId::local(999_999), SpawnError::ParentNotFound),
        ] {
            let spawn = Spawn {
                resource: Some(Box::new(SpawnResource::document(under, "text/plain", None))),
                ..Spawn::default()
            };
            assert_eq!(
                spawn_resource(&mut owner, request_id, spawn).await,
                SpawnResult::Err(refusal)
            );
        }
        drop(owner);
        server.stop().await;
    });
}

/// A tree or surface is named for what it observes, a typed document for
/// its `--name`.
#[test]
fn documents_publish_their_labels_as_names() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, parent) = demo_parent().await;
        let specs = [
            SpawnResource::accessibility_tree(parent.clone(), "Safari"),
            SpawnResource::pixel_surface(parent.clone(), "display"),
            document(&parent, "text/plain"),
        ];
        let mut docs = Vec::new();
        for (request, spec) in (2..).zip(specs) {
            docs.push(spawn_kind(&mut owner, request, spec).await);
        }

        let mut viewer = server.connect().await;
        let names = |s: &_| -> Vec<Option<String>> {
            docs.iter()
                .map(|doc| find(s, doc).and_then(|info| info.title.clone()))
                .collect()
        };
        let want = ["Safari", "display", "notes"].map(|n| Some(n.to_owned()));
        wait_for_state(&mut viewer, 10, "document names", |s| names(s) == want).await;

        drop((viewer, owner));
        server.stop().await;
    });
}

/// Connected while the opening connection lives, gone after it disconnects,
/// back once a new connection publishes, and the newest frame stays readable.
#[test]
fn a_document_reports_its_producer_connected_then_gone() {
    const DOC: &[u8] = br#"{"role":"window"}"#;
    let producer = |snapshot: &SessionSnapshot, doc: &ResourceId| {
        find(snapshot, doc).and_then(|info| info.producer_connected)
    };
    phux_server_testkit::run_local(async {
        let (server, mut owner, parent) = demo_parent().await;
        let tree = spawn_kind(
            &mut owner,
            2,
            SpawnResource::accessibility_tree(parent.clone(), "app:Test"),
        )
        .await;
        assert_taken(&publish(&mut owner, 3, &tree, DOC).await);

        let mut viewer = server.connect().await;
        let up = wait_for_state(&mut viewer, 10, "producer connected", |s| {
            producer(s, &tree) == Some(true)
        })
        .await;
        assert_eq!(producer(&up, &parent), None);

        drop(owner);
        wait_for_state(&mut viewer, 100, "producer gone", |s| {
            producer(s, &tree) == Some(false)
        })
        .await;

        // A producer that redials (a server upgrade closed its connection)
        // takes the same document back by publishing to it.
        let mut redialed = server.connect().await;
        assert_taken(&publish(&mut redialed, 4, &tree, DOC).await);
        wait_for_state(&mut viewer, 200, "producer back", |s| {
            producer(s, &tree) == Some(true)
        })
        .await;
        drop(redialed);

        send_command(
            &mut viewer,
            500,
            Command::attach_terminal(tree.clone(), None),
        )
        .await;
        let (_, _, _, chunks) = bootstrap(&mut viewer, &tree).await;
        assert_eq!(chunks, DOC, "the last frame outlives its producer");

        drop(viewer);
        server.stop().await;
    });
}
