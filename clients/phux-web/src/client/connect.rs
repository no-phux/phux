//! Establishing a connection: WebTransport, WebSocket, hosted, and the redial loop.

use super::*;

impl WebSocketAttempt {
    pub(super) fn new(socket: WebSocket) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    pub(super) fn socket(&self) -> &WebSocket {
        self.socket
            .as_ref()
            .expect("WebSocket attempt is still armed")
    }

    fn transfer_to_app(&mut self) {
        self.socket.take();
    }
}

impl Drop for WebSocketAttempt {
    fn drop(&mut self) {
        if let Some(socket) = self.socket.take() {
            let _ = socket.close();
        }
    }
}

impl WebTransportAttempt {
    pub(super) fn new(session: WebTransport) -> Self {
        Self {
            session: Some(session),
        }
    }

    pub(super) fn session(&self) -> &WebTransport {
        self.session
            .as_ref()
            .expect("WebTransport attempt is still armed")
    }

    fn transfer_to_app(&mut self) {
        self.session.take();
    }
}

impl Drop for WebTransportAttempt {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.close();
        }
    }
}

impl AppEstablishment {
    pub(super) fn new(app: Rc<RefCell<App>>) -> Self {
        Self { app: Some(app) }
    }

    pub(super) fn complete(mut self) -> Client {
        Client {
            app: self.app.take().expect("App establishment is still armed"),
        }
    }
}

impl Drop for AppEstablishment {
    fn drop(&mut self) {
        if let Some(app) = self.app.take() {
            app.borrow_mut().dispose();
        }
    }
}

impl AttemptDeadline {
    pub(super) fn new() -> Result<Self, JsValue> {
        let now = monotonic_now_ms()?;
        Ok(Self {
            end_ms: now + f64::from(CONNECT_DEADLINE_MS),
        })
    }

    fn remaining_ms(&self) -> u32 {
        let remaining = monotonic_now_ms()
            .map(|now| (self.end_ms - now).ceil())
            .unwrap_or(0.0);
        remaining.clamp(0.0, f64::from(CONNECT_DEADLINE_MS)) as u32
    }
}

pub(super) fn monotonic_now_ms() -> Result<f64, JsValue> {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .ok_or_else(|| JsValue::from_str("monotonic browser clock unavailable"))
}

async fn load_vt() -> Result<Rc<Vt>, JsValue> {
    match select(
        Box::pin(async {
            crate::fonts::load().await?;
            Vt::load().await
        }),
        Box::pin(TimeoutFuture::new(CONNECT_DEADLINE_MS)),
    )
    .await
    {
        Either::Left((result, _)) => result,
        Either::Right(((), _)) => Err(JsValue::from_str("terminal engine load timed out")),
    }
}

/// Connect to a phux server over WebSocket and render the attached terminal
/// into the given canvas, routing keyboard input back. Resolves only after the
/// aggregate attach reaches READY; handlers then run for the connection's lifetime.
///
/// # Errors
/// Fails if the engine can't load, the canvas has no 2D context, or the
/// WebSocket can't be opened.
pub async fn run(
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Client, JsValue> {
    let vt = load_vt().await?;
    run_websocket(ws_url, None, canvas, cols, rows, false, &vt).await
}

/// Connect over WebSocket for the hosted live-demo Worker.
///
/// The first message must be the text `phux.session.v1` envelope. HELLO waits
/// until that preamble arrives; later binary messages are the phux wire.
///
/// # Errors
/// Fails if the engine, canvas, or WebSocket cannot be initialized, or if the
/// hosted preamble is missing or invalid.
pub async fn run_hosted(
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    callback: js_sys::Function,
) -> Result<Client, JsValue> {
    let vt = load_vt().await?;
    let deadline = AttemptDeadline::new()?;
    let ws = websocket(ws_url, None)?;
    let mut attempt = WebSocketAttempt::new(ws);
    attempt.socket().set_binary_type(BinaryType::Arraybuffer);

    let app_socket = attempt.socket().clone();
    let tx = WireTx::Ws(WsTx::new(app_socket.clone()));
    let (app, ready) = build_app(&vt, tx, canvas, cols, rows, false)?;
    let app_attempt = AppEstablishment::new(Rc::clone(&app));
    attempt.transfer_to_app();
    install_transport_failure_hook(&app);

    install_hosted_websocket_handlers(&app, &app_socket, callback);
    await_protocol_ready(&app, ready, deadline.remaining_ms()).await?;
    ensure_app_live(&app)?;

    install_input(&app)?;
    path_picker::install(&app)?;
    install_cursor_blink(&app)?;

    Ok(app_attempt.complete())
}

/// Connect over WebSocket while explicitly advertising synthesized
/// compatibility profiles only.
///
/// # Errors
/// Fails if the engine, canvas, or WebSocket cannot be initialized.
pub async fn run_synthesized_compat(
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Client, JsValue> {
    let vt = load_vt().await?;
    run_websocket(ws_url, None, canvas, cols, rows, true, &vt).await
}

async fn run_websocket(
    ws_url: &str,
    bearer_hex: Option<&str>,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    synthesized_only: bool,
    vt: &Rc<Vt>,
) -> Result<Client, JsValue> {
    let deadline = AttemptDeadline::new()?;
    let ws = websocket(ws_url, bearer_hex)?;
    let mut attempt = WebSocketAttempt::new(ws);
    attempt.socket().set_binary_type(BinaryType::Arraybuffer);

    let app_socket = attempt.socket().clone();
    let tx = WireTx::Ws(WsTx::new(app_socket.clone()));
    let (app, ready) = build_app(vt, tx, canvas, cols, rows, synthesized_only)?;
    let app_attempt = AppEstablishment::new(Rc::clone(&app));
    attempt.transfer_to_app();
    install_transport_failure_hook(&app);

    install_websocket_handlers(&app, &app_socket);
    await_protocol_ready(&app, ready, deadline.remaining_ms()).await?;
    ensure_app_live(&app)?;

    install_input(&app)?;
    path_picker::install(&app)?;
    install_cursor_blink(&app)?;

    Ok(app_attempt.complete())
}

/// Connect over WebTransport (HTTP/3 over QUIC), falling back to the
/// WebSocket path when WebTransport is unavailable — an older browser
/// without the API, or a server not listening on the WebTransport endpoint.
///
/// `wt_url` is an `https://` session URL (`phux server --webtransport`; on a
/// token-authenticated listener append `?token=<hex>`, since the JS
/// `WebTransport` API cannot set request headers). `ws_url` is the WebSocket
/// fallback URL. The token is offered there through `Sec-WebSocket-Protocol`
/// rather than copied into the WSS URL; the direct server must support that
/// authenticated upgrade seam.
///
/// # Errors
/// Fails only if *both* paths fail to come up.
pub async fn run_with_fallback(
    wt_url: &str,
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Client, JsValue> {
    let fallback_token = fallback_token_from_webtransport_url(wt_url);
    if matches!(fallback_token, FallbackToken::Invalid) {
        return Err(JsValue::from_str(
            "WebTransport URL contains an invalid or duplicate token",
        ));
    }
    let vt = load_vt().await?;
    match run_webtransport_loaded(wt_url, canvas.clone(), cols, rows, &vt).await {
        Ok(client) => Ok(client),
        Err(_) => {
            web_sys::console::warn_1(&JsValue::from_str(
                "phux-web: WebTransport unavailable; falling back to WebSocket",
            ));
            let bearer = fallback_token.valid();
            run_websocket(ws_url, bearer, canvas, cols, rows, false, &vt).await
        }
    }
}

async fn run_webtransport_loaded(
    wt_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    vt: &Rc<Vt>,
) -> Result<Client, JsValue> {
    let deadline = AttemptDeadline::new()?;
    // `WebTransport::new` throws (rather than returning Err) when the API is
    // absent from the global scope; the `catch` binding surfaces both cases
    // as Err so the caller's fallback fires either way.
    let wt = WebTransport::new(wt_url)
        .map_err(|_| JsValue::from_str("WebTransport initialization failed"))?;
    let mut attempt = WebTransportAttempt::new(wt);
    if await_js_promise_with_deadline(
        attempt.session().ready(),
        "WebTransport readiness",
        deadline.remaining_ms(),
    )
    .await
    .is_err()
    {
        return Err(JsValue::from_str("WebTransport readiness failed"));
    }

    // One bidirectional stream carries the whole wire, mirroring the QUIC
    // transport's one-stream-per-connection contract.
    let stream: web_sys::WebTransportBidirectionalStream = match await_js_promise_with_deadline(
        attempt.session().create_bidirectional_stream(),
        "WebTransport stream creation",
        deadline.remaining_ms(),
    )
    .await
    {
        Ok(stream) => stream,
        Err(_) => return Err(JsValue::from_str("WebTransport stream creation failed")),
    };
    let writer = WritableStreamDefaultWriter::new(&stream.writable())
        .map_err(|_| JsValue::from_str("WebTransport writer initialization failed"))?;
    let reader = ReadableStreamDefaultReader::new(&stream.readable())
        .map_err(|_| JsValue::from_str("WebTransport reader initialization failed"))?;

    let reader_session = attempt.session().clone();
    let tx = WireTx::Wt(Rc::new(WtTx::new(writer, attempt.session().clone())));
    let (app, ready) = build_app(vt, tx, canvas, cols, rows, false)?;
    let app_attempt = AppEstablishment::new(Rc::clone(&app));
    attempt.transfer_to_app();
    install_transport_failure_hook(&app);

    // The session is already established (unlike the WebSocket path there is
    // no onopen moment): send HELLO now; ATTACH follows HELLO_OK.
    {
        send_handshake(&app);
    }

    // Read pump: stream chunks land at arbitrary boundaries, so reassemble
    // complete frames before decoding. `wt` is moved in to keep the session
    // handle alive for the pump's lifetime.
    {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        app.borrow()
            .bindings
            .borrow_mut()
            .wt_reader_cancel
            .replace(cancel_tx);
        let app = Rc::clone(&app);
        wasm_bindgen_futures::spawn_local(async move {
            run_webtransport_reader(app, reader, reader_session, cancel_rx).await;
        });
    }

    await_protocol_ready(&app, ready, deadline.remaining_ms()).await?;
    ensure_app_live(&app)?;

    install_input(&app)?;
    path_picker::install(&app)?;
    install_cursor_blink(&app)?;

    Ok(app_attempt.complete())
}

pub(super) fn spawn_reconnect(config: ReconnectConfig) {
    wasm_bindgen_futures::spawn_local(async move {
        TimeoutFuture::new(250).await;
        loop {
            let canvas = config.canvas.clone();
            let attempt = match config.wt_url.as_deref() {
                Some(wt_url) => {
                    run_with_fallback(wt_url, &config.ws_url, canvas, config.cols, config.rows)
                        .await
                }
                None => run(&config.ws_url, canvas, config.cols, config.rows).await,
            };
            match attempt {
                Ok(client) => {
                    client.enable_auto_reconnect(config.wt_url.as_deref(), &config.ws_url);
                    return;
                }
                Err(error) => {
                    if let Some(message) = error.as_string().filter(|m| is_version_refusal(m)) {
                        mark_incompatible(&config.canvas, &message);
                        return;
                    }
                    TimeoutFuture::new(1_000).await;
                }
            }
        }
    });
}

/// Stop on a protocol-major refusal and say why: the canvas is marked
/// `incompatible` with the reason in [`ERROR_ATTRIBUTE`], and the console
/// carries it too.
pub(super) fn mark_incompatible(canvas: &HtmlCanvasElement, message: &str) {
    web_sys::console::error_1(&JsValue::from_str(&format!("phux-web: {message}")));
    let _ = canvas.set_attribute(CONNECTION_ATTRIBUTE, "incompatible");
    let _ = canvas.set_attribute(ERROR_ATTRIBUTE, message);
    let _ = canvas.set_attribute("title", message);
}

pub(super) fn websocket(ws_url: &str, bearer_hex: Option<&str>) -> Result<WebSocket, JsValue> {
    let Some(token) = bearer_hex else {
        return WebSocket::new(ws_url);
    };
    let protocols = js_sys::Array::new();
    for protocol in authenticated_websocket_protocols(token) {
        protocols.push(&JsValue::from_str(&protocol));
    }
    WebSocket::new_with_str_sequence(ws_url, protocols.as_ref())
        .map_err(|_| JsValue::from_str("authenticated WebSocket initialization failed"))
}

pub(super) fn authenticated_websocket_protocols(token: &str) -> [String; 2] {
    [
        PHUX_WS_PROTOCOL.to_owned(),
        format!("{PHUX_WS_BEARER_PREFIX}{token}"),
    ]
}

impl<'a> FallbackToken<'a> {
    const fn valid(self) -> Option<&'a str> {
        match self {
            Self::Valid(token) => Some(token),
            Self::Missing | Self::Invalid => None,
        }
    }
}

pub(super) fn fallback_token_from_webtransport_url(url: &str) -> FallbackToken<'_> {
    let Some(query) = url.split_once('?').map(|(_, rest)| rest) else {
        return FallbackToken::Missing;
    };
    let mut tokens = query
        .split('#')
        .next()
        .unwrap_or_default()
        .split('&')
        .filter(|part| *part == "token" || part.starts_with("token="));
    let Some(carrier) = tokens.next() else {
        return FallbackToken::Missing;
    };
    let Some(token) = carrier.strip_prefix("token=") else {
        return FallbackToken::Invalid;
    };
    if tokens.next().is_some()
        || token.is_empty()
        || token.len() % 2 != 0
        || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return FallbackToken::Invalid;
    }
    FallbackToken::Valid(token)
}
