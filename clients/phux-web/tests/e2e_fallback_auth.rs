//! Browser end-to-end proof that a blackholed WebTransport attempt falls back
//! to token-authenticated WSS without placing the bearer in the WebSocket URL
//! or echoing it as the negotiated subprotocol.

use std::time::Duration;

use gloo_timers::future::sleep;
use js_sys::{Array, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{HtmlCanvasElement, WebSocket};

wasm_bindgen_test_configure!(run_in_browser);

const WSS_URL: &str = match option_env!("PHUX_TEST_WSS_URL") {
    Some(url) => url,
    None => "wss://127.0.0.1:47655/",
};
const WT_URL: &str = match option_env!("PHUX_TEST_WT_URL") {
    Some(url) => url,
    None => "https://127.0.0.1:47656/session?token=00",
};
const TOKEN: &str = match option_env!("PHUX_TEST_TOKEN") {
    Some(token) => token,
    None => "00",
};
const MARKER: &str = "PHUX_WEB_OK";

struct WebSocketCapture;

impl WebSocketCapture {
    fn install() -> Self {
        js_sys::eval(
            r#"
            globalThis.__phuxOriginalWebSocket = globalThis.WebSocket;
            globalThis.__phuxWebSocketCalls = [];
            globalThis.WebSocket = class extends globalThis.__phuxOriginalWebSocket {
              constructor(url, protocols) {
                super(url, protocols);
                globalThis.__phuxWebSocketCalls.push({
                  url: String(url),
                  protocols: Array.from(protocols || []),
                  socket: this,
                });
              }
            };
            "#,
        )
        .expect("install isolated WebSocket capture");
        Self
    }

    fn first_call(&self) -> (String, Vec<String>, WebSocket) {
        let calls = self.calls();
        let call = calls.get(0);
        let url = Reflect::get(&call, &JsValue::from_str("url"))
            .unwrap()
            .as_string()
            .unwrap();
        let protocols: Array = Reflect::get(&call, &JsValue::from_str("protocols"))
            .unwrap()
            .unchecked_into();
        let protocols = protocols
            .iter()
            .filter_map(|value| value.as_string())
            .collect();
        let socket = Reflect::get(&call, &JsValue::from_str("socket"))
            .unwrap()
            .unchecked_into();
        (url, protocols, socket)
    }

    fn calls(&self) -> Array {
        js_sys::eval("globalThis.__phuxWebSocketCalls")
            .expect("read WebSocket capture")
            .unchecked_into()
    }
}

impl Drop for WebSocketCapture {
    fn drop(&mut self) {
        let _ = js_sys::eval(
            r#"
            globalThis.WebSocket = globalThis.__phuxOriginalWebSocket;
            delete globalThis.__phuxOriginalWebSocket;
            delete globalThis.__phuxWebSocketCalls;
            "#,
        );
    }
}

fn canvas(id: &str) -> HtmlCanvasElement {
    let document = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.set_id(id);
    document
        .document_element()
        .unwrap()
        .append_child(&canvas)
        .unwrap();
    canvas
}

async fn wait_for_marker(client: &phux_web::client::Client) -> bool {
    for _ in 0..120 {
        if client.rows_text().iter().any(|row| row.contains(MARKER)) {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

fn immediate_failed_wt_url(token: Option<&str>) -> String {
    token.map_or_else(
        || "ftp://invalid/session".to_owned(),
        |token| format!("ftp://invalid/session?token={token}"),
    )
}

#[wasm_bindgen_test]
async fn blackholed_wt_falls_back_to_authenticated_wss() {
    let capture = WebSocketCapture::install();
    let client = phux_web::client::run_with_fallback(
        WT_URL,
        WSS_URL,
        canvas("authenticated-fallback-canvas"),
        80,
        24,
    )
    .await
    .expect("blackholed WebTransport falls back to authenticated WSS");
    assert!(
        wait_for_marker(&client).await,
        "authenticated fallback never rendered marker"
    );

    let (url, offered, socket) = capture.first_call();
    assert_eq!(
        url, WSS_URL,
        "WSS URL must not be rewritten with credentials"
    );
    assert!(!url.contains(TOKEN), "bearer leaked into WSS URL");
    assert_eq!(
        offered,
        ["phux.v1".to_owned(), format!("phux.bearer.{TOKEN}")],
        "browser offered the authenticated subprotocol carrier"
    );
    assert_eq!(
        socket.protocol(),
        "phux.v1",
        "server echoed a public protocol only"
    );
    assert!(
        !socket.protocol().contains(TOKEN),
        "server echoed the bearer secret"
    );
    drop(client);

    let wrong = format!(
        "{:02x}{}",
        u8::from_str_radix(&TOKEN[..2], 16).unwrap() ^ 0xff,
        &TOKEN[2..]
    );
    assert!(
        phux_web::client::run_with_fallback(
            &immediate_failed_wt_url(Some(&wrong)),
            WSS_URL,
            canvas("wrong-token-canvas"),
            80,
            24,
        )
        .await
        .is_err(),
        "wrong browser token authenticated"
    );
    assert!(
        phux_web::client::run_with_fallback(
            &immediate_failed_wt_url(None),
            WSS_URL,
            canvas("missing-token-canvas"),
            80,
            24,
        )
        .await
        .is_err(),
        "missing browser token authenticated"
    );
    assert_eq!(
        capture.calls().length(),
        3,
        "every scenario reached the WSS constructor"
    );
}
