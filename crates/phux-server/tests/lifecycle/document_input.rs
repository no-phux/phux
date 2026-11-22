//! `SEND_DOCUMENT_INPUT` over the production dispatch loop
//! (`docs/resource-kinds.md#producers`): a viewer's intent reaches the
//! connection producing the document as an unjournaled event, is refused
//! when a hub's stamped classes leave out the document's control class, and
//! is refused once that producer is gone. A clipboard copy is held open
//! until the producer answers, and only that producer may; a paste is held
//! briefly for the producer's outcome.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlClass, ControlClasses, DocumentInput,
    DocumentKey, ErrorCode, FrameKind, SpawnResource,
};
use phux_server_testkit::{assert_command_error, command};
use tokio::net::UnixStream;

use crate::common::{demo_parent, spawn_kind, wait_frame};

/// A pixel surface under `parent` whose input is `class`.
async fn open_surface(
    producer: &mut UnixStream,
    parent: ResourceId,
    label: &str,
    class: ControlClass,
) -> ResourceId {
    let surface = SpawnResource::pixel_surface(parent, label).with_control(Some(class));
    spawn_kind(producer, 2, surface).await
}

/// An unstamped `SEND_DOCUMENT_INPUT` of `intent` to `surface`.
fn input(surface: &ResourceId, intent: DocumentInput) -> Command {
    Command::SendDocumentInput {
        terminal_id: surface.clone(),
        intent,
        control: None,
        on_behalf_of: None,
    }
}

#[test]
fn a_viewer_intent_reaches_the_producer_and_is_refused_once_it_is_gone() {
    phux_server_testkit::run_local(async {
        let (server, mut producer, parent) = demo_parent().await;
        let surface = open_surface(
            &mut producer,
            parent.clone(),
            "page:test",
            ControlClass::Window,
        )
        .await;

        let mut viewer = server.connect().await;
        let send = |intent| input(&surface, intent);
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
            input(&parent, DocumentInput::Type { text: "x".into() }),
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
        assert_command_error(&refused, ErrorCode::NotProducer);

        drop(viewer);
        server.stop().await;
    });
}

#[test]
fn a_stamped_class_set_without_the_documents_class_is_refused() {
    phux_server_testkit::run_local(async {
        let (server, mut producer, parent) = demo_parent().await;
        let surface = open_surface(&mut producer, parent, "page:test", ControlClass::Window).await;

        let mut viewer = server.connect().await;
        // A hub stamps the classes it authorized: one without the
        // document's class is refused before the producer hears of it.
        let stamped = |classes: &[ControlClass]| {
            let mut command = input(&surface, DocumentInput::Click { x: 1, y: 1 });
            if let Command::SendDocumentInput { control, .. } = &mut command {
                *control = Some(
                    classes
                        .iter()
                        .fold(ControlClasses::EMPTY, |set, class| set.with(*class)),
                );
            }
            command
        };
        let refused = command(&mut viewer, 9, stamped(&[ControlClass::Display])).await;
        assert_command_error(&refused, ErrorCode::PermissionDenied);
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
        let (server, mut producer, parent) = demo_parent().await;
        let surface = open_surface(&mut producer, parent, "page:test", ControlClass::Display).await;
        let send = |intent| input(&surface, intent);

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
        assert_command_error(&refused, ErrorCode::NotProducer);
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

        // A cut of the selection is held the same way; a producer that
        // cannot read its clipboard says why.
        let copy = send(DocumentInput::CopySelection {
            reply: 0,
            cut: true,
        });
        let asked = tokio::task::spawn_local(async move {
            let failed = command(&mut viewer, 14, copy).await;
            (viewer, failed)
        });
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
        let (viewer, failed) = asked.await.unwrap();
        assert!(
            matches!(
                &failed,
                CommandResult::Error { code: ErrorCode::Unavailable, message }
                    if message.contains("wl-paste")
            ),
            "{failed:?}"
        );

        empty_copies_nothing(viewer, &mut producer, send).await;
        drop((producer, stranger));
        server.stop().await;
    });
}

#[test]
fn a_paste_waits_briefly_for_its_producers_outcome() {
    phux_server_testkit::run_local(async {
        let (server, mut producer, parent) = demo_parent().await;
        let surface = open_surface(&mut producer, parent, "display", ControlClass::Display).await;
        let send = |intent| input(&surface, intent);
        let paste = || {
            send(DocumentInput::Paste {
                text: "hello".into(),
                chord: true,
            })
        };
        let pasted = |frame: FrameKind| match frame {
            FrameKind::Event {
                event:
                    AgentEvent::DocumentInput {
                        intent: DocumentInput::Paste { .. },
                    },
                ..
            } => Some(()),
            _ => None,
        };

        // The chord could not be pressed: the viewer hears why.
        let mut viewer = server.connect().await;
        let asked = tokio::task::spawn_local({
            let paste = paste();
            async move {
                let result = command(&mut viewer, 10, paste).await;
                (viewer, result)
            }
        });
        wait_frame(&mut producer, "paste", pasted).await;
        let outcome = |text| send(DocumentInput::ClipboardText { reply: 0, text });
        let no_keyboard = Err("nothing here can press the paste chord".to_owned());
        let taken = command(&mut producer, 11, outcome(no_keyboard)).await;
        assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
        let (mut viewer, failed) = asked.await.unwrap();
        assert!(
            matches!(
                &failed,
                CommandResult::Error { code: ErrorCode::Unavailable, message }
                    if message.contains("paste chord")
            ),
            "{failed:?}"
        );

        // A producer that never answers (it predates the answer) is a
        // success once the wait runs out.
        let started = std::time::Instant::now();
        let silent = command(&mut viewer, 12, paste()).await;
        assert!(matches!(silent, CommandResult::Ok), "{silent:?}");
        assert!(started.elapsed() >= std::time::Duration::from_secs(2));
        drop((producer, viewer));
        server.stop().await;
    });
}

/// The server's token on the next copy relayed to `producer`.
/// An older producer answers an empty clipboard with "": never a copy.
async fn empty_copies_nothing(
    mut viewer: UnixStream,
    producer: &mut UnixStream,
    send: impl Fn(DocumentInput) -> Command,
) {
    let copy = send(DocumentInput::CopySelection {
        reply: 0,
        cut: false,
    });
    let asked = tokio::task::spawn_local(async move { command(&mut viewer, 16, copy).await });
    let reply = copy_relayed(producer).await;
    let empty = send(DocumentInput::ClipboardText {
        reply,
        text: Ok(String::new()),
    });
    let taken = command(producer, 17, empty).await;
    assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
    let nothing = asked.await.unwrap();
    assert!(
        matches!(
            &nothing,
            CommandResult::Error { code: ErrorCode::Unavailable, message }
                if message == "Nothing copied on the host"
        ),
        "{nothing:?}"
    );
}

async fn copy_relayed(producer: &mut UnixStream) -> u32 {
    wait_frame(producer, "copy", |frame| match frame {
        FrameKind::Event {
            event:
                AgentEvent::DocumentInput {
                    intent:
                        DocumentInput::CopyClipboard { reply }
                        | DocumentInput::CopySelection { reply, .. },
                },
            ..
        } => Some(reply),
        _ => None,
    })
    .await
}
