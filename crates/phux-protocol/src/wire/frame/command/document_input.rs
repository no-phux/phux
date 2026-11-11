//! [`DocumentInput`]: a viewer's typed input intent for a document's
//! producer (`docs/resource-kinds.md#authority`), carried by
//! `SEND_DOCUMENT_INPUT` and relayed to the producer as the unjournaled
//! `document_input` event.

use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;

/// Maximum UTF-8 bytes in a [`DocumentInput::Navigate`] URL, a
/// [`DocumentInput::Type`] text or a [`DocumentInput::Activate`] node id.
pub const MAX_DOCUMENT_INPUT_TEXT: usize = 8 * 1024;

/// Maximum UTF-8 bytes of clipboard text a [`DocumentInput::Paste`] or a
/// [`DocumentInput::ClipboardText`] carries.
pub const MAX_CLIPBOARD_TEXT: usize = 1024 * 1024;

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

wire_enum! { to_u8 / from_u8;
/// What input to a document controls (`docs/resource-kinds.md#authority`):
/// its producer declares it when it opens the document. A document with
/// none takes input only from the owner socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ControlClass {
    /// One browser page, driven through its `DevTools` protocol.
    Page = 1,
    /// One application window.
    Window = 2,
    /// A whole screen or output: every application on it.
    Display = 3,
}
}

impl ControlClass {
    /// Every class, in wire order.
    pub const ALL: [Self; 3] = [Self::Page, Self::Window, Self::Display];

    /// The class's name: `page`, `window` or `display`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Window => "window",
            Self::Display => "display",
        }
    }

    /// The class named exactly `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.name() == name)
    }

    /// Whether watching such a document needs a grant naming its class
    /// (`docs/authority.md`): a window or display shows the machine, so no
    /// terminal grant reaches it; a page shows one site and is watched
    /// through its parent Terminal.
    #[must_use]
    pub const fn guards_observe(self) -> bool {
        !matches!(self, Self::Page)
    }

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// A set of [`ControlClass`]es: the classes a relaying hub authorized a
/// `SEND_DOCUMENT_INPUT` for, on the wire as one bit per class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ControlClasses(u8);

impl ControlClasses {
    /// No class.
    pub const EMPTY: Self = Self(0);

    /// This set with `class` added.
    #[must_use]
    pub const fn with(self, class: ControlClass) -> Self {
        Self(self.0 | class.bit())
    }

    /// Whether `class` is in the set.
    #[must_use]
    pub const fn contains(self, class: ControlClass) -> bool {
        self.0 & class.bit() != 0
    }

    /// Whether the set holds no class.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Classes in both sets.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// The wire byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The set a wire byte names; unknown bits are dropped, which only
    /// narrows it.
    #[must_use]
    pub fn from_bits(bits: u8) -> Self {
        ControlClass::ALL
            .into_iter()
            .filter(|class| bits & class.bit() != 0)
            .fold(Self::EMPTY, Self::with)
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
    /// Put `text` on the host clipboard, then, with `chord`, press the
    /// host's paste chord (Cmd+V, Ctrl+V) so the focus takes it. A page
    /// inserts the text instead.
    Paste {
        /// The text, at most [`MAX_CLIPBOARD_TEXT`] bytes.
        text: String,
        /// Press the paste chord after setting the clipboard.
        chord: bool,
    },
    /// Ask for the host clipboard's text. The server relaying it stamps
    /// `reply` (a viewer sends 0) and holds the command open until the
    /// producer answers with [`Self::ClipboardText`]; the viewer's reply is
    /// `OkWith(Bytes)`, the text as UTF-8.
    CopyClipboard {
        /// The server's correlation token for the producer's answer.
        reply: u32,
    },
    /// The producer's answer to [`Self::CopyClipboard`], sent to its own
    /// document. Only the producer the copy was relayed to may send it.
    ClipboardText {
        /// The `reply` the copy carried.
        reply: u32,
        /// The text (at most [`MAX_CLIPBOARD_TEXT`] bytes), or why the host
        /// clipboard could not be read.
        text: Result<String, String>,
    },
    /// Carry out the default action (press, click) of one accessibility
    /// node, by the `id` the tree document gives it. Sent to the tree; a
    /// producer that cannot act on the node semantically clicks its bounds.
    Activate {
        /// The node's id, at most [`MAX_DOCUMENT_INPUT_TEXT`] bytes.
        node: String,
    },
    /// Press the left button at `from`, move to `to`, release there, in the
    /// published surface image's own pixels: a text selection or a drag.
    /// The producer moves through the points between.
    Drag {
        /// Where the press lands, `(x, y)`.
        from: (u32, u32),
        /// Where the release lands, `(x, y)`.
        to: (u32, u32),
    },
}

const NAVIGATE: u8 = 0;
const CLICK: u8 = 1;
const TYPE: u8 = 2;
const KEY: u8 = 3;
const SCROLL: u8 = 4;
const PASTE: u8 = 5;
const COPY_CLIPBOARD: u8 = 6;
const CLIPBOARD_TEXT: u8 = 7;
const ACTIVATE: u8 = 8;
const DRAG: u8 = 9;

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
            Self::Paste { text, chord } => {
                enc.write_u8(PASTE);
                enc.write_str(text);
                enc.write_u8(u8::from(*chord));
            }
            Self::CopyClipboard { reply } => {
                enc.write_u8(COPY_CLIPBOARD);
                enc.write_u32_be(*reply);
            }
            Self::ClipboardText { reply, text } => {
                enc.write_u8(CLIPBOARD_TEXT);
                enc.write_u32_be(*reply);
                enc.write_u8(u8::from(text.is_ok()));
                enc.write_str(text.as_ref().unwrap_or_else(|error| error));
            }
            Self::Activate { node } => {
                enc.write_u8(ACTIVATE);
                enc.write_str(node);
            }
            Self::Drag { from, to } => {
                enc.write_u8(DRAG);
                for v in [from.0, from.1, to.0, to.1] {
                    enc.write_u32_be(v);
                }
            }
        }
    }

    /// Decode one intent; an unknown tag or key, an over-long text or an
    /// out-of-range delta is an error.
    pub(crate) fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let capped = |dec: &mut Decoder<'_>, cap: usize| -> Result<String, DecodeError> {
            let text = dec.read_str()?;
            if text.len() > cap {
                return Err(DecodeError::AppendResourceOutputLimitExceeded);
            }
            Ok(text.to_owned())
        };
        let text = |dec: &mut Decoder<'_>| capped(dec, MAX_DOCUMENT_INPUT_TEXT);
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
            PASTE => Self::Paste {
                text: capped(dec, MAX_CLIPBOARD_TEXT)?,
                chord: dec.read_bool("DocumentInput.Paste.chord")?,
            },
            COPY_CLIPBOARD => Self::CopyClipboard {
                reply: dec.read_u32_be()?,
            },
            CLIPBOARD_TEXT => {
                let reply = dec.read_u32_be()?;
                let ok = dec.read_bool("DocumentInput.ClipboardText.ok")?;
                let text = capped(dec, MAX_CLIPBOARD_TEXT)?;
                Self::ClipboardText {
                    reply,
                    text: if ok { Ok(text) } else { Err(text) },
                }
            }
            ACTIVATE => Self::Activate { node: text(dec)? },
            DRAG => Self::Drag {
                from: (dec.read_u32_be()?, dec.read_u32_be()?),
                to: (dec.read_u32_be()?, dec.read_u32_be()?),
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
        assert!(DocumentInput::decode(&mut Decoder::new(&[10])).is_err());
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
            },
        };
        for frame in [
            send(None),
            send(Some(window)),
            send(Some(ControlClasses::EMPTY)),
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
        assert!(
            all.intersect(window) == window && window.intersect(ControlClasses::EMPTY).is_empty()
        );
        for class in ControlClass::ALL {
            assert_eq!(ControlClass::parse(class.name()), Some(class));
            assert_eq!(ControlClass::from_u8(class.to_u8()), Some(class));
        }
        assert_eq!(ControlClass::parse("Display"), None);
    }
}
