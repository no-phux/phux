//! Live-server browser tests' page and polling helpers.
#![allow(dead_code, reason = "each suite uses its own subset")]
use std::time::Duration;

use gloo_timers::future::sleep;
use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;

/// The seeded pane's marker line.
pub const MARKER: &str = "PHUX_WEB_OK";
pub const POLL: Duration = Duration::from_millis(50);
pub const POLLS: usize = 120;

/// A canvas with `id`, appended to the document.
pub fn canvas(id: &str) -> HtmlCanvasElement {
    let document = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.set_id(id);
    let root = document.document_element().unwrap();
    root.append_child(&canvas).unwrap();
    canvas
}

/// Poll `ready` until it holds, for at most `POLLS` polls.
pub async fn poll(mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..POLLS {
        if ready() {
            return true;
        }
        sleep(POLL).await;
    }
    false
}

pub async fn wait_for_marker(client: &phux_web::client::Client) -> bool {
    poll(|| client.rows_text().iter().any(|row| row.contains(MARKER))).await
}
