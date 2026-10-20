//! Browser glue: a transport to the phux server (WebTransport or WebSocket),
//! a `<canvas>`, and the keyboard, all driving a [`Connection`](crate::Connection).
//! This is the only part that touches the DOM/network; the protocol logic
//! lives in [`crate::session`].
//!
//! Two connect paths speak the identical wire (ADR-0025: the transport is a
//! byte-stream detail below the frame codec):
//!
//! * **WebSocket** ([`run`]) — one binary message per encoded frame. The
//!   historical path; works everywhere.
//! * **WebTransport** ([`run_with_fallback`]) — HTTP/3 over QUIC. One
//!   bidirectional stream carries length-prefixed frames (reassembled by
//!   [`FrameBuffer`](crate::framing::FrameBuffer)); falls back to WebSocket
//!   when the API or the endpoint is unavailable.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
#[cfg(test)]
use std::rc::Weak;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
use gloo_timers::future::TimeoutFuture;
use phux_protocol::BootstrapProfile;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyEvent, ModSet};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::FrameKind;
use phux_vt_web::{Grid, Vt};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    BinaryType, CanvasRenderingContext2d, ClipboardEvent, CompositionEvent, HtmlCanvasElement,
    HtmlTextAreaElement, KeyboardEvent, MessageEvent, ReadableStreamDefaultReader, WebSocket,
    WebTransport, WritableStreamDefaultWriter,
};

use crate::framing::FrameBuffer;
use crate::input::WheelAction;
use crate::search::{Search, find_matches, reveal_row};
use crate::selection::{Selection, cell_at};
use crate::session::is_version_refusal;
use crate::touch::{TouchGesture, TouchMode, TouchStep};
use crate::{Mark, Metrics, Overlay, render_cursor_row_appearance, render_selected_appearance};

mod app;
mod connect;
mod find;
mod gestures;
mod handle;
mod handlers;
mod keyboard;
mod paint;
mod path_picker;
mod pointer;
mod receive;
mod transport;

use app::*;
pub use connect::*;
use gestures::*;
use handlers::*;
use keyboard::*;
use pointer::*;
use receive::*;
#[cfg(test)]
use transport::await_or_cancel;

const CONNECT_DEADLINE_MS: u32 = 10_000;
const MAX_OUTBOUND_BYTES: usize = 1024 * 1024;
const PHUX_WS_PROTOCOL: &str = "phux.v1";
const PHUX_WS_BEARER_PREFIX: &str = "phux.bearer.";
const BOOTSTRAP_EXPIRY_POLL_MS: i32 = 1_000;

#[cfg(test)]
thread_local! {
    static RETAINED_APP_FOR_TEST: RefCell<Option<Weak<RefCell<App>>>> = const {
        RefCell::new(None)
    };
}

struct AttemptDeadline {
    end_ms: f64,
}

struct WebTransportAttempt {
    session: Option<WebTransport>,
}

struct WebSocketAttempt {
    socket: Option<WebSocket>,
}

struct AppEstablishment {
    app: Option<Rc<RefCell<App>>>,
}

/// Canvas attribute naming the connection state: `connected` once attached,
/// `disconnected` after the transport or protocol fails (while it redials),
/// `incompatible` when the server is on another protocol major and no redial
/// can help.
pub const CONNECTION_ATTRIBUTE: &str = "data-phux-connection";

/// Canvas attribute holding why the connection stopped for good, set with
/// `incompatible`.
pub const ERROR_ATTRIBUTE: &str = "data-phux-error";

/// Canvas attribute holding the title the program set (OSC 0/2).
pub const TITLE_ATTRIBUTE: &str = "data-phux-title";

/// Bubbling `CustomEvent` dispatched on the canvas when the program's title
/// changes; `detail` is the new title (empty when cleared).
pub const TITLE_EVENT: &str = "phux-title";

/// Bubbling `CustomEvent` dispatched on the canvas when the program rings
/// the bell (BEL). The canvas also flashes briefly, unless the page prefers
/// reduced motion.
pub const BELL_EVENT: &str = "phux-bell";

/// Bubbling event after a full real canvas paint. `detail` has canonical
/// `panes`, total cell `cols`/`rows`, and backing `pixelWidth`/`pixelHeight`.
/// Cursor-only blink redraws do not emit it. Listeners may call the client.
pub const RENDER_EVENT: &str = "phux-render";

/// How long the visual bell shows, and the least time between two flashes.
const BELL_FLASH_MS: u32 = 150;

/// The visual bell: a translucent wash over the whole canvas.
const BELL_FLASH_FILL: &str = "rgba(255, 255, 255, 0.18)";

/// DOM id of the element that holds the focused pane's agent badges.
pub const BADGE_CONTAINER_ID: &str = "phux-agent-badges";

/// A live connection handle. The event handlers run for the connection's
/// lifetime; this lets a caller (or test) inspect the rendered grid.
pub struct Client {
    app: Rc<RefCell<App>>,
}

/// The send half of whichever transport carried the connection. Both carry
/// identical encoded frames; only the byte-stream mechanics differ.
enum WireTx {
    /// One binary message per frame.
    Ws(WsTx),
    /// Length-prefixed frames over the session's single bidirectional stream.
    Wt(Rc<WtTx>),
}

type FailureCallback = Rc<dyn Fn(String)>;

#[derive(Clone, Default)]
struct FailureHook(Rc<RefCell<Option<FailureCallback>>>);

struct WsTx {
    socket: WebSocket,
    failure: FailureHook,
}

struct WtTx {
    writer: WritableStreamDefaultWriter,
    session: WebTransport,
    queue: RefCell<OutboundQueue>,
    writing: Cell<bool>,
    failed: Cell<bool>,
    cancel: RefCell<Option<oneshot::Sender<()>>>,
    failure: FailureHook,
}

enum PromiseOutcome<T> {
    Completed(Result<T, JsValue>, oneshot::Receiver<()>),
    Cancelled,
}

#[derive(Default)]
struct OutboundQueue {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
}

#[derive(Default)]
struct AppBindings {
    websocket: Option<WebSocketBindings>,
    input: Option<InputBinding>,
    find: Option<find::FindBinding>,
    path_picker: Option<path_picker::PickerBinding>,
    blink: Option<BlinkBinding>,
    frame: Option<FrameBinding>,
    bootstrap_expiry: Option<BlinkBinding>,
    wt_reader_cancel: Option<oneshot::Sender<()>>,
}

struct WebSocketBindings {
    socket: WebSocket,
    onopen: Closure<dyn FnMut()>,
    onmessage: Closure<dyn FnMut(MessageEvent)>,
    onerror: Closure<dyn FnMut(web_sys::Event)>,
    onclose: Closure<dyn FnMut(web_sys::Event)>,
}

/// Class of the hidden `<textarea>` that owns terminal input.
pub const INPUT_SURFACE_CLASS: &str = "phux-web-input";

/// Class of the find bar mounted after the canvas (hidden until opened);
/// its field is its `input`, its match count `.phux-find-count`.
pub const FIND_BAR_CLASS: &str = "phux-find";

/// The input surface and every listener it and the canvas carry.
struct InputBinding {
    surface: HtmlTextAreaElement,
    listeners: Listeners,
}

/// DOM event listeners removed together.
#[derive(Default)]
struct Listeners(Vec<Listener>);

struct Listener {
    target: web_sys::EventTarget,
    kind: &'static str,
    callback: Closure<dyn FnMut(web_sys::Event)>,
}

/// The one animation-frame callback that paints, and its pending request.
struct FrameBinding {
    window: web_sys::Window,
    callback: Closure<dyn FnMut()>,
    pending: Cell<Option<i32>>,
}

struct BlinkBinding {
    window: web_sys::Window,
    interval: i32,
    callback: Closure<dyn FnMut()>,
}

struct App {
    session: crate::Connection,
    tx: WireTx,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    metrics: Metrics,
    canvas_geometry: Cell<crate::canvas::CanvasGeometry>,
    host_paths_enabled: bool,
    /// The explicit hosted edge backend has no event stream or host filesystem.
    hosted_edge: bool,
    prefix: Cell<bool>,
    /// Cursor blink phase; toggled by an interval in `run`.
    cursor_on: Cell<bool>,
    /// Opt-in appearance; terminal defaults remain opaque elsewhere.
    glass_mode: Cell<bool>,
    /// Fractional wheel rows not yet scrolled (trackpads send small deltas).
    wheel_carry: Cell<f64>,
    /// The grid the canvas shows, so a cursor blink redraws one row
    /// without reading the whole grid back from the engine.
    painted: RefCell<Option<Grid>>,
    /// Pane-local cursor rows from the last full paint, without rereading grids.
    painted_cursor_rows: RefCell<Vec<(phux_protocol::ids::ResourceId, u16)>>,
    /// The program title last published to the page.
    title: RefCell<String>,
    /// The search matches the canvas shows, so a cursor blink keeps them.
    painted_marks: RefCell<Vec<Mark>>,
    /// The mouse selection over the viewport, and whether a drag is live.
    selection: Cell<Option<Selection>>,
    selecting: Cell<bool>,
    /// Find in the terminal, and whether new output has made its matches
    /// stale.
    search: RefCell<Search>,
    search_stale: Cell<bool>,
    /// The button a press forwarded to the program holds down, and the
    /// last cell a mouse report named (motion reports once per cell).
    forwarded_button: Cell<Option<MouseButton>>,
    mouse_cell: Cell<Option<(u16, u16)>>,
    /// Whether the pointer shows a link under a held Command/Ctrl.
    link_hover: Cell<bool>,
    /// The finger gesture over the canvas, while one is down.
    touch: Cell<Option<TouchGesture>>,
    /// An on-screen Ctrl held for the next typed key (a page's key bar sets
    /// it; a phone keyboard has no Ctrl of its own).
    ctrl_latch: Cell<bool>,
    /// When the visual bell last started, on the monotonic clock.
    bell_started_ms: Cell<f64>,
    bindings: RefCell<AppBindings>,
    ready: RefCell<Option<oneshot::Sender<Result<(), String>>>>,
    failure_reason: RefCell<Option<String>>,
    reconnect: RefCell<Option<ReconnectConfig>>,
    self_owner: RefCell<Option<Rc<RefCell<App>>>>,
}

type AppReady = (Rc<RefCell<App>>, oneshot::Receiver<Result<(), String>>);

struct ReconnectConfig {
    wt_url: Option<String>,
    ws_url: String,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FallbackToken<'a> {
    Missing,
    Valid(&'a str),
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiveFlow {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebTransportExit<'a> {
    CleanEof,
    PartialEof,
    ReadError(&'a str),
    MissingValue,
}

#[derive(Clone, Copy)]
struct BatchEffects {
    flow: ReceiveFlow,
    render: bool,
    badges: bool,
    bell: bool,
}

/// Inline style of the input surface: invisible and inert to the pointer,
/// but a real focusable text control so the browser runs IME composition,
/// dead keys, mobile keyboards, and clipboard paste against it.
/// Its 16px font keeps iOS from zooming the page when it takes focus.
const INPUT_SURFACE_STYLE: &str = "position:fixed;width:1px;height:1px;padding:0;border:0;\
margin:0;opacity:0;resize:none;overflow:hidden;white-space:pre;pointer-events:none;\
caret-color:transparent;font-size:16px;";

/// The canvas's own touch handling: the browser neither pans nor zooms over
/// it (a finger scrolls the terminal instead), nor raises a text-selection
/// callout on a long press (which selects terminal cells).
const CANVAS_TOUCH_STYLE: [(&str, &str); 3] = [
    ("touch-action", "none"),
    ("user-select", "none"),
    ("-webkit-touch-callout", "none"),
];

type InputHandler = fn(&Rc<RefCell<App>>, &web_sys::Event, &HtmlTextAreaElement);

/// Where one pointer event goes.
enum PointerRoute {
    /// Local selection handles it.
    Local,
    /// The program owns the pointer but asked for no report of this event.
    Swallow,
    /// Report it to the program.
    Report(MouseAction, MouseButton),
}

/// Most wheel clicks one wheel event reports, so a flung trackpad cannot
/// flood the program.
const MAX_WHEEL_CLICKS: i32 = 10;

/// Most arrow keys one wheel event sends: the rows of
/// [`MAX_WHEEL_CLICKS`] clicks.
const MAX_WHEEL_ARROWS: i32 = 30;

enum PaneAction<'a> {
    Split(&'a str),
    Next,
    Focus(&'a phux_protocol::ids::ResourceId),
    CloseView,
    Kill,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod presentation_tests;
