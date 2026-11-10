//! Applications as resources (`docs/resource-kinds.md#applications`).
//!
//! The argv of the pane that runs `phux agent app run`, and the documents it
//! opens under that pane: one builder for `phux spawn --app` and the MCP
//! `phux_app_open` tool.

use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::ResourceInfo;
use serde_json::{Value, json};

use crate::attach::AttachError;
use crate::selector::format_terminal_id;

/// What one application helper launches.
#[derive(Debug, Clone, Copy)]
pub struct AppSpec<'a> {
    /// The application, or with `browser` the browser.
    pub name: &'a str,
    /// `Some(url)` launches a browser with a debugging port and opens `url`.
    pub browser: Option<&'a str>,
    /// Run the browser with no window.
    pub headless: bool,
}

/// How many documents a helper opens: a tree and a surface.
pub const APP_DOCUMENTS: usize = 2;

/// The argv of a pane that runs `spec` with the `phux` binary `program`.
#[must_use]
pub fn helper_argv(program: &str, spec: AppSpec<'_>) -> Vec<String> {
    let mut argv = vec![
        program.to_owned(),
        "agent".into(),
        "app".into(),
        "run".into(),
        spec.name.to_owned(),
    ];
    if let Some(url) = spec.browser {
        argv.push(format!("--browser={url}"));
        if spec.headless {
            argv.push("--headless".into());
        }
    }
    argv
}

/// Why a pane cannot run `spec` usefully, with the remedy.
///
/// On macOS a pane's processes belong to the server's launchd job, which holds no Screen
/// Recording or Accessibility grant, so a non-browser app's window and tree
/// mirrors would fail. `None` for a browser, or off macOS.
#[must_use]
pub fn pane_refusal(spec: &AppSpec<'_>) -> Option<String> {
    (cfg!(target_os = "macos") && spec.browser.is_none()).then(|| {
        format!(
            "a phux pane cannot capture the macOS app {}: its processes hold no Screen Recording or Accessibility grant. Run `phux agent app run {} --pane @N` in a terminal that has both grants",
            spec.name, spec.name
        )
    })
}

/// One document as `phux_documents` lists it.
#[must_use]
pub fn document_entry(info: &ResourceInfo) -> Value {
    json!({
        "id": format_terminal_id(&info.id),
        "kind": crate::resource::kind_name(info.kind),
        "parent": info.parent.as_ref().map(format_terminal_id),
        "name": info.title,
        "producer": crate::resource::producer_name(info),
        "control": info.control.map(phux_protocol::wire::frame::ControlClass::name),
    })
}

/// Whether `info` is a document resource: a tree, a surface or a typed document.
#[must_use]
pub const fn is_document(info: &ResourceInfo) -> bool {
    info.kind.is_document()
}

/// The documents under `pane`, once `want` of them exist or `timeout`
/// passes. `None` when the pane is gone: its helper exited.
///
/// # Errors
///
/// A transport error from `GET_STATE`.
pub async fn documents_under(
    socket: &Path,
    pane: &ResourceId,
    want: usize,
    timeout: Duration,
) -> Result<Option<Vec<Value>>, AttachError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let view = crate::state::get_state(socket).await?;
        let resources = &view.snapshot().resources;
        if !resources.iter().any(|info| info.id == *pane) {
            return Ok(None);
        }
        let documents: Vec<Value> = resources
            .iter()
            .filter(|info| is_document(info) && info.parent.as_ref() == Some(pane))
            .map(document_entry)
            .collect();
        if documents.len() >= want || tokio::time::Instant::now() >= deadline {
            return Ok(Some(documents));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_macos_app_is_refused_a_pane() {
        let app = AppSpec {
            name: "TextEdit",
            browser: None,
            headless: false,
        };
        let refusal = pane_refusal(&app);
        assert_eq!(refusal.is_some(), cfg!(target_os = "macos"));
        if let Some(refusal) = refusal {
            assert!(
                refusal.contains("phux agent app run TextEdit --pane @N"),
                "{refusal}"
            );
        }
        let browser = AppSpec {
            browser: Some("about:blank"),
            ..app
        };
        assert_eq!(pane_refusal(&browser), None);
    }

    #[test]
    fn the_helper_argv_names_the_app_and_only_browser_flags_with_a_browser() {
        let app = AppSpec {
            name: "TextEdit",
            browser: None,
            headless: true,
        };
        assert_eq!(
            helper_argv("phux", app),
            ["phux", "agent", "app", "run", "TextEdit"]
        );
        let browser = AppSpec {
            name: "chrome",
            browser: Some("data:text/html,<b>x</b>"),
            headless: true,
        };
        assert_eq!(
            helper_argv("/bin/phux", browser),
            [
                "/bin/phux",
                "agent",
                "app",
                "run",
                "chrome",
                "--browser=data:text/html,<b>x</b>",
                "--headless"
            ]
        );
    }
}
