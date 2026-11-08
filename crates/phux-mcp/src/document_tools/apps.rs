//! The application tools (`docs/resource-kinds.md#applications`):
//! `phux_app_open` spawns a pane running `phux agent app run`, as
//! `phux spawn --app` does, and returns the pane with the documents its
//! helper opened; `phux_app_close` kills that pane, which quits the app.

use std::time::Duration;

use phux_client::app::{APP_DOCUMENTS, AppSpec, documents_under, helper_argv, pane_refusal};
use phux_client::selector::format_terminal_id;
use phux_protocol::ids::ResourceId;
use serde_json::{Value, json};

use crate::cli_adapter::bounded_string;
use crate::cli_tools::{optional_bool, schema, string_schema};
use crate::tools::{ToolError, socket_arg, strict_object, transport_error};

/// How long `phux_app_open` waits for the app's documents.
const DOCUMENTS_WAIT: Duration = Duration::from_secs(20);

pub(crate) fn schemas() -> Vec<Value> {
    vec![
        schema(
            "phux_app_open",
            "Launch an application as resources: a new pane runs `phux agent app run`, which launches the app, mirrors it into documents under the pane (a browser page with control `page`; else a window surface with control `window` and the app's accessibility tree), and quits the app when the pane is killed. `name` is a macOS app name, a Linux command or `.desktop` id, or with `browser` the browser (`chrome`, `chromium`, or a path), opened on `url` (default about:blank) with a fresh profile; `headless` keeps the browser off screen. Returns the pane and the documents opened within 20 s; read them with phux_tree_snapshot or phux_surface_snapshot, and close with phux_app_close. Input to the documents needs the owner socket or an `input@CLASS` grant. On macOS a non-browser app is refused, because a pane holds no Screen Recording or Accessibility grant: ask the person to run `phux agent app run NAME --pane @N` in a terminal that has both grants.",
            json!({
                "name": string_schema(),
                "browser": { "type": "boolean" },
                "url": string_schema(),
                "headless": { "type": "boolean" },
                "target": string_schema(),
                "socket": string_schema(),
            }),
            &["name"],
        ),
        schema(
            "phux_app_close",
            "Close an application phux_app_open launched: kill its pane, which quits the app (only the instance its helper launched) and closes its documents. Requires confirm=true.",
            json!({
                "pane": string_schema(),
                "confirm": { "type": "boolean", "const": true, "description": "Required explicit destructive-operation confirmation." },
                "socket": string_schema(),
            }),
            &["pane", "confirm"],
        ),
    ]
}

pub(crate) fn owns(name: &str) -> bool {
    matches!(name, "phux_app_open" | "phux_app_close")
}

pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "phux_app_open" => open(args).await,
        "phux_app_close" => {
            strict_object(args, &["pane", "confirm", "socket"], &["pane", "confirm"])?;
            let mut kill = json!({ "target": args["pane"], "confirm": true });
            if let Some(socket) = args.get("socket") {
                kill["socket"] = socket.clone();
            }
            crate::tools::phux_kill(&kill).await
        }
        other => Err(ToolError::new(format!("unknown app tool: {other}"))),
    }
}

async fn open(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["name", "browser", "url", "headless", "target", "socket"],
        &["name"],
    )?;
    let name = bounded_string(args, "name", true)?.unwrap_or_default();
    let url = bounded_string(args, "url", false)?;
    let browser = optional_bool(args, "browser")?.unwrap_or(false) || url.is_some();
    let headless = optional_bool(args, "headless")?.unwrap_or(false);
    if headless && !browser {
        return Err(ToolError::new("`headless` needs `browser`"));
    }
    let spec = AppSpec {
        name: &name,
        browser: browser.then(|| url.as_deref().unwrap_or("about:blank")),
        headless,
    };
    if let Some(refusal) = pane_refusal(&spec) {
        return Err(ToolError::new(refusal));
    }
    let program = crate::cli_adapter::phux_program();
    let helper = helper_argv(&program.to_string_lossy(), spec);
    let mut spawn = json!({ "command": helper });
    for key in ["target", "socket"] {
        if let Some(value) = args.get(key) {
            spawn[key] = value.clone();
        }
    }
    let mut document = crate::pane_tools::spawn(&spawn).await?;
    let id = document["terminal_id"]
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| ToolError::new("the spawn returned no local pane"))?;
    let pane = ResourceId::local(id);
    let socket = socket_arg(args)?;
    let documents = documents_under(&socket, &pane, APP_DOCUMENTS, DOCUMENTS_WAIT)
        .await
        .map_err(|err| transport_error(&err, &socket, "app open"))?
        .ok_or_else(|| {
            ToolError::new(
                "the app pane exited before it opened any document; run `phux agent app run NAME` inside a pane to see why",
            )
        })?;
    document["pane"] = json!(format_terminal_id(&pane));
    document["documents"] = Value::Array(documents);
    Ok(document)
}
