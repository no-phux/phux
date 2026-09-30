//! phux-web binary entry (Trunk builds this).
//!
//! On load: read the server URL from `?ws=…` (defaulting to
//! `ws://<host>/session`) and attach the terminal into `#phux-term`. A
//! `?wt=…` parameter (an `https://` WebTransport session URL, from
//! `phux server --webtransport`) makes the client try WebTransport first,
//! falling back to the WebSocket URL. Either way the page reconnects after
//! the server restarts or the network drops.

use wasm_bindgen::{JsCast, JsValue};

fn main() {
    wasm_bindgen_futures::spawn_local(async {
        if let Err(err) = auto_start().await {
            web_sys::console::error_1(&err);
        }
    });
}

async fn auto_start() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let location = window.location();
    let search = location.search().unwrap_or_default();
    let ws_url = url_from_query(&search, "ws=")
        .unwrap_or_else(|| format!("ws://{}/session", location.host().unwrap_or_default()));
    match url_from_query(&search, "wt=") {
        Some(wt_url) => {
            phux_web::start_webtransport(wt_url, ws_url, "phux-term".to_owned(), 80, 24).await
        }
        None => {
            let canvas = window
                .document()
                .and_then(|document| document.get_element_by_id("phux-term"))
                .ok_or_else(|| JsValue::from_str("canvas element not found"))?
                .dyn_into()?;
            let client = phux_web::client::run(&ws_url, canvas, 80, 24).await?;
            client.enable_auto_reconnect(None, &ws_url);
            Ok(())
        }
    }
}

/// Extract a URL-valued query parameter, decoding the `:`, `/`, `?`, and `=`
/// a browser escapes (enough for the `ws=`/`wt=` values, including a
/// `?token=<hex>` suffix on a WebTransport URL).
fn url_from_query(search: &str, key: &str) -> Option<String> {
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|kv| kv.strip_prefix(key))
        .map(|v| {
            v.replace("%3A", ":")
                .replace("%3a", ":")
                .replace("%2F", "/")
                .replace("%2f", "/")
                .replace("%3F", "?")
                .replace("%3f", "?")
                .replace("%3D", "=")
                .replace("%3d", "=")
        })
}
