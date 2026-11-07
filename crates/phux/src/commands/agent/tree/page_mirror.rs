//! `phux agent page mirror PAGE`: the browser producer, over the Chrome
//! `DevTools` Protocol. A page is two documents in phux's ladder: its
//! accessibility tree (semantic, for an agent) and its screencast (pixels,
//! for a person), published into an `AccessibilityTree` and a
//! `PixelSurface` under one pane. No browser kind of its own: viewers'
//! input intents (`SEND_DOCUMENT_INPUT`) reach this producer as relayed
//! events, and it carries them out over `DevTools`.
//!
//! The browser must listen for `DevTools` clients, for example
//! `chrome --remote-debugging-port=9222`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt as _, StreamExt as _};
use phux_client::attach::connection::Connection;
use phux_client::tree::{self, Kind};
use phux_protocol::ids::ResourceId;
use phux_server::runtime::default_socket_path;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

use super::page_input::{Viewport, cdp_calls};
use super::{dial, png_size, report, resolve};
use crate::commands::{cli_runtime, json_err};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// How the mirror was asked to run.
pub(super) struct MirrorArgs<'a> {
    /// A fragment of the page's title or URL; `first` for the first page.
    pub(super) page: &'a str,
    /// The browser's `DevTools` port.
    pub(super) port: u16,
    /// The pane the documents bind to; this process's own pane when absent.
    pub(super) pane: Option<&'a str>,
    /// Milliseconds between accessibility-tree reads.
    pub(super) interval_ms: u64,
    pub(super) json: bool,
    pub(super) socket: Option<PathBuf>,
}

/// A `DevTools` page, as `/json` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Page {
    pub(super) title: String,
    pub(super) url: String,
    pub(super) ws: String,
}

/// Spawn's provider field is a short label, not a lossless browser URL. The
/// producer retains the full source address in its startup receipt.
fn page_label(page: &Page) -> &str {
    let label = if page.title.trim().is_empty() {
        &page.url
    } else {
        &page.title
    };
    if label.is_empty() {
        return "Browser page";
    }
    let mut end = label
        .len()
        .min(phux_protocol::wire::frame::MAX_RESOURCE_PROVIDER_BYTES);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    &label[..end]
}

/// The page whose title or URL contains `spec` (`first` takes the first),
/// among the `page` targets of a `/json` listing.
pub(super) fn pick_page(spec: &str, listing: &str) -> Result<Page, String> {
    let targets: Vec<serde_json::Value> = serde_json::from_str(listing.trim())
        .map_err(|err| format!("the DevTools page list was not readable: {err}"))?;
    targets
        .iter()
        .filter(|t| t["type"].as_str() == Some("page"))
        .filter_map(|t| {
            Some(Page {
                title: t["title"].as_str().unwrap_or_default().to_owned(),
                url: t["url"].as_str().unwrap_or_default().to_owned(),
                ws: t["webSocketDebuggerUrl"].as_str()?.to_owned(),
            })
        })
        .find(|page| spec == "first" || page.title.contains(spec) || page.url.contains(spec))
        .ok_or_else(|| format!("no page matches {spec:?}"))
}

/// What one `DevTools` message means to the mirror.
#[derive(Debug, PartialEq)]
pub(super) enum Incoming {
    /// A screencast frame: its PNG, the session id to acknowledge, and the
    /// viewport's CSS size from its metadata.
    Frame {
        png: Vec<u8>,
        session: u64,
        css: Option<(f64, f64)>,
    },
    /// The answer to `Accessibility.getFullAXTree`: the tree document.
    Tree(Vec<u8>),
    Other,
}

/// Classify one message; `tree_request` is the id of the last tree read.
pub(super) fn classify(text: &str, tree_request: u64) -> Incoming {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(text) else {
        return Incoming::Other;
    };
    if message["method"].as_str() == Some("Page.screencastFrame") {
        let params = &message["params"];
        let Some(png) = params["data"]
            .as_str()
            .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
        else {
            return Incoming::Other;
        };
        let metadata = &params["metadata"];
        let css = metadata["deviceWidth"]
            .as_f64()
            .zip(metadata["deviceHeight"].as_f64());
        return Incoming::Frame {
            png,
            session: params["sessionId"].as_u64().unwrap_or(0),
            css,
        };
    }
    if message["id"].as_u64() == Some(tree_request) && message["result"]["nodes"].is_array() {
        return Incoming::Tree(serde_json::to_vec(&message["result"]).unwrap_or_default());
    }
    Incoming::Other
}

/// `GET /json` from the browser's `DevTools` server, by hand: one request,
/// one small body, no HTTP client to carry. Chrome answers HTTP/1.1 only
/// and keeps the connection open, so the body is read by its length.
async fn list_pages(port: u16) -> Result<String, String> {
    let mut tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|err| format!("no DevTools server on port {port}: {err}"))?;
    tcp.write_all(
        format!("GET /json HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .map_err(|err| err.to_string())?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = tcp.read(&mut chunk).await.map_err(|err| err.to_string())?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        if let Some(body) = http_body(&raw) {
            return Ok(body.to_owned());
        }
    }
    http_body(&raw)
        .map(str::to_owned)
        .ok_or_else(|| "the DevTools server sent no body".to_owned())
}

/// The body of a response once its `Content-Length` has fully arrived.
fn http_body(raw: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(raw).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    (body.len() >= length).then_some(body)
}

pub(super) fn run_mirror(args: MirrorArgs<'_>) -> ExitCode {
    let MirrorArgs {
        page,
        port,
        pane,
        interval_ms,
        json,
        socket,
    } = args;
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let socket_path = socket.unwrap_or_else(default_socket_path);
        let pane = match mirror_parent(&socket_path, pane, json).await {
            Ok(pane) => pane,
            Err(code) => return code,
        };
        let found = match list_pages(port).await.and_then(|listing| pick_page(page, &listing)) {
            Ok(found) => found,
            Err(message) => return usage(json, &message),
        };
        let mut conn = match dial(&socket_path, json).await {
            Ok(conn) => conn,
            Err(code) => return code,
        };
        let (tree_res, surface) = match open_page_resources(&mut conn, &pane, &found).await {
            Ok(resources) => resources,
            Err(err) => return report(json, &err),
        };
        let (tree_id, surface_id) = (
            phux_client::selector::format_terminal_id(&tree_res),
            phux_client::selector::format_terminal_id(&surface),
        );
        if json {
            outln!(
                "{}",
                serde_json::json!({ "schema_version": 1, "tree": tree_id, "surface": surface_id, "title": found.title, "url": found.url })
            );
        } else {
            outln!("{tree_id} {surface_id}");
        }
        let interval = Duration::from_millis(interval_ms.max(100));
        match mirror(&mut conn, &tree_res, &surface, &found.ws, interval).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(Failure::Tree(err)) => report(json, &err),
            Err(Failure::Browser(message)) => json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::WORKLOAD,
                    format!("the browser connection failed: {message}"),
                    "start the browser with `--remote-debugging-port` and keep the page open",
                ),
                EXIT_FAILURE,
            ),
        }
    })
}

async fn mirror_parent(
    socket: &std::path::Path,
    pane: Option<&str>,
    json: bool,
) -> Result<ResourceId, ExitCode> {
    match pane {
        Some(selector) => resolve(socket, selector, "agent page mirror", json).await,
        None => own_pane()
            .ok_or_else(|| usage(json, "not running in a phux pane and no `--pane` given")),
    }
}

async fn open_page_resources(
    conn: &mut Connection,
    parent: &ResourceId,
    page: &Page,
) -> Result<(ResourceId, ResourceId), tree::TreeError> {
    let label = page_label(page);
    let tree = tree::open_kind(conn, Kind::Tree, parent, label)
        .await?
        .resource;
    let surface = tree::open_kind(conn, Kind::Surface, parent, label)
        .await?
        .resource;
    Ok((tree, surface))
}

enum Failure {
    Tree(tree::TreeError),
    Browser(String),
}

impl From<tree::TreeError> for Failure {
    fn from(err: tree::TreeError) -> Self {
        Self::Tree(err)
    }
}

/// One `DevTools` connection and the next request id on it.
struct Cdp {
    ws: tokio_tungstenite::WebSocketStream<TcpStream>,
    next_id: u64,
}

impl Cdp {
    async fn connect(ws_url: &str) -> Result<Self, Failure> {
        let host = ws_url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .ok_or_else(|| Failure::Browser(format!("unexpected `DevTools` URL {ws_url}")))?;
        let tcp = TcpStream::connect(host)
            .await
            .map_err(|err| Failure::Browser(err.to_string()))?;
        let (ws, _) = tokio_tungstenite::client_async(ws_url, tcp)
            .await
            .map_err(|err| Failure::Browser(err.to_string()))?;
        Ok(Self { ws, next_id: 1 })
    }

    /// Send one method call and return its id.
    async fn call(&mut self, method: &str, params: serde_json::Value) -> Result<u64, Failure> {
        let id = self.next_id;
        self.next_id += 1;
        let text = serde_json::json!({ "id": id, "method": method, "params": params }).to_string();
        self.ws
            .send(Message::Text(text.into()))
            .await
            .map_err(|err| Failure::Browser(err.to_string()))?;
        Ok(id)
    }

    async fn read_tree(&mut self) -> Result<u64, Failure> {
        self.call("Accessibility.getFullAXTree", serde_json::json!({}))
            .await
    }

    /// Carry out one viewer intent.
    async fn act(&mut self, intent: &tree::Intent, view: Viewport) -> Result<(), Failure> {
        for (method, params) in cdp_calls(&intent.input, view) {
            self.call(method, params).await?;
        }
        Ok(())
    }
}

/// Drive the page until Ctrl-C or until the browser hangs up: every
/// screencast frame goes to the surface and is acknowledged, the
/// accessibility tree is read every `interval` and published when changed,
/// and every input intent a viewer sends is carried out, then the tree is
/// read again.
#[allow(
    clippy::future_not_send,
    reason = "the CLI runtime is single-threaded; the connection is not shared"
)]
async fn mirror(
    conn: &mut Connection,
    tree_res: &ResourceId,
    surface: &ResourceId,
    ws_url: &str,
    interval: Duration,
) -> Result<(), Failure> {
    let mut cdp = Cdp::connect(ws_url).await?;
    cdp.call("Page.enable", serde_json::json!({})).await?;
    cdp.call("Accessibility.enable", serde_json::json!({}))
        .await?;
    cdp.call(
        "Page.startScreencast",
        serde_json::json!({ "format": "png", "everyNthFrame": 1 }),
    )
    .await?;
    let mut tree_request = cdp.read_tree().await?;
    let mut last_tree: Option<Vec<u8>> = None;
    let mut tick = tokio::time::interval(interval);
    let mut view = Viewport::IDENTITY;
    let mut intents = Vec::new();
    loop {
        if !intents.is_empty() {
            for intent in std::mem::take(&mut intents) {
                cdp.act(&intent, view).await?;
            }
            tree_request = cdp.read_tree().await?;
        }
        let message = tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tick.tick() => {
                tree_request = cdp.read_tree().await?;
                continue;
            }
            frame = conn.recv() => {
                match frame {
                    Ok(frame) => intents.extend(tree::intent(&frame)),
                    Err(phux_client::attach::AttachError::Disconnected) => return Ok(()),
                    Err(err) => return Err(Failure::Tree(err.into())),
                }
                continue;
            }
            message = cdp.ws.next() => message,
        };
        let text = match message {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => continue,
            Some(Err(err)) => return Err(Failure::Browser(err.to_string())),
        };
        match classify(&text, tree_request) {
            Incoming::Frame { png, session, css } => {
                if let (Some(image), Some(css)) = (png_size(&png), css) {
                    view = Viewport::of(image, css);
                }
                tree::publish_collecting(conn, surface, png, &mut intents).await?;
                cdp.call(
                    "Page.screencastFrameAck",
                    serde_json::json!({ "sessionId": session }),
                )
                .await?;
            }
            Incoming::Tree(bytes) => {
                if last_tree.as_deref() != Some(bytes.as_slice()) {
                    tree::publish_collecting(conn, tree_res, bytes.clone(), &mut intents).await?;
                    last_tree = Some(bytes);
                }
            }
            Incoming::Other => {}
        }
    }
}

fn usage(json: bool, message: &str) -> ExitCode {
    json_err::emit(
        json,
        &json_err::CliError::new(
            json_err::codes::USAGE_ERROR,
            message,
            "start the browser with `--remote-debugging-port=9222`, run inside a pane or pass `--pane @N`",
        ),
        EXIT_USAGE,
    )
}

/// The pane this process runs in, from the injected `PHUX_TERMINAL_ID`.
fn own_pane() -> Option<ResourceId> {
    let raw = std::env::var("PHUX_TERMINAL_ID").ok()?;
    raw.parse::<u32>().ok().map(ResourceId::local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::wire::frame::SpawnResource;

    const LISTING: &str = r#"[
        {"type":"background_page","title":"ext","url":"chrome-extension://x","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/A"},
        {"type":"page","title":"phux - terminal multiplexer","url":"https://phux.sh/","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/B"},
        {"type":"page","title":"Docs","url":"https://phux.sh/docs","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/C"}
    ]"#;

    #[test]
    fn browser_labels_fit_spawn_bounds_without_changing_source_identity() {
        let mut page = Page {
            title: "雪".repeat(50),
            url: format!("https://example.test/{}", "a".repeat(250)),
            ws: "ws://localhost/devtools/page/1".into(),
        };
        let original = page.clone();
        assert_eq!(page_label(&page).len(), 120);
        assert_eq!(page, original);
        page.title.clear();
        assert_eq!(page_label(&page).len(), 120);
        page.url.clear();
        assert_eq!(page_label(&page), "Browser page");
        page.title = format!("{}雪", "a".repeat(119));
        assert_eq!(page_label(&page), "a".repeat(119));
        let label = page_label(&page);
        for resource in [
            SpawnResource::accessibility_tree(ResourceId::local(1), label),
            SpawnResource::pixel_surface(ResourceId::local(1), label),
        ] {
            assert!(
                resource.provider.unwrap().len()
                    <= phux_protocol::wire::frame::MAX_RESOURCE_PROVIDER_BYTES
            );
        }
    }

    #[test]
    fn a_page_is_picked_by_title_or_url_and_never_an_extension() {
        assert!(pick_page("first", LISTING).unwrap().ws.ends_with("/B"));
        assert_eq!(
            pick_page("Docs", LISTING).unwrap().url,
            "https://phux.sh/docs"
        );
        assert_eq!(pick_page("/docs", LISTING).unwrap().title, "Docs");
        assert!(pick_page("ext", LISTING).is_err());
    }

    #[test]
    fn a_body_is_complete_once_its_content_length_has_arrived() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length:5\r\n\r\n[1,2";
        assert_eq!(http_body(raw), None);
        assert_eq!(
            http_body(b"HTTP/1.1 200 OK\r\nContent-Length:5\r\n\r\n[1,2]"),
            Some("[1,2]")
        );
    }

    #[test]
    fn a_screencast_frame_and_a_tree_answer_are_told_apart() {
        let frame = r#"{"method":"Page.screencastFrame","params":{"data":"iVBORw0=","sessionId":7,"metadata":{"deviceWidth":800,"deviceHeight":600}}}"#;
        assert_eq!(
            classify(frame, 4),
            Incoming::Frame {
                png: vec![0x89, 0x50, 0x4e, 0x47, 0x0d],
                session: 7,
                css: Some((800.0, 600.0)),
            }
        );
        let tree = r#"{"id":4,"result":{"nodes":[{"nodeId":"1","role":{"value":"RootWebArea"}}]}}"#;
        assert!(
            matches!(classify(tree, 4), Incoming::Tree(bytes) if bytes.starts_with(b"{\"nodes\""))
        );
        assert_eq!(classify(tree, 5), Incoming::Other);
        assert_eq!(classify(r#"{"id":2,"result":{}}"#, 4), Incoming::Other);
    }
}
