//! `phux agent page mirror|navigate|click|type|key|scroll|paste|copy`: a
//! browser page as a tree and a surface, and input for it.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::tree::DocumentInput;

use super::surface::FrameOpts;
use super::{page_input, page_mirror};

/// `phux agent page mirror`: drive a browser page over the Chrome debugging protocol (CDP).
#[derive(Debug, usage::Subcommands)]
pub(crate) enum PageAction {
    /// Mirror a browser page into a tree and a surface until Ctrl-C.
    Mirror {
        /// A fragment of the page's title or URL, or `first`.
        page: String,
        /// The browser's debugging port (default 9222).
        #[usage(long, value_name = "PORT")]
        port: Option<u16>,
        /// The pane the documents bind to; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// Milliseconds between accessibility-tree reads (default 1000).
        #[usage(long, value_name = "MS")]
        interval: Option<u64>,
        /// Exit once this process is gone, as the app that launched the mirror passes its own.
        #[usage(long, value_name = "PID")]
        parent_pid: Option<u32>,
        #[usage(flatten)]
        frames: FrameOpts,
        #[usage(long)]
        json: bool,
    },
    /// Load a URL in a mirrored page.
    Navigate {
        /// The page's surface or tree.
        target: String,
        /// The URL to load.
        url: String,
        #[usage(long)]
        json: bool,
    },
    /// Click a mirrored page at a point of its surface image, in image pixels.
    Click {
        /// The page's surface or tree.
        target: String,
        /// Pixels from the image's left edge.
        #[usage(value_name = "X")]
        x: u32,
        /// Pixels from the image's top edge.
        #[usage(value_name = "Y")]
        y: u32,
        #[usage(long)]
        json: bool,
    },
    /// Drag across a mirrored page or screen with the left button, from one point of its surface image to another: selects text.
    Drag {
        /// The document's surface or tree.
        target: String,
        /// Where the press lands: pixels from the image's left edge.
        #[usage(value_name = "X1")]
        x1: u32,
        /// Where the press lands: pixels from the image's top edge.
        #[usage(value_name = "Y1")]
        y1: u32,
        /// Where the release lands: pixels from the image's left edge.
        #[usage(value_name = "X2")]
        x2: u32,
        /// Where the release lands: pixels from the image's top edge.
        #[usage(value_name = "Y2")]
        y2: u32,
        #[usage(long)]
        json: bool,
    },
    /// Type text into a mirrored page's focused field.
    Type {
        /// The page's surface or tree.
        target: String,
        /// The text to insert.
        text: String,
        #[usage(long)]
        json: bool,
    },
    /// Press one key in a mirrored page: Enter, Tab, Backspace, Escape, or an arrow.
    Key {
        /// The page's surface or tree.
        target: String,
        /// The key, e.g. `Enter` or `ArrowDown`.
        key: String,
        #[usage(long)]
        json: bool,
    },
    /// Scroll a mirrored page or screen by a delta in surface image pixels.
    Scroll {
        /// The document's surface or tree.
        target: String,
        /// Horizontal pixels; positive scrolls right.
        #[usage(value_name = "DX", allow_negative_numbers)]
        dx: i32,
        /// Vertical pixels; positive scrolls down.
        #[usage(value_name = "DY", allow_negative_numbers)]
        dy: i32,
        #[usage(long)]
        json: bool,
    },
    /// Paste text into a mirrored page or screen: a window or display puts it on the host clipboard and presses the paste chord.
    Paste {
        /// The document's surface or tree.
        target: String,
        /// The text to paste.
        text: String,
        #[usage(long)]
        json: bool,
    },
    /// Print the host clipboard's text, read by a mirrored page's or screen's producer.
    Copy {
        /// The document's surface or tree.
        target: String,
        #[usage(long)]
        json: bool,
    },
}

pub(crate) fn run_page(action: &PageAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        PageAction::Mirror {
            page,
            port,
            pane,
            interval,
            parent_pid,
            frames,
            json,
        } => {
            super::parent_watch::exit_with(*parent_pid);
            match frames.resolve(*json, super::surface::FrameFormat::Jpeg) {
                Ok(frames) => page_mirror::run_mirror(page_mirror::MirrorArgs {
                    page,
                    port: port.unwrap_or(9222),
                    pane: pane.as_deref(),
                    interval_ms: interval.unwrap_or(1000),
                    frames,
                    json: *json,
                    socket,
                }),
                Err(code) => code,
            }
        }
        PageAction::Navigate { target, url, json } => page_input::run_send(
            target,
            DocumentInput::Navigate { url: url.clone() },
            *json,
            socket,
        ),
        PageAction::Click { target, x, y, json } => {
            page_input::run_send(target, DocumentInput::Click { x: *x, y: *y }, *json, socket)
        }
        PageAction::Drag {
            target,
            x1,
            y1,
            x2,
            y2,
            json,
        } => page_input::run_send(
            target,
            DocumentInput::Drag {
                from: (*x1, *y1),
                to: (*x2, *y2),
            },
            *json,
            socket,
        ),
        PageAction::Type { target, text, json } => page_input::run_send(
            target,
            DocumentInput::Type { text: text.clone() },
            *json,
            socket,
        ),
        PageAction::Key { target, key, json } => match page_input::parse_key(key, *json) {
            Ok(key) => page_input::run_send(target, DocumentInput::Key { key }, *json, socket),
            Err(code) => code,
        },
        PageAction::Scroll {
            target,
            dx,
            dy,
            json,
        } => page_input::run_send(
            target,
            DocumentInput::Scroll { dx: *dx, dy: *dy },
            *json,
            socket,
        ),
        PageAction::Paste { target, text, json } => page_input::run_send(
            target,
            DocumentInput::Paste {
                text: text.clone(),
                chord: true,
            },
            *json,
            socket,
        ),
        PageAction::Copy { target, json } => page_input::run_copy(target, *json, socket),
    }
}
