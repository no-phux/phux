//! `phux_git`: the `phux git` reads (`status`, `diff`, `log`, `branches`,
//! `pr`, `show`) over `CHECKOUT`, in-process through the same client helper
//! the CLI verb calls.

use phux_client::selector::format_terminal_id;
use phux_protocol::checkout::{CheckoutAnswer, CheckoutArea, CheckoutRequest};
use serde_json::{Value, json};

use crate::tools::{
    ToolError, ToolSpec, parse_selector, required_str, resolve_kinded, socket_arg, strict_object,
    transport_error,
};

pub(crate) const SPECS: &[ToolSpec] = &[ToolSpec::in_process("phux_git", schema, |args| {
    Box::pin(call(args))
})];

const ARGS: &[&str] = &[
    "target", "op", "path", "area", "skip", "limit", "sha", "socket",
];

async fn call(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, ARGS, &["target", "op"])?;
    let request = request(args)?;
    let socket = socket_arg(args)?;
    let selector = parse_selector(required_str(args, "target")?)?;
    let target = resolve_kinded(&socket, &selector, false).await?;
    let answer = phux_client::checkout::checkout(&socket, &target.id, request)
        .await
        .map_err(|err| transport_error(&err, &socket, "git"))?;
    let refused = matches!(answer, CheckoutAnswer::Refused(_));
    let document = json!({
        "schema_version": 1,
        "terminal_id": format_terminal_id(&target.id),
        "answer": answer,
    });
    if refused {
        Err(ToolError::new(document.to_string()))
    } else {
        Ok(document)
    }
}

/// The wire request `args` names, validated before anything connects.
fn request(args: &Value) -> Result<CheckoutRequest, ToolError> {
    let text = |key| args.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |key, default: u32| {
        args.get(key).map_or(Ok(default), |value| {
            value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| ToolError::new(format!("`{key}` must be a non-negative integer")))
        })
    };
    let needed = |key| text(key).ok_or_else(|| ToolError::new(format!("`op` needs `{key}`")));
    Ok(match required_str(args, "op")? {
        "status" => CheckoutRequest::Status,
        "diff" => CheckoutRequest::Diff {
            path: needed("path")?,
            area: match text("area").as_deref() {
                None | Some("unstaged") => CheckoutArea::Unstaged,
                Some("staged") => CheckoutArea::Staged,
                Some("untracked") => CheckoutArea::Untracked,
                Some(other) => return Err(ToolError::new(format!("unknown `area` {other}"))),
            },
        },
        "log" => CheckoutRequest::Log {
            skip: number("skip", 0)?,
            limit: number("limit", 20)?,
            path: text("path"),
        },
        "branches" => CheckoutRequest::Branches,
        "pr" => CheckoutRequest::PullRequest,
        "show" => CheckoutRequest::Show {
            sha: needed("sha")?,
        },
        other => return Err(ToolError::new(format!("unknown `op` {other}"))),
    })
}

fn schema() -> Value {
    json!({
        "name": "phux_git",
        "description": "Read the Git checkout a pane is in, resolved by the server from the pane's live working directory (a satellite pane reads that host's checkout). op: status (branch, upstream, changed files with line counts), diff (one file's patch; needs path, area defaults to unstaged), log (newest first; skip, limit, optional path), branches, pr (current branch's pull request and checks via the host's gh), show (one commit; needs sha). A typed refusal (not_a_repository, git_unavailable, no_pull_request, ...) is an error carrying the answer.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "target": { "type": "string", "description": "Target selector: session, session:group, session:group.pane, @paneid, host/@paneid, or `.` for the focused session." },
                "op": { "type": "string", "enum": ["status", "diff", "log", "branches", "pr", "show"] },
                "path": { "type": "string", "description": "Checkout-relative path: the file for diff, an optional filter for log." },
                "area": { "type": "string", "enum": ["unstaged", "staged", "untracked"], "description": "diff only. Default unstaged." },
                "skip": { "type": "integer", "minimum": 0, "description": "log only. Commits to skip from HEAD." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "description": "log only. Default 20." },
                "sha": { "type": "string", "description": "show only. A commit id or a 4-64 hex digit prefix." },
                "socket": { "type": "string" }
            },
            "required": ["target", "op"]
        }
    })
}

/// The `structuredContent` shape: the CLI's `phux git --json` document.
pub(crate) fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "schema_version": { "type": "integer" },
            "terminal_id": { "type": "string" },
            "answer": { "type": "object", "description": "The typed checkout answer, tagged by `kind`." }
        },
        "required": ["answer"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_op_maps_to_its_checkout_request() {
        let diff = |area| CheckoutRequest::Diff {
            path: "a.rs".into(),
            area,
        };
        for (args, want) in [
            (json!({"op": "status"}), CheckoutRequest::Status),
            (
                json!({"op": "diff", "path": "a.rs"}),
                diff(CheckoutArea::Unstaged),
            ),
            (
                json!({"op": "diff", "path": "a.rs", "area": "staged"}),
                diff(CheckoutArea::Staged),
            ),
            (
                json!({"op": "log"}),
                CheckoutRequest::Log {
                    skip: 0,
                    limit: 20,
                    path: None,
                },
            ),
            (
                json!({"op": "log", "skip": 2, "limit": 5, "path": "src"}),
                CheckoutRequest::Log {
                    skip: 2,
                    limit: 5,
                    path: Some("src".into()),
                },
            ),
            (json!({"op": "branches"}), CheckoutRequest::Branches),
            (json!({"op": "pr"}), CheckoutRequest::PullRequest),
            (
                json!({"op": "show", "sha": "abcd"}),
                CheckoutRequest::Show { sha: "abcd".into() },
            ),
        ] {
            assert_eq!(request(&args).unwrap(), want, "{args}");
        }
        for bad in [
            json!({"op": "stage"}),
            json!({"op": "diff"}),
            json!({"op": "diff", "path": "a", "area": "conflicted"}),
            json!({"op": "show"}),
            json!({"op": "log", "limit": -1}),
        ] {
            assert!(request(&bad).is_err(), "{bad}");
        }
    }
}
