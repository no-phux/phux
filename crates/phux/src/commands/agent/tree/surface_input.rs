//! Input for a mirrored window or display: viewers send intents (`phux
//! agent page click|type|key`, or a scroll over the wire) to a surface, and its mirror
//! turns each into input events at global screen points.
//!
//! The mapping is pure: a click in frame pixels lands at the target's
//! origin plus the pixel scaled by points per frame pixel, which covers a
//! Retina backing scale and `--max-width` at once. Posting is macOS only
//! ([`Injector`]), through the `core-graphics` crate's safe event API.

#![cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS mirror posts events yet")
)]

use phux_client::tree::{DocumentInput, DocumentKey};

/// A rectangle in global screen points: CoreGraphics' space, whose origin
/// is the main display's top-left corner, y down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

/// Where pixel `(px, py)` of a `frame`-sized image of `rect` lands, clamped
/// inside `rect`; `None` before any frame.
pub(super) fn to_screen(frame: (u32, u32), rect: Rect, px: u32, py: u32) -> Option<(f64, f64)> {
    let axis = |p: u32, n: u32, origin: f64, len: f64| {
        origin + f64::from(p.min(n - 1)) * len / f64::from(n)
    };
    (frame.0 > 0 && frame.1 > 0).then(|| {
        (
            axis(px, frame.0, rect.x, rect.w),
            axis(py, frame.1, rect.y, rect.h),
        )
    })
}

/// One input event, in screen points.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Event {
    Move(f64, f64),
    LeftDown(f64, f64),
    LeftUp(f64, f64),
    /// Key down and up carrying this text (at most [`TEXT_CHUNK`] UTF-16
    /// units, the most one keyboard event carries).
    Text(String),
    /// Key down and up of a macOS virtual key code.
    Key(u16),
    /// Cmd+V: paste what a viewer's paste put on the clipboard.
    PasteChord,
    /// One scroll-wheel event in pixels; positive is up and left.
    Scroll {
        dy: i32,
        dx: i32,
    },
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Move(x, y) => write!(f, "mouse-moved at {x:.1},{y:.1}"),
            Self::LeftDown(x, y) => write!(f, "left-down at {x:.1},{y:.1}"),
            Self::LeftUp(x, y) => write!(f, "left-up at {x:.1},{y:.1}"),
            Self::Text(text) => write!(f, "key down+up with text {text:?}"),
            Self::Key(code) => write!(f, "key down+up of virtual key 0x{code:02x}"),
            Self::PasteChord => write!(f, "Cmd+V"),
            Self::Scroll { dy, dx } => write!(f, "scroll-wheel {dy} up, {dx} left (pixels)"),
        }
    }
}

/// A blank 160x100 PNG a dry run publishes when the capture is refused.
pub(super) const DRY_RUN_FRAME: &[u8] =
    b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\xa0\x00\x00\
      \x00\x64\x08\x00\x00\x00\x00\x24\xdd\x64\x53\x00\x00\x00\x59\x49\x44\x41\x54\x78\xda\xed\
      \xce\x01\x0d\x00\x00\x0c\x02\x20\xa3\x1b\xdd\x1a\xdf\x07\x09\x48\x8f\x8b\xa0\xa0\xa0\xa0\
      \xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\
      \xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\
      \xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xa0\xe0\x97\xe0\x00\x56\x40\x41\xd2\xde\x80\
      \x21\xa8\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";

pub(super) const TEXT_CHUNK: usize = 20;

/// The events that carry out `input` on a `frame`-sized image of `rect`.
/// A navigation has none: a window or display has no address.
pub(super) fn events(input: &DocumentInput, frame: (u32, u32), rect: Rect) -> Vec<Event> {
    match input {
        // The clipboard itself is the mirror's (`clipboard`): only the
        // paste chord is an event.
        DocumentInput::Navigate { .. }
        | DocumentInput::CopyClipboard { .. }
        | DocumentInput::ClipboardText { .. }
        | DocumentInput::Paste { chord: false, .. } => Vec::new(),
        DocumentInput::Paste { chord: true, .. } => vec![Event::PasteChord],
        DocumentInput::Navigate { .. } | DocumentInput::Activate { .. } => Vec::new(),
        DocumentInput::Click { x, y } => to_screen(frame, rect, *x, *y)
            .map(|(x, y)| {
                vec![
                    Event::Move(x, y),
                    Event::LeftDown(x, y),
                    Event::LeftUp(x, y),
                ]
            })
            .unwrap_or_default(),
        DocumentInput::Type { text } => text_chunks(text).into_iter().map(Event::Text).collect(),
        DocumentInput::Key { key } => vec![Event::Key(key_code(*key))],
        DocumentInput::Scroll { dx, dy } => {
            // The wheel turns under the pointer, so it moves to the middle first.
            let scale = if frame.0 == 0 {
                1.0
            } else {
                rect.w / f64::from(frame.0)
            };
            #[expect(clippy::cast_possible_truncation, reason = "a scroll step is small")]
            let points = |d: i32| (-f64::from(d) * scale).round() as i32;
            vec![
                Event::Move(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0),
                Event::Scroll {
                    dy: points(*dy),
                    dx: points(*dx),
                },
            ]
        }
    }
}

/// `text` in pieces of at most [`TEXT_CHUNK`] UTF-16 units, never splitting
/// a character.
pub(super) fn text_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for ch in text.chars() {
        if chunk.encode_utf16().count() + ch.len_utf16() > TEXT_CHUNK {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// The macOS virtual key code (`Carbon` `kVK_*`) of `key`.
pub(super) const fn key_code(key: DocumentKey) -> u16 {
    match key {
        DocumentKey::Enter => 0x24,
        DocumentKey::Tab => 0x30,
        DocumentKey::Backspace => 0x33,
        DocumentKey::Escape => 0x35,
        DocumentKey::ArrowLeft => 0x7B,
        DocumentKey::ArrowRight => 0x7C,
        DocumentKey::ArrowDown => 0x7D,
        DocumentKey::ArrowUp => 0x7E,
    }
}

/// A window's bounds and owning process, from [`WINDOW_INFO`]'s output;
/// `None` when the window is gone.
pub(super) fn window_info(json: &str) -> Option<(Rect, i64)> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let n = |key: &str| v[key].as_f64();
    Some((
        Rect {
            x: n("x")?,
            y: n("y")?,
            w: n("w")?,
            h: n("h")?,
        },
        v["pid"].as_i64()?,
    ))
}

/// One window's bounds and owner pid as JSON (empty when it is gone),
/// through the JavaScript automation bridge: no bindings, no `unsafe`. With
/// `raise`, its application is activated first, since posted input goes to
/// whatever is on top.
pub(super) const WINDOW_INFO: &str = r#"function run(argv) {
ObjC.import("CoreGraphics"); ObjC.import("AppKit");
const id = Number(argv[0]);
const list = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionIncludingWindow, id))) || [];
const w = list.find(w => w.kCGWindowNumber === id);
if (!w) return "";
if (argv[1] === "raise") {
  const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(w.kCGWindowOwnerPID);
  if (app) app.activateWithOptions($.NSApplicationActivateIgnoringOtherApps);
}
const b = w.kCGWindowBounds;
return JSON.stringify({ x: b.X, y: b.Y, w: b.Width, h: b.Height, pid: w.kCGWindowOwnerPID });
}"#;

#[cfg(target_os = "macos")]
pub(super) use macos::{Injector, post};

#[cfg(target_os = "macos")]
mod macos {
    use std::process::Command;
    use std::time::Duration;

    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;
    use phux_client::tree::DocumentInput;

    use super::{Event, Rect, WINDOW_INFO, events, window_info};

    const NOT_TRUSTED: &str = "phux: input from viewers is ignored: posting it needs the Accessibility grant. Grant the app that started this mirror (your terminal) Accessibility in System Settings, Privacy & Security, Accessibility";

    /// Carries out the intents a surface's viewers send to its window (by
    /// CoreGraphics id) or to the main display.
    pub(in crate::commands::agent::tree) struct Injector {
        pub(in crate::commands::agent::tree) window: Option<u32>,
        /// Log the events instead of posting them.
        pub(in crate::commands::agent::tree) dry_run: bool,
        warned: bool,
    }

    impl Injector {
        pub(in crate::commands::agent::tree) const fn new(
            window: Option<u32>,
            dry_run: bool,
        ) -> Self {
            Self {
                window,
                dry_run,
                warned: false,
            }
        }

        /// Carry out `input` on the newest frame, `frame` pixels in size.
        /// A failure is reported and skipped; the capture goes on.
        pub(in crate::commands::agent::tree) fn act(
            &mut self,
            input: &DocumentInput,
            frame: (u32, u32),
        ) {
            if let DocumentInput::Navigate { url } = input {
                tracing::debug!(%url, "a window or display has no address; navigate ignored");
                if self.dry_run {
                    eprintln!(
                        "dry-run: navigate {url:?} ignored: a window or display has no address"
                    );
                }
                return;
            }
            if !self.dry_run && !trusted() {
                if !std::mem::replace(&mut self.warned, true) {
                    eprintln!("{NOT_TRUSTED}");
                }
                return;
            }
            let (rect, pid) = match self.window {
                None => (main_display(), None),
                Some(id) => match window(id, !self.dry_run) {
                    Ok((rect, pid)) => (rect, Some(pid)),
                    Err(message) => {
                        eprintln!("phux: input skipped: {message}");
                        return;
                    }
                },
            };
            let events = events(input, frame, rect);
            if self.dry_run {
                eprintln!(
                    "dry-run: {input:?} on a {}x{} frame of {} at {:.1},{:.1} {:.1}x{:.1} pt",
                    frame.0,
                    frame.1,
                    self.window.map_or_else(
                        || "the main display".to_owned(),
                        |id| format!("window {id}")
                    ),
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h
                );
                if let Some(pid) = pid {
                    eprintln!("dry-run: would activate pid {pid}");
                }
                for event in &events {
                    eprintln!("dry-run: {event}");
                }
                return;
            }
            if pid.is_some() {
                // Activation is asynchronous; give the window time to come up.
                std::thread::sleep(Duration::from_millis(150));
            }
            if let Err(message) = post(&events) {
                eprintln!("phux: input failed: {message}");
            }
        }
    }

    /// Whether this process may post input. An untrusted process's posted
    /// events vanish without an error, and every accessibility call answers
    /// `kAXErrorAPIDisabled`, so one read tells.
    fn trusted() -> bool {
        use accessibility::{AXUIElement, AXUIElementAttributes as _, Error};
        !matches!(
            AXUIElement::system_wide().role(),
            Err(Error::Ax(accessibility_sys::kAXErrorAPIDisabled))
        )
    }

    fn main_display() -> Rect {
        let bounds = core_graphics::display::CGDisplay::main().bounds();
        Rect {
            x: bounds.origin.x,
            y: bounds.origin.y,
            w: bounds.size.width,
            h: bounds.size.height,
        }
    }

    /// Window `id`'s bounds now (windows move) and its owner, activating the
    /// owner first when `raise`.
    // ponytail: activating the app raises its frontmost window, which is not
    // always this one; raising the window itself needs its AX element.
    fn window(id: u32, raise: bool) -> Result<(Rect, i64), String> {
        let mut command = Command::new("osascript");
        command.args(["-l", "JavaScript", "-e", WINDOW_INFO, &id.to_string()]);
        if raise {
            command.arg("raise");
        }
        let output = command
            .output()
            .map_err(|err| format!("could not find window {id}: {err}"))?;
        window_info(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| format!("window {id} is gone"))
    }

    /// Post `events` to the HID event stream, as if typed and clicked.
    pub(in crate::commands::agent::tree) fn post(events: &[Event]) -> Result<(), String> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| "CoreGraphics gave no event source".to_owned())?;
        let made = |event: Result<CGEvent, ()>| {
            event.map_err(|()| "CoreGraphics could not make an event".to_owned())
        };
        let mouse = |kind, x, y| {
            made(CGEvent::new_mouse_event(
                source.clone(),
                kind,
                CGPoint::new(x, y),
                CGMouseButton::Left,
            ))
        };
        let key = |code, text: Option<&str>| -> Result<Vec<CGEvent>, String> {
            [true, false]
                .into_iter()
                .map(|down| {
                    let event = made(CGEvent::new_keyboard_event(source.clone(), code, down))?;
                    if let Some(text) = text {
                        event.set_string(text);
                    }
                    Ok(event)
                })
                .collect()
        };
        for event in events {
            let posted = match event {
                Event::Move(x, y) => vec![mouse(CGEventType::MouseMoved, *x, *y)?],
                Event::LeftDown(x, y) => vec![mouse(CGEventType::LeftMouseDown, *x, *y)?],
                Event::LeftUp(x, y) => vec![mouse(CGEventType::LeftMouseUp, *x, *y)?],
                Event::Text(text) => key(0, Some(text))?,
                Event::Key(code) => key(*code, None)?,
                Event::PasteChord => {
                    // kVK_ANSI_V with Command held.
                    let chord = key(0x09, None)?;
                    for event in &chord {
                        event.set_flags(CGEventFlags::CGEventFlagCommand);
                    }
                    chord
                }
                Event::Scroll { dy, dx } => vec![made(CGEvent::new_scroll_event(
                    source.clone(),
                    ScrollEventUnit::PIXEL,
                    2,
                    *dy,
                    *dx,
                    0,
                ))?],
            };
            for event in posted {
                event.post(CGEventTapLocation::HID);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1512.0,
        h: 982.0,
    };

    #[test]
    fn a_display_at_2x_halves_the_frame_pixels() {
        assert_eq!(
            to_screen((3024, 1964), DISPLAY, 100, 50),
            Some((50.0, 25.0))
        );
        assert_eq!(to_screen((0, 0), DISPLAY, 1, 1), None);
    }

    #[test]
    fn a_window_adds_its_origin_and_scales_its_pixels() {
        let window = Rect {
            x: 300.0,
            y: 102.0,
            w: 1320.0,
            h: 840.0,
        };
        // A Retina capture: 2640x1680 pixels of a 1320x840 point window.
        assert_eq!(
            to_screen((2640, 1680), window, 200, 100),
            Some((400.0, 152.0))
        );
        // `--max-width 660` on it: each frame pixel is two points.
        assert_eq!(to_screen((660, 420), window, 100, 50), Some((500.0, 202.0)));
    }

    #[test]
    fn a_point_outside_the_frame_clamps_to_its_last_pixel() {
        let (x, y) = to_screen((1512, 982), DISPLAY, 5000, 5000).unwrap();
        assert_eq!((x, y), (1511.0, 981.0));
    }

    #[test]
    fn a_click_is_move_down_up_and_a_scroll_turns_under_the_pointer() {
        let click = events(
            &DocumentInput::Click { x: 100, y: 50 },
            (3024, 1964),
            DISPLAY,
        );
        assert_eq!(
            click,
            [
                Event::Move(50.0, 25.0),
                Event::LeftDown(50.0, 25.0),
                Event::LeftUp(50.0, 25.0)
            ]
        );
        let scroll = events(
            &DocumentInput::Scroll { dx: 0, dy: 40 },
            (3024, 1964),
            DISPLAY,
        );
        assert_eq!(
            scroll,
            [Event::Move(756.0, 491.0), Event::Scroll { dy: -20, dx: 0 }]
        );
        let nav = DocumentInput::Navigate {
            url: "https://phux.sh/".into(),
        };
        assert!(events(&nav, (1, 1), DISPLAY).is_empty());
        let paste = |chord| DocumentInput::Paste {
            text: "x".into(),
            chord,
        };
        assert_eq!(events(&paste(true), (1, 1), DISPLAY), [Event::PasteChord]);
        assert!(events(&paste(false), (1, 1), DISPLAY).is_empty());
        let copy = DocumentInput::CopyClipboard { reply: 1 };
        assert!(events(&copy, (1, 1), DISPLAY).is_empty());
    }

    #[test]
    fn text_goes_in_chunks_a_keyboard_event_can_carry() {
        let long = "a".repeat(45);
        let chunks = text_chunks(&long);
        assert_eq!(
            chunks.iter().map(String::len).collect::<Vec<_>>(),
            [20, 20, 5]
        );
        // An emoji is two UTF-16 units and is never split.
        let emoji = format!("{}😀", "b".repeat(19));
        assert_eq!(text_chunks(&emoji), ["b".repeat(19), "😀".to_owned()]);
        assert!(text_chunks("").is_empty());
    }

    #[test]
    fn every_key_has_its_virtual_key_code() {
        let mut unique: Vec<u16> = DocumentKey::ALL.iter().map(|key| key_code(*key)).collect();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), DocumentKey::ALL.len());
        #[cfg(target_os = "macos")]
        {
            use core_graphics::event::KeyCode;
            assert_eq!(key_code(DocumentKey::Enter), KeyCode::RETURN);
            assert_eq!(key_code(DocumentKey::Tab), KeyCode::TAB);
            assert_eq!(key_code(DocumentKey::Backspace), KeyCode::DELETE);
            assert_eq!(key_code(DocumentKey::Escape), KeyCode::ESCAPE);
            assert_eq!(key_code(DocumentKey::ArrowLeft), KeyCode::LEFT_ARROW);
            assert_eq!(key_code(DocumentKey::ArrowRight), KeyCode::RIGHT_ARROW);
            assert_eq!(key_code(DocumentKey::ArrowDown), KeyCode::DOWN_ARROW);
            assert_eq!(key_code(DocumentKey::ArrowUp), KeyCode::UP_ARROW);
        }
    }

    #[test]
    fn the_dry_run_frame_is_a_160x100_png() {
        assert_eq!(phux_client::tree::png_size(DRY_RUN_FRAME), Some((160, 100)));
    }

    #[test]
    fn window_info_reads_bounds_and_owner_or_nothing() {
        let (rect, pid) = window_info(r#"{"x":300,"y":102,"w":1320,"h":840,"pid":93904}"#).unwrap();
        assert_eq!((rect.x, rect.w, pid), (300.0, 1320.0, 93904));
        assert_eq!(window_info(""), None);
    }
}
