//! `SEND_DOCUMENT_INPUT` over the production dispatch loop
//! (`docs/resource-kinds.md#producers`): a viewer's intent reaches the
//! connection producing the document as an unjournaled event, is refused
//! when a hub's stamped classes leave out the document's control class, and
//! is refused once that producer is gone.

use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ControlClass, ControlClasses, DocumentInput, DocumentKey,
    ErrorCode, FrameKind, SpawnResource, SpawnResult,
};
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::{Spawn, command, spawn_resource};

use crate::common::{Server, attach, sh, spawned, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";

#[test]
fn a_viewer_intent_reaches_the_producer_and_is_refused_once_it_is_gone() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut producer = server.connect().await;
        attach(&mut producer, "demo").await;
        let parent = spawned(&mut producer, 1, sh(FOREVER)).await;
        let spawn = Spawn {
            resource: Some(Box::new(
                SpawnResource::pixel_surface(parent.clone(), "page:test")
                    .with_control(Some(ControlClass::Window)),
            )),
            ..Spawn::default()
        };
        let SpawnResult::Ok(surface) = spawn_resource(&mut producer, 2, spawn).await else {
            panic!("spawn surface failed");
        };

        let mut viewer = server.connect().await;
        let send = |intent| Command::SendDocumentInput {
            terminal_id: surface.clone(),
            intent,
            control: None,
        };
        for (request_id, intent) in [
            (10, DocumentInput::Click { x: 40, y: 12 }),
            (
                11,
                DocumentInput::Key {
                    key: DocumentKey::Enter,
                },
            ),
        ] {
            let taken = command(&mut viewer, request_id, send(intent.clone())).await;
            assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
            let got = wait_frame(&mut producer, "document_input", |frame| match frame {
                FrameKind::Event {
                    terminal: Some(terminal),
                    event: AgentEvent::DocumentInput { intent },
                    stamp,
                } if terminal == surface => {
                    assert!(stamp.is_none(), "an intent is never journaled");
                    Some(intent)
                }
                _ => None,
            })
            .await;
            assert_eq!(got, intent);
        }

        // A Terminal is not a document: no producer relays its input here.
        let refused = command(
            &mut viewer,
            12,
            Command::SendDocumentInput {
                terminal_id: parent,
                intent: DocumentInput::Type { text: "x".into() },
                control: None,
            },
        )
        .await;
        assert!(
            matches!(refused, CommandResult::Error { .. }),
            "{refused:?}"
        );

        drop(producer);
        let mut refused = CommandResult::Ok;
        for request_id in 20..60 {
            refused = command(
                &mut viewer,
                request_id,
                send(DocumentInput::Click { x: 1, y: 1 }),
            )
            .await;
            if !matches!(refused, CommandResult::Ok) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            matches!(
                refused,
                CommandResult::Error {
                    code: ErrorCode::NotProducer,
                    ..
                }
            ),
            "{refused:?}"
        );

        drop(viewer);
        server.stop().await;
    });
}

#[test]
fn a_stamped_class_set_without_the_documents_class_is_refused() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut producer = server.connect().await;
        attach(&mut producer, "demo").await;
        let parent = spawned(&mut producer, 1, sh(FOREVER)).await;
        let spawn = Spawn {
            resource: Some(Box::new(
                SpawnResource::pixel_surface(parent.clone(), "page:test")
                    .with_control(Some(ControlClass::Window)),
            )),
            ..Spawn::default()
        };
        let SpawnResult::Ok(surface) = spawn_resource(&mut producer, 2, spawn).await else {
            panic!("spawn surface failed");
        };

        let mut viewer = server.connect().await;
        // A hub stamps the classes it authorized: one without the
        // document's class is refused before the producer hears of it.
        let stamped = |classes: &[ControlClass]| Command::SendDocumentInput {
            terminal_id: surface.clone(),
            intent: DocumentInput::Click { x: 1, y: 1 },
            control: Some(
                classes
                    .iter()
                    .fold(ControlClasses::EMPTY, |set, class| set.with(*class)),
            ),
        };
        let refused = command(&mut viewer, 9, stamped(&[ControlClass::Display])).await;
        assert!(
            matches!(
                refused,
                CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            ),
            "{refused:?}"
        );
        let taken = command(
            &mut viewer,
            8,
            stamped(&[ControlClass::Page, ControlClass::Window]),
        )
        .await;
        assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
        wait_frame(
            &mut producer,
            "stamped document_input",
            |frame| match frame {
                FrameKind::Event {
                    event: AgentEvent::DocumentInput { .. },
                    ..
                } => Some(()),
                _ => None,
            },
        )
        .await;
        drop(viewer);
        drop(producer);
        server.stop().await;
    });
}
