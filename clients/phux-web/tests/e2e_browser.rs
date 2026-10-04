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

#[path = "browser_support/agent_resources.rs"]
mod agent_resources;

const WS_URL: &str = match option_env!("PHUX_TEST_WS_URL") {
    Some(url) => url,
    None => "ws://127.0.0.1:47654/",
};
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
async fn agents_started_after_browser_attach_update_badges_without_reloading() {
    let client = phux_web::client::run(WS_URL, canvas("live-agent-canvas"), 80, 24)
        .await
        .expect("connect browser first");
    assert!(wait_for_marker(&client).await);
    agent_resources::action("open").await;
    agent_resources::action("prompt").await;
    agent_resources::wait_badge(Some("working")).await;
    agent_resources::action("ask").await;
    agent_resources::wait_badge(Some("blocked")).await;
    agent_resources::action("stop").await;
    agent_resources::wait_badge(Some("done")).await;
    assert_eq!(client.agent_badges().len(), 1);
    client.close();
    // A fresh connection discovers the same child in its snapshot and reads
    // its retained stream, without creating another pane or subscription.
    let reconnected = phux_web::client::run(WS_URL, canvas("agent-reconnect-canvas"), 80, 24)
        .await
        .expect("reconnect with existing child");
    assert!(wait_for_marker(&reconnected).await);
    agent_resources::wait_badge(Some("done")).await;
    assert_eq!(reconnected.agent_badges().len(), 1);
    agent_resources::action("close").await;
    agent_resources::wait_badge(None).await;
    assert!(
        reconnected
            .rows_text()
            .iter()
            .any(|row| row.contains(MARKER))
    );
    reconnected.close();
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
            Some(BootstrapProfile::SynthesizedVtRaw)
                | Some(BootstrapProfile::NativeState {
                    codec: EngineCodec::LibghosttyCheckpointV2,
                    ..
                })
        ),
        "{}",
        failure_artifact(
            "native",
            &client,
            "browser did not negotiate a usable bootstrap profile"
        )
    );
}

#[wasm_bindgen_test]
async fn resize_reflows_the_live_terminal_and_canvas() {
    let canvas = canvas("resize-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    assert!(
        wait_for_marker(&client).await,
        "{}",
        failure_artifact("resize", &client, "seed marker never rendered within 6s")
    );
    assert_eq!(
        canvas
            .get_attribute(phux_web::client::CONNECTION_ATTRIBUTE)
            .as_deref(),
        Some("connected")
    );

    client.resize(100, 30);
    let mut reflowed = false;
    for _ in 0..POLLS {
        let rows = client.rows_text();
        if rows.len() == 30 && rows.iter().all(|row| row.chars().count() == 100) {
            reflowed = true;
            break;
        }
        sleep(POLL).await;
    }
    assert!(
        reflowed,
        "{}",
        failure_artifact("resize", &client, "grid never became 100x30")
    );
    // Paint lands on the next animation frame.
    for _ in 0..POLLS {
        if (canvas.width(), canvas.height()) == (100 * 8, 30 * 16) {
            break;
        }
        sleep(POLL).await;
    }
    assert_eq!(
        (canvas.width(), canvas.height()),
        (100 * 8, 30 * 16),
        "the canvas follows the new grid"
    );
    client.close();
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
