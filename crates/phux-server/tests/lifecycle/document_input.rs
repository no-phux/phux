//! `SEND_DOCUMENT_INPUT` over the production dispatch loop
//! (`docs/resource-kinds.md#producers`): a viewer's intent reaches the
//! connection producing the document as an unjournaled event, is refused
//! when a hub's stamped classes leave out the document's control class, and
//! is refused once that producer is gone. A clipboard copy is held open
//! until the producer answers, and only that producer may.

use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlClass, ControlClasses, DocumentInput,
    DocumentKey, ErrorCode, FrameKind, SpawnResource, SpawnResult,
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

#[test]
fn a_copy_waits_for_its_producer_and_reaches_only_the_viewer_that_asked() {
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
                SpawnResource::pixel_surface(parent, "page:test")
                    .with_control(Some(ControlClass::Display)),
            )),
            ..Spawn::default()
        };
        let SpawnResult::Ok(surface) = spawn_resource(&mut producer, 2, spawn).await else {
            panic!("spawn surface failed");
        };
        let send = |intent| Command::SendDocumentInput {
            terminal_id: surface.clone(),
            intent,
            control: None,
        };

        // The viewer's token is not trusted: the server stamps its own.
        let mut viewer = server.connect().await;
        let copy = send(DocumentInput::CopyClipboard { reply: 77 });
        let asked = tokio::task::spawn_local(async move {
            let copied = command(&mut viewer, 10, copy).await;
            (viewer, copied)
        });
        let reply = copy_relayed(&mut producer).await;
        assert_ne!(reply, 77);

        let answer =
            |text: Result<String, String>| send(DocumentInput::ClipboardText { reply, text });
        let mut stranger = server.connect().await;
        let refused = command(&mut stranger, 11, answer(Ok("forged".into()))).await;
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
        let taken = command(&mut producer, 12, answer(Ok("on the host".into()))).await;
        assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
        let (mut viewer, copied) = asked.await.unwrap();
        assert_eq!(
            copied,
            CommandResult::OkWith(CommandValue::Bytes(b"on the host".to_vec()))
        );
        // Answered once: a second answer finds nothing waiting.
        let late = command(&mut producer, 13, answer(Ok("again".into()))).await;
        assert!(matches!(late, CommandResult::Error { .. }), "{late:?}");

        // A producer that cannot read its clipboard says why.
        let copy = send(DocumentInput::CopyClipboard { reply: 0 });
        let asked = tokio::task::spawn_local(async move { command(&mut viewer, 14, copy).await });
        let reply = copy_relayed(&mut producer).await;
        let no_tool = Err("wl-paste is not installed here".to_owned());
        let taken = command(
            &mut producer,
            15,
            send(DocumentInput::ClipboardText {
                reply,
                text: no_tool,
            }),
        )
        .await;
        assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
        let failed = asked.await.unwrap();
        assert!(
            matches!(
                &failed,
                CommandResult::Error { code: ErrorCode::Unavailable, message }
                    if message.contains("wl-paste")
            ),
            "{failed:?}"
        );
        drop((producer, stranger));
        server.stop().await;
    });
}

/// The server's token on the next copy relayed to `producer`.
async fn copy_relayed(producer: &mut tokio::net::UnixStream) -> u32 {
    wait_frame(producer, "copy", |frame| match frame {
        FrameKind::Event {
            event:
                AgentEvent::DocumentInput {
                    intent: DocumentInput::CopyClipboard { reply },
                },
            ..
        } => Some(reply),
        _ => None,
    })
    .await
}
