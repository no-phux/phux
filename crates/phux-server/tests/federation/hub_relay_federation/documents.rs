//! A satellite's document resources relay through the hub: the link
//! negotiated a terminal profile, but a tree's stream passes regardless.

use super::*;
use phux_protocol::caps::{BootstrapCapabilities, BootstrapProfileSet, BootstrapStreamProfile};

/// `SEND_DOCUMENT_INPUT` to a satellite document relays through the hub to
/// the satellite producer, and the classes a sender stamps cross the link:
/// the satellite, which knows the document's class, refuses one outside
/// them (`docs/resource-kinds.md#authority`). The hub names the device it
/// relays for, so the producer's `input_activity` shows that device, and a
/// claim the sender wrote itself is replaced (`docs/authority.md`).
#[test]
fn input_to_a_satellite_document_reaches_its_producer_through_the_hub() {
    use phux_protocol::wire::frame::{ControlClass, ControlClasses, DocumentInput};
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut sat = fed.satellite().await;
        let surface = SpawnResource::pixel_surface(ResourceId::local(fed.seed), "Smoke")
            .with_control(Some(ControlClass::Window));
        let spawn = Spawn {
            resource: Some(Box::new(surface)),
            ..Spawn::default()
        };
        let SpawnResult::Ok(ResourceId::Local { id }) = spawn_resource(&mut sat, 1, spawn).await
        else {
            panic!("spawn surface on the satellite failed");
        };
        let mut hub = fed.linked_hub().await;
        let send = |control| Command::SendDocumentInput {
            terminal_id: ResourceId::satellite("sat", id),
            intent: DocumentInput::Click { x: 3, y: 4 },
            control,
            on_behalf_of: Some(phux_protocol::wire::frame::OnBehalfOf {
                credential: "sha256:spoofed".to_owned(),
                name: "Spoofed".to_owned(),
            }),
        };
        let display = Some(ControlClasses::EMPTY.with(ControlClass::Display));
        let refused = phux_server_testkit::command(&mut hub, 2, send(display)).await;
        assert_command_error(&refused, ErrorCode::PermissionDenied);
        let taken = phux_server_testkit::command(&mut hub, 3, send(None)).await;
        assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
        let deadline = Instant::now() + STEP_DEADLINE;
        let (mut origin, mut got) = (None, None);
        while got.is_none() && Instant::now() < deadline {
            match recv_typed(&mut sat).await.1 {
                FrameKind::Event {
                    terminal: Some(ResourceId::Local { id: at }),
                    event: AgentEvent::InputActivity { origin: from },
                    ..
                } if at == id => origin = Some(from),
                FrameKind::Event {
                    terminal: Some(ResourceId::Local { id: at }),
                    event: AgentEvent::DocumentInput { intent },
                    ..
                } if at == id => got = Some(intent),
                _ => {}
            }
        }
        assert_eq!(got, Some(DocumentInput::Click { x: 3, y: 4 }));
        assert_eq!(
            origin.as_deref(),
            Some("phux-server-integration-test"),
            "the hub's client, not the hub link nor the sender's own claim"
        );

        drop(hub);
        fed.shutdown().await;
    });
}

/// A PNG's signature and `IHDR` for a 4x4 image, tagged so two differ.
fn png(tag: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend_from_slice(&4_u32.to_be_bytes());
    bytes.extend_from_slice(&4_u32.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, tag]);
    bytes
}

/// One satellite resource of each non-Terminal document kind, published
/// once: its local id, its stream profile, and the next document to publish.
struct SatDocument {
    id: u32,
    profile: BootstrapStreamProfile,
    first: Vec<u8>,
    next: Vec<u8>,
}

async fn publish(sat: &mut UnixStream, request_id: u32, id: u32, bytes: &[u8]) {
    let published = phux_server_testkit::command(
        sat,
        request_id,
        Command::PublishResourceState {
            terminal_id: ResourceId::local(id),
            bytes: bytes.to_vec(),
        },
    )
    .await;
    assert!(
        matches!(published, CommandResult::OkWith(_)),
        "publish on the satellite: {published:?}"
    );
}

async fn spawn_sat_documents(fed: &Fed, sat: &mut UnixStream) -> Vec<SatDocument> {
    let parent = || ResourceId::local(fed.seed);
    let rows = [
        (
            SpawnResource::accessibility_tree(parent(), "Smoke"),
            BootstrapStreamProfile::AccessibilityTreeJsonV1,
            br#"{"role":"window","title":"one"}"#.to_vec(),
            br#"{"role":"window","title":"two"}"#.to_vec(),
        ),
        (
            SpawnResource::pixel_surface(parent(), "Smoke"),
            BootstrapStreamProfile::PixelSurfacePngV1,
            png(1),
            png(2),
        ),
        (
            SpawnResource::document(parent(), "text/markdown", Some("notes".to_owned())),
            BootstrapStreamProfile::DocumentTypedV1,
            b"# one".to_vec(),
            b"# two".to_vec(),
        ),
    ];
    let mut documents = Vec::new();
    for ((resource, profile, first, next), request_id) in rows.into_iter().zip(10..) {
        let spawn = Spawn {
            resource: Some(Box::new(resource)),
            ..Spawn::default()
        };
        let SpawnResult::Ok(ResourceId::Local { id }) =
            spawn_resource(sat, request_id, spawn).await
        else {
            panic!("spawn {profile:?} on the satellite failed");
        };
        publish(sat, request_id + 100, id, &first).await;
        documents.push(SatDocument {
            id,
            profile,
            first,
            next,
        });
    }
    documents
}

/// A hub client whose HELLO offered `profiles` (`None`: what real clients
/// offer), once the link answers.
async fn kind_client(fed: &Fed, profiles: Option<BootstrapProfileSet>) -> UnixStream {
    let Some(profiles) = profiles else {
        return fed.linked_hub().await;
    };
    let mut hub =
        phux_server_testkit::wait_for_socket_with(&fed.hub_path(), STEP_DEADLINE, |caps| {
            caps.with_bootstrap(BootstrapCapabilities::new().with_profiles(profiles))
        })
        .await;
    assert!(matches!(
        get_screen_until_ok(&mut hub, fed.seed).await,
        CommandResult::OkWith(_)
    ));
    hub
}

/// Attach `doc` through the hub and return its bootstrap profile and
/// document, then publish its next document and return the live bytes.
async fn attach_and_follow(
    hub: &mut UnixStream,
    sat: &mut UnixStream,
    request_id: u32,
    doc: &SatDocument,
) -> (Option<BootstrapStreamProfile>, Vec<u8>, Vec<u8>) {
    let id = ResourceId::satellite("sat", doc.id);
    let (result, frames) =
        command_via_hub(hub, request_id, Command::attach_terminal(id.clone(), None)).await;
    assert_eq!(
        result,
        CommandResult::Ok,
        "attach {:?} through the hub",
        doc.profile
    );
    let mut profile = None;
    let mut payload = Vec::new();
    let mut interleaved = frames.into_iter();
    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    loop {
        let frame = match interleaved.next() {
            Some(frame) => frame,
            None => recv_until_deadline(hub, deadline, |_, frame| Some(frame))
                .await
                .expect("bootstrap READY through the hub"),
        };
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                profile: got,
                ..
            } if terminal_id == id => profile = Some(got),
            FrameKind::BootstrapChunk {
                terminal_id,
                payload: chunk,
                ..
            } if terminal_id == id => payload.extend_from_slice(&chunk),
            FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == id => break,
            _ => {}
        }
    }
    let document = if doc.profile == BootstrapStreamProfile::DocumentTypedV1 {
        phux_protocol::wire::frame::typed_document::split_bootstrap(&payload)
            .expect("a typed bootstrap")
            .1
            .to_vec()
    } else {
        payload
    };
    publish(sat, request_id + 1, doc.id, &doc.next).await;
    let live = recv_until_deadline(hub, deadline, |_, frame| match frame {
        FrameKind::ResourceOutput {
            terminal_id, bytes, ..
        } if terminal_id == id => Some(bytes.to_vec()),
        _ => None,
    })
    .await
    .expect("live document through the hub");
    (profile, document, live)
}

/// A hub client with the link's own Terminal profile, no Terminal profile,
/// or another one than the link negotiated, attaches a satellite tree,
/// surface and document, each streaming its own profile and its newest
/// document across both hops: only a Terminal stream needs the
/// connection's Terminal profile.
#[test]
fn satellite_documents_relay_whatever_terminal_profile_the_client_chose() {
    use phux_protocol::caps::BootstrapProfileKind;
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut sat = fed.satellite().await;
        let mut documents = spawn_sat_documents(&fed, &mut sat).await;
        let clients = [
            None,
            Some(BootstrapProfileSet::new()),
            // The link selects NativeState or SynthesizedVtRaw first.
            Some(BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtStateSync,
            ])),
        ];
        for (profiles, base) in clients.into_iter().zip([3000, 1000, 2000]) {
            let mut hub = kind_client(&fed, profiles).await;
            for (doc, request_id) in documents.iter_mut().zip((base..).step_by(10)) {
                let (profile, document, live) =
                    attach_and_follow(&mut hub, &mut sat, request_id, doc).await;
                assert_eq!(profile, Some(doc.profile), "{profiles:?}");
                assert_eq!(
                    document, doc.first,
                    "{:?} bootstrap for {profiles:?}",
                    doc.profile
                );
                assert_eq!(live, doc.next, "{:?} live for {profiles:?}", doc.profile);
                std::mem::swap(&mut doc.first, &mut doc.next);
            }
            // The satellite Terminal itself still needs the link's profile.
            if profiles.is_some() {
                let attach = Command::attach_terminal(fed.sat_id(), None);
                let (refused, _) = command_via_hub(&mut hub, base + 500, attach).await;
                assert_command_error(&refused, ErrorCode::CodecUnavailable);
            }
            drop(hub);
        }

        drop(sat);
        fed.shutdown().await;
    });
}

/// The federated inventory lists a satellite session's documents among its
/// members, not just its Terminals.
#[test]
fn a_satellite_sessions_members_include_its_documents() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut sat = fed.satellite().await;
        let documents = spawn_sat_documents(&fed, &mut sat).await;
        let mut hub = fed.linked_hub().await;
        let (snapshot, _) = state_via_hub(&mut hub, 4000).await;
        let [sat_row] = snapshot.hosts() else {
            panic!("{:?}", snapshot.hosts());
        };
        let [session] = sat_row.sessions.as_slice() else {
            panic!("{sat_row:?}");
        };
        let mut expected = vec![fed.sat_id()];
        expected.extend(
            documents
                .iter()
                .map(|doc| ResourceId::satellite("sat", doc.id)),
        );
        for member in &expected {
            assert!(
                session.members.contains(member),
                "{member} missing: {session:?}"
            );
            assert_eq!(
                snapshot.host_session_of(member).map(|row| row.id),
                Some(session.id)
            );
        }
        assert_eq!(session.pane_count, 1, "pane_count counts Terminals");

        drop((sat, hub));
        fed.shutdown().await;
    });
}
