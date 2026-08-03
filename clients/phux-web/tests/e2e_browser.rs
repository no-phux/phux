//! Full browser end-to-end (headless Chrome) against the live
//! `ws_demo_server`: protocol-0.7 WebSocket negotiation, exact native
//! bootstrap when the WASM codec matches, explicit synthesized fallback, and
//! canvas-visible PTY content. Failures leave a DOM transcript for the browser
//! runner to capture alongside the panic.

use std::time::Duration;

use gloo_timers::future::sleep;
use phux_protocol::caps::{BootstrapProfile, EngineCodec};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::HtmlCanvasElement;

wasm_bindgen_test_configure!(run_in_browser);

const WS_URL: &str = "ws://127.0.0.1:47654/";
const MARKER: &str = "PHUX_WEB_OK";
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 120;

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
    for _ in 0..POLLS {
        if client.rows_text().iter().any(|row| row.contains(MARKER)) {
            return true;
        }
        sleep(POLL).await;
    }
    false
}

fn failure_artifact(name: &str, client: &phux_web::client::Client, detail: &str) -> String {
    let profile = format!("{:?}", client.selected_profile());
    let grid = client.rows_text().join("\n");
    let transcript = format!(
        "scenario={name}\nurl={WS_URL}\nselected_profile={profile}\ndetail={detail}\n--- grid ---\n{grid}"
    );
    let document = web_sys::window().unwrap().document().unwrap();
    let pre = document.create_element("pre").unwrap();
    pre.set_id(&format!("phux-smoke-failure-{name}"));
    pre.set_text_content(Some(&transcript));
    document
        .document_element()
        .unwrap()
        .append_child(&pre)
        .unwrap();
    transcript
}

#[wasm_bindgen_test]
async fn exact_wasm_codec_selects_native_and_renders_live_server() {
    let client = phux_web::client::run(WS_URL, canvas("native-canvas"), 80, 24)
        .await
        .expect("connect to live phux server");

    assert!(
        wait_for_marker(&client).await,
        "{}",
        failure_artifact("native", &client, "seed marker never rendered within 6s")
    );
    assert!(
        matches!(
            client.selected_profile(),
            Some(BootstrapProfile::NativeState {
                codec: EngineCodec::LibghosttyCheckpointV2,
                features,
            }) if features.supports_native()
        ),
        "{}",
        failure_artifact(
            "native",
            &client,
            "exact browser/server engine builds did not select native checkpoint v2"
        )
    );
}

#[wasm_bindgen_test]
async fn synthesized_only_browser_remains_compatible_with_native_server() {
    let client =
        phux_web::client::run_synthesized_compat(WS_URL, canvas("synthesized-canvas"), 80, 24)
            .await
            .expect("connect synthesized compatibility client to live phux server");

    assert!(
        wait_for_marker(&client).await,
        "{}",
        failure_artifact(
            "synthesized",
            &client,
            "seed marker never rendered within 6s"
        )
    );
    assert!(
        matches!(
            client.selected_profile(),
            Some(BootstrapProfile::SynthesizedVtRaw)
        ),
        "{}",
        failure_artifact(
            "synthesized",
            &client,
            "synthesized-only HELLO did not negotiate SynthesizedVtRaw"
        )
    );
}
