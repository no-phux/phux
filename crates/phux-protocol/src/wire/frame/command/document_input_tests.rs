//! Tests for [`super`]: wire round trips and bounds.

use super::*;

#[test]
fn every_intent_round_trips_and_bounds_hold() {
    let intents = [
        DocumentInput::Navigate {
            url: "https://phux.sh/".into(),
        },
        DocumentInput::Click { x: 10, y: 20 },
        DocumentInput::Type { text: "hi".into() },
        DocumentInput::Key {
            key: DocumentKey::Enter,
        },
        DocumentInput::Scroll { dx: -3, dy: 400 },
        DocumentInput::Paste {
            text: "x".repeat(MAX_DOCUMENT_INPUT_TEXT + 1),
            chord: true,
        },
        DocumentInput::CopyClipboard { reply: 9 },
        DocumentInput::ClipboardText {
            reply: 9,
            text: Ok("copied".into()),
        },
        DocumentInput::ClipboardText {
            reply: 9,
            text: Err("no wl-paste".into()),
        },
        DocumentInput::Activate {
            node: "0.2.1".into(),
        },
        DocumentInput::Drag {
            from: (10, 20),
            to: (300, 25),
        },
        DocumentInput::SelectAll,
        DocumentInput::CopySelection {
            reply: 4,
            cut: true,
        },
    ];
    let encode = |intent: &DocumentInput| {
        let mut buf = bytes::BytesMut::new();
        intent.encode(&mut Encoder::new(&mut buf));
        buf
    };
    for intent in intents {
        let bytes = encode(&intent);
        assert_eq!(DocumentInput::decode(&mut Decoder::new(&bytes)), Ok(intent));
    }
    let long = encode(&DocumentInput::Type {
        text: "x".repeat(MAX_DOCUMENT_INPUT_TEXT + 1),
    });
    assert!(DocumentInput::decode(&mut Decoder::new(&long)).is_err());
    let huge = encode(&DocumentInput::Paste {
        text: "x".repeat(MAX_CLIPBOARD_TEXT + 1),
        chord: false,
    });
    assert!(DocumentInput::decode(&mut Decoder::new(&huge)).is_err());
    assert!(DocumentInput::decode(&mut Decoder::new(&[12])).is_err());
    assert!(DocumentInput::decode(&mut Decoder::new(&[9, 0, 0])).is_err());
    assert_eq!(DocumentKey::parse("escape"), Some(DocumentKey::Escape));
    assert_eq!(DocumentKey::parse("F1"), None);
}

#[test]
fn the_command_and_the_relayed_event_round_trip() {
    use crate::ids::ResourceId;
    use crate::wire::frame::{AgentEvent, Command, FrameKind};
    let intent = DocumentInput::Key {
        key: DocumentKey::Tab,
    };
    let window = ControlClasses::EMPTY.with(ControlClass::Window);
    let send = |control| FrameKind::Command {
        request_id: 7,
        command: Command::SendDocumentInput {
            terminal_id: ResourceId::local(3),
            intent: intent.clone(),
            control,
            on_behalf_of: None,
        },
    };
    let relayed = |control| FrameKind::Command {
        request_id: 8,
        command: Command::SendDocumentInput {
            terminal_id: ResourceId::local(3),
            intent: intent.clone(),
            control,
            on_behalf_of: Some(OnBehalfOf {
                credential: "sha256:phone".to_owned(),
                name: "iPhone".to_owned(),
            }),
        },
    };
    for frame in [
        send(None),
        send(Some(window)),
        send(Some(ControlClasses::EMPTY)),
        relayed(None),
        relayed(Some(window)),
        FrameKind::Event {
            terminal: Some(ResourceId::local(3)),
            event: AgentEvent::DocumentInput { intent },
            stamp: None,
        },
    ] {
        let mut buf = bytes::BytesMut::new();
        frame.encode(&mut buf);
        assert_eq!(FrameKind::decode(&buf).unwrap().0, frame);
    }
}

#[test]
fn control_classes_are_one_bit_each_and_unknown_bits_only_narrow() {
    let all = ControlClass::ALL
        .into_iter()
        .fold(ControlClasses::EMPTY, ControlClasses::with);
    assert_eq!(all.bits(), 0b1110);
    assert_eq!(ControlClasses::from_bits(0xff), all);
    let window = ControlClasses::from_bits(0b0100);
    assert!(window.contains(ControlClass::Window));
    assert!(!window.contains(ControlClass::Display));
    assert!(all.intersect(window) == window && window.intersect(ControlClasses::EMPTY).is_empty());
    for class in ControlClass::ALL {
        assert_eq!(ControlClass::parse(class.name()), Some(class));
        assert_eq!(ControlClass::from_u8(class.to_u8()), Some(class));
    }
    assert_eq!(ControlClass::parse("Display"), None);
}
