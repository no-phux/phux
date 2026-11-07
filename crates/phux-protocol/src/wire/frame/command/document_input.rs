//! [`DocumentInput`]: a viewer's typed input intent for a document's
//! producer (`docs/resource-kinds.md#authority`), carried by
//! `SEND_DOCUMENT_INPUT` and relayed to the producer as the unjournaled
//! `document_input` event.

use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;

/// Maximum UTF-8 bytes in a [`DocumentInput::Navigate`] URL or a
/// [`DocumentInput::Type`] text.
pub const MAX_DOCUMENT_INPUT_TEXT: usize = 8 * 1024;

wire_enum! { to_u8 / from_u8;
/// A named key for [`DocumentInput::Key`]: a small closed set, not a keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentKey {
    /// Enter / Return.
    Enter = 0,
    /// Tab.
    Tab = 1,
    /// Backspace.
    Backspace = 2,
    /// Escape.
    Escape = 3,
    /// Arrow up.
    ArrowUp = 4,
    /// Arrow down.
    ArrowDown = 5,
    /// Arrow left.
    ArrowLeft = 6,
    /// Arrow right.
    ArrowRight = 7,
}
}

impl DocumentKey {
    /// Every key, in wire order.
    pub const ALL: [Self; 8] = [
        Self::Enter,
        Self::Tab,
        Self::Backspace,
        Self::Escape,
        Self::ArrowUp,
        Self::ArrowDown,
        Self::ArrowLeft,
        Self::ArrowRight,
    ];

    /// The key's name, as the DOM `KeyboardEvent.key` spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Enter => "Enter",
            Self::Tab => "Tab",
            Self::Backspace => "Backspace",
            Self::Escape => "Escape",
            Self::ArrowUp => "ArrowUp",
            Self::ArrowDown => "ArrowDown",
            Self::ArrowLeft => "ArrowLeft",
            Self::ArrowRight => "ArrowRight",
        }
    }

    /// The key named `name`, ignoring ASCII case.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|key| key.name().eq_ignore_ascii_case(name))
    }
}

/// One input intent for the producer of a document resource. The server
/// relays it without reading it; the producer maps it to its source (a
/// browser page's `DevTools` input, for example).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentInput {
    /// Load a URL.
    Navigate {
        /// The URL, at most [`MAX_DOCUMENT_INPUT_TEXT`] bytes.
        url: String,
    },
    /// Click at a point in the published surface image's own pixels.
    Click {
        /// Pixels from the image's left edge.
        x: u32,
        /// Pixels from the image's top edge.
        y: u32,
    },
    /// Insert text at the focus, at most [`MAX_DOCUMENT_INPUT_TEXT`] bytes.
    Type {
        /// The text.
        text: String,
    },
    /// Press and release one named key.
    Key {
        /// The key.
        key: DocumentKey,
    },
    /// Scroll by a delta, in surface pixels.
    Scroll {
        /// Horizontal delta; positive scrolls right.
        dx: i32,
        /// Vertical delta; positive scrolls down.
        dy: i32,
    },
}

const NAVIGATE: u8 = 0;
const CLICK: u8 = 1;
const TYPE: u8 = 2;
const KEY: u8 = 3;
const SCROLL: u8 = 4;

impl DocumentInput {
    pub(crate) fn encode(&self, enc: &mut Encoder<'_>) {
        match self {
            Self::Navigate { url } => {
                enc.write_u8(NAVIGATE);
                enc.write_str(url);
            }
            Self::Click { x, y } => {
                enc.write_u8(CLICK);
                enc.write_u32_be(*x);
                enc.write_u32_be(*y);
            }
            Self::Type { text } => {
                enc.write_u8(TYPE);
                enc.write_str(text);
            }
            Self::Key { key } => {
                enc.write_u8(KEY);
                enc.write_u8(key.to_u8());
            }
            Self::Scroll { dx, dy } => {
                enc.write_u8(SCROLL);
                enc.write_i64_be(i64::from(*dx));
                enc.write_i64_be(i64::from(*dy));
            }
        }
    }

    /// Decode one intent; an unknown tag or key, an over-long text or an
    /// out-of-range delta is an error.
    pub(crate) fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let text = |dec: &mut Decoder<'_>| -> Result<String, DecodeError> {
            let text = dec.read_str()?;
            if text.len() > MAX_DOCUMENT_INPUT_TEXT {
                return Err(DecodeError::AppendResourceOutputLimitExceeded);
            }
            Ok(text.to_owned())
        };
        let delta = |dec: &mut Decoder<'_>| -> Result<i32, DecodeError> {
            let value = dec.read_i64_be()?;
            i32::try_from(value).map_err(|_| DecodeError::LengthOverflow)
        };
        Ok(match dec.read_u8()? {
            NAVIGATE => Self::Navigate { url: text(dec)? },
            CLICK => Self::Click {
                x: dec.read_u32_be()?,
                y: dec.read_u32_be()?,
            },
            TYPE => Self::Type { text: text(dec)? },
            KEY => {
                let byte = dec.read_u8()?;
                let key = DocumentKey::from_u8(byte)
                    .ok_or_else(|| DecodeError::unknown_enum("DocumentKey", byte))?;
                Self::Key { key }
            }
            SCROLL => Self::Scroll {
                dx: delta(dec)?,
                dy: delta(dec)?,
            },
            other => return Err(DecodeError::unknown_enum("DocumentInput", other)),
        })
    }
}

#[cfg(test)]
mod tests {
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
        assert!(DocumentInput::decode(&mut Decoder::new(&[9])).is_err());
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
        for frame in [
            FrameKind::Command {
                request_id: 7,
                command: Command::SendDocumentInput {
                    terminal_id: ResourceId::local(3),
                    intent: intent.clone(),
                },
            },
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
}
