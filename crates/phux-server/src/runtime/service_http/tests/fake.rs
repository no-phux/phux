//! A fake `OpenCode` for the service tests: a loopback hyper server that
//! speaks the pairing exchange and a few test routes, and a shell script
//! standing in for the `opencode` CLI.

use std::collections::HashSet;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::channel::{Channel, Sender};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::Notify;

use super::super::opencode::OpenCode;

type Body = BoxBody<Bytes, Infallible>;

/// What the fake service saw and decides.
#[derive(Debug, Default)]
pub(super) struct Seen {
    /// Tokens refused with `401` although they were minted.
    pub(super) revoked: HashSet<String>,
    /// Connections whose serving ended (the client closed them).
    pub(super) closed_connections: usize,
    /// The held event stream, fed by [`Fake::send_event`].
    pub(super) events: Option<Sender<Bytes>>,
}

#[derive(Debug)]
pub(super) struct Fake {
    pub(super) addr: SocketAddr,
    pub(super) seen: Arc<Mutex<Seen>>,
    pub(super) closed: Arc<Notify>,
    accept: tokio::task::JoinHandle<()>,
    dir: tempfile::TempDir,
}

const SCRIPT: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
case "$1 $2" in
  "service status") cat "$dir/origin" 2>/dev/null || echo stopped ;;
  "service start") cp "$dir/origin.next" "$dir/origin" ;;
  "api server.pair")
    n=$(cat "$dir/pairs" 2>/dev/null || echo 0)
    n=$((n + 1))
    echo "$n" > "$dir/pairs"
    printf '{"code":"code-%s","expires_in":300}\n' "$n" ;;
  *) exit 2 ;;
esac
"#;

impl Fake {
    /// A fake service whose CLI reports it stopped until `service start`.
    pub(super) async fn stopped() -> Self {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let closed = Arc::new(Notify::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(accept(listener, seen.clone(), closed.clone()));
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("opencode");
        std::fs::write(&script, SCRIPT).unwrap();
        make_executable(&script);
        std::fs::write(dir.path().join("origin.next"), format!("http://{addr}\n")).unwrap();
        Self {
            addr,
            seen,
            closed,
            accept,
            dir,
        }
    }

    /// The fake CLI.
    pub(super) fn cli(&self) -> PathBuf {
        self.dir.path().join("opencode")
    }

    /// A provider for this fake, leaked: providers are process statics.
    pub(super) fn provider(&self) -> &'static OpenCode {
        Box::leak(Box::new(OpenCode::at(self.cli())))
    }

    /// How many pairing codes the CLI printed.
    pub(super) fn pairs(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pairs"))
            .map_or(0, |pairs| pairs.trim().parse().unwrap())
    }

    /// Point the CLI's status at `addr`, as a restarted service would.
    pub(super) fn report_origin(&self, addr: SocketAddr) {
        std::fs::write(self.dir.path().join("origin"), format!("http://{addr}\n")).unwrap();
    }

    /// Forget the start target, so `service start` fails.
    pub(super) fn break_start(&self) {
        std::fs::remove_file(self.dir.path().join("origin.next")).unwrap();
    }

    /// Stop accepting connections, as a stopped service would.
    pub(super) fn stop(&self) {
        self.accept.abort();
    }

    pub(super) async fn send_event(&self, event: &'static str) {
        let mut events = self.seen.lock().unwrap().events.take().unwrap();
        events
            .send_data(Bytes::from_static(event.as_bytes()))
            .await
            .unwrap();
        self.seen.lock().unwrap().events = Some(events);
    }

    pub(super) fn closed_connections(&self) -> usize {
        self.seen.lock().unwrap().closed_connections
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

async fn accept(listener: TcpListener, seen: Arc<Mutex<Seen>>, closed: Arc<Notify>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let seen = seen.clone();
        let closed = closed.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn({
                let seen = seen.clone();
                move |request| {
                    let response = route(&request, &seen);
                    async move {
                        match response {
                            Some(response) => Ok::<_, Infallible>(response),
                            // `/hang`: the head never arrives.
                            None => std::future::pending().await,
                        }
                    }
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
            seen.lock().unwrap().closed_connections += 1;
            closed.notify_waiters();
        });
    }
}

fn route(request: &Request<Incoming>, seen: &Mutex<Seen>) -> Option<Response<Body>> {
    let path = request.uri().path();
    if let Some(code) = path.strip_prefix("/auth/connect/") {
        let token = code.replace("code-", "tok-");
        return Some(text(StatusCode::OK, format!(r#"{{"token":"{token}"}}"#)));
    }
    if !authorized(request, seen) {
        let mut refused = text(StatusCode::UNAUTHORIZED, "unauthorized".to_owned());
        let challenge = "Basic realm=\"Secure Area\"".parse().unwrap();
        refused.headers_mut().insert("www-authenticate", challenge);
        return Some(refused);
    }
    Some(match path {
        "/echo" => echo(request),
        "/chunked" => chunked(),
        "/sse" => sse(seen),
        // The request's own refusal, not the token's.
        "/denied" => text(StatusCode::UNAUTHORIZED, "not yours".to_owned()),
        "/hang" => return None,
        _ => text(StatusCode::NOT_FOUND, "no route".to_owned()),
    })
}

fn authorized(request: &Request<Incoming>, seen: &Mutex<Seen>) -> bool {
    use base64::Engine as _;
    let Some(value) = request.headers().get(http::header::AUTHORIZATION) else {
        return false;
    };
    let encoded = value.to_str().unwrap().strip_prefix("Basic ").unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let credentials = String::from_utf8(decoded).unwrap();
    let token = credentials.strip_prefix("opencode:").unwrap();
    token.starts_with("tok-") && !seen.lock().unwrap().revoked.contains(token)
}

fn text(status: StatusCode, body: String) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from(body)).boxed());
    *response.status_mut() = status;
    response
}

/// The request's headers, one `name: value` line each, with response
/// headers the policy must strip.
fn echo(request: &Request<Incoming>) -> Response<Body> {
    let lines = request
        .headers()
        .iter()
        .fold(String::new(), |mut lines, (name, value)| {
            let _ = writeln!(lines, "{name}: {}", value.to_str().unwrap());
            lines
        });
    let mut response = text(StatusCode::OK, lines);
    let headers = response.headers_mut();
    headers.insert("set-cookie", "session=secret".parse().unwrap());
    headers.insert("connection", "x-hop".parse().unwrap());
    headers.insert("x-hop", "1".parse().unwrap());
    headers.insert("x-trace", "kept".parse().unwrap());
    response
}

fn chunked() -> Response<Body> {
    let (mut sender, body) = Channel::<Bytes, Infallible>::new(4);
    tokio::spawn(async move {
        for part in ["one", "two", "three"] {
            sender
                .send_data(Bytes::from_static(part.as_bytes()))
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
    });
    Response::new(body.boxed())
}

/// One event now; later ones when the test sends them.
fn sse(seen: &Mutex<Seen>) -> Response<Body> {
    let (mut sender, body) = Channel::<Bytes, Infallible>::new(4);
    sender
        .try_send(hyper::body::Frame::data(Bytes::from_static(b"data: 1\n\n")))
        .unwrap();
    seen.lock().unwrap().events = Some(sender);
    let mut response = Response::new(body.boxed());
    response
        .headers_mut()
        .insert("content-type", "text/event-stream".parse().unwrap());
    response
}
