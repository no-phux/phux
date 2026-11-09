//! Input for a mirrored browser page: the viewer verbs (`phux agent page
//! navigate|click|type|key|scroll`) send a typed intent to the page's surface or
//! tree, and the page mirror, its producer, maps each intent it receives to
//! `DevTools` calls.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::tree::{self, DocumentInput, DocumentKey};

use super::{SCHEMA_VERSION, connect, report};
use crate::commands::{cli_runtime, json_err};
use crate::exit_codes::EXIT_USAGE;

/// The key `name` names, or a usage error listing the keys.
pub(super) fn parse_key(name: &str, json: bool) -> Result<DocumentKey, ExitCode> {
    DocumentKey::parse(name).ok_or_else(|| {
        let keys: Vec<&str> = DocumentKey::ALL.iter().map(|key| key.name()).collect();
        json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::USAGE_ERROR,
                format!("unknown key {name:?}"),
                format!("name one of {}", keys.join(", ")),
            ),
            EXIT_USAGE,
        )
    })
}

/// Send `input` to the producer of the page `target` names.
pub(super) fn run_send(
    target: &str,
    input: DocumentInput,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (mut conn, resource) = match connect(target, "agent page input", json, socket).await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        match tree::send_input(&mut conn, &resource, input).await {
            Ok(()) => {
                if json {
                    outln!(
                        "{}",
                        serde_json::json!({
                            "schema_version": SCHEMA_VERSION,
                            "sent": phux_client::selector::format_terminal_id(&resource),
                        })
                    );
                }
                ExitCode::SUCCESS
            }
            Err(err) => report(json, &err),
        }
    })
}

/// Print the host clipboard's text from the producer of the page `target`
/// names.
pub(super) fn run_copy(target: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (mut conn, resource) = match connect(target, "agent page copy", json, socket).await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        match tree::copy_clipboard(&mut conn, &resource).await {
            Ok(text) if json => {
                outln!(
                    "{}",
                    serde_json::json!({ "schema_version": SCHEMA_VERSION, "text": text })
                );
                ExitCode::SUCCESS
            }
            Ok(text) => {
                outln!("{text}");
                ExitCode::SUCCESS
            }
            Err(err) => report(json, &err),
        }
    })
}

/// How the newest screencast frame maps to the page: CSS pixels per image
/// pixel on each axis, and the viewport's CSS size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Viewport {
    pub(super) scale_x: f64,
    pub(super) scale_y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

impl Viewport {
    /// The mapping before any frame: one to one, no known size.
    pub(super) const IDENTITY: Self = Self {
        scale_x: 1.0,
        scale_y: 1.0,
        width: 0.0,
        height: 0.0,
    };

    /// From a frame's image size and its metadata's `deviceWidth` and
    /// `deviceHeight` (the viewport in CSS pixels).
    pub(super) fn of(image: (u32, u32), css: (f64, f64)) -> Self {
        let ratio = |css: f64, px: u32| {
            if px == 0 || css <= 0.0 {
                1.0
            } else {
                css / f64::from(px)
            }
        };
        Self {
            scale_x: ratio(css.0, image.0),
            scale_y: ratio(css.1, image.1),
            width: css.0,
            height: css.1,
        }
    }
}

/// The `DevTools` calls that carry out `input`, in order.
pub(super) fn cdp_calls(
    input: &DocumentInput,
    view: Viewport,
) -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    match input {
        DocumentInput::Navigate { url } => vec![("Page.navigate", json!({ "url": url }))],
        // A page takes a paste as inserted text; its clipboard is not ours.
        DocumentInput::Type { text } | DocumentInput::Paste { text, .. } => {
            vec![("Input.insertText", json!({ "text": text }))]
        }
        // The page mirror answers a copy from the host clipboard itself.
        DocumentInput::CopyClipboard { .. } | DocumentInput::ClipboardText { .. } => Vec::new(),
        DocumentInput::Click { x, y } => {
            let (x, y) = (f64::from(*x) * view.scale_x, f64::from(*y) * view.scale_y);
            ["mousePressed", "mouseReleased"]
                .into_iter()
                .map(|kind| {
                    (
                        "Input.dispatchMouseEvent",
                        json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }),
                    )
                })
                .collect()
        }
        DocumentInput::Scroll { dx, dy } => vec![(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseWheel",
                "x": view.width / 2.0,
                "y": view.height / 2.0,
                "deltaX": f64::from(*dx) * view.scale_x,
                "deltaY": f64::from(*dy) * view.scale_y,
            }),
        )],
        DocumentInput::Key { key } => {
            let code = match key {
                DocumentKey::Enter => 13,
                DocumentKey::Tab => 9,
                DocumentKey::Backspace => 8,
                DocumentKey::Escape => 27,
                DocumentKey::ArrowLeft => 37,
                DocumentKey::ArrowUp => 38,
                DocumentKey::ArrowRight => 39,
                DocumentKey::ArrowDown => 40,
            };
            let mut down = json!({
                "type": "keyDown",
                "key": key.name(),
                "code": key.name(),
                "windowsVirtualKeyCode": code,
            });
            // Enter submits a form only when it also types its character.
            if *key == DocumentKey::Enter {
                down["text"] = json!("\r");
            }
            let mut up = down.clone();
            up["type"] = json!("keyUp");
            up.as_object_mut().map(|up| up.remove("text"));
            vec![
                ("Input.dispatchKeyEvent", down),
                ("Input.dispatchKeyEvent", up),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_lands_in_css_pixels_scaled_from_the_image() {
        // A 2x device: the 1600x1200 image shows an 800x600 viewport.
        let view = Viewport::of((1600, 1200), (800.0, 600.0));
        let calls = cdp_calls(&DocumentInput::Click { x: 100, y: 50 }, view);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1["type"], "mousePressed");
        assert_eq!(calls[1].1["type"], "mouseReleased");
        assert_eq!(
            (calls[0].1["x"].as_f64(), calls[0].1["y"].as_f64()),
            (Some(50.0), Some(25.0))
        );
        assert_eq!(
            cdp_calls(&DocumentInput::Click { x: 3, y: 4 }, Viewport::IDENTITY)[0].1["x"],
            3.0
        );
        // `--max-width 800` on a 1600x900 viewport: half-size frames.
        let scaled = Viewport::of((800, 450), (1600.0, 900.0));
        let calls = cdp_calls(&DocumentInput::Click { x: 100, y: 50 }, scaled);
        assert_eq!(
            (calls[0].1["x"].as_f64(), calls[0].1["y"].as_f64()),
            (Some(200.0), Some(100.0))
        );
    }

    #[test]
    fn keys_text_and_navigation_map_to_their_devtools_calls() {
        let enter = cdp_calls(
            &DocumentInput::Key {
                key: DocumentKey::Enter,
            },
            Viewport::IDENTITY,
        );
        assert_eq!(enter[0].1["text"], "\r");
        assert_eq!(enter[1].1["type"], "keyUp");
        assert!(enter[1].1.get("text").is_none());
        let typed = cdp_calls(
            &DocumentInput::Type { text: "hi".into() },
            Viewport::IDENTITY,
        );
        assert_eq!(typed[0].0, "Input.insertText");
        let nav = cdp_calls(
            &DocumentInput::Navigate {
                url: "https://phux.sh/".into(),
            },
            Viewport::IDENTITY,
        );
        assert_eq!(
            nav,
            vec![(
                "Page.navigate",
                serde_json::json!({ "url": "https://phux.sh/" })
            )]
        );
        let wheel = cdp_calls(
            &DocumentInput::Scroll { dx: 0, dy: 10 },
            Viewport::of((400, 300), (800.0, 600.0)),
        );
        assert_eq!(wheel[0].1["deltaY"], 20.0);
        assert_eq!(wheel[0].1["x"], 400.0);
    }
}
