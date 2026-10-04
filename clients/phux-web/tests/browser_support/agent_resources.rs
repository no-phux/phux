//! The loopback fixture drives the disposable server through its local-owner
//! Unix socket. Browser peers intentionally cannot forge producer records.

use std::time::Duration;

use gloo_timers::future::sleep;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

pub async fn action(action: &str) {
    let Some(url) = option_env!("PHUX_TEST_AGENT_URL") else {
        panic!("run through scripts/ci/web-browser.py for the local owner fixture");
    };
    let options = web_sys::RequestInit::new();
    options.set_method("POST");
    let response: web_sys::Response = JsFuture::from(
        web_sys::window()
            .unwrap()
            .fetch_with_str_and_init(&format!("{url}/{action}"), &options),
    )
    .await
    .expect("fixture request")
    .dyn_into()
    .expect("HTTP response");
    let status = response.status();
    let body = JsFuture::from(response.text().unwrap()).await.unwrap();
    assert_eq!(status, 200, "fixture {action}: {body:?}");
}

pub async fn wait_badge(state: Option<&str>) {
    let document = web_sys::window().unwrap().document().unwrap();
    for _ in 0..120 {
        let badge = document
            .query_selector("#phux-agent-badges .phux-agent-badge")
            .unwrap();
        match (state, badge) {
            (None, None) => return,
            (Some(expected), Some(badge))
                if badge.get_attribute("data-state").as_deref() == Some(expected) =>
            {
                assert_eq!(badge.get_attribute("data-provider").as_deref(), Some("pi"));
                return;
            }
            _ => sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("badge never reached {state:?}");
}
