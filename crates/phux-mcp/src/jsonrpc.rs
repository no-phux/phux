//! JSON-RPC 2.0 envelopes for the MCP stdio transport (newline-delimited
//! JSON). A request carries an `id` and gets a response; a notification
//! omits it and gets none.

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The payload was not valid JSON.
pub(crate) const PARSE_ERROR: i64 = -32700;
/// The JSON was not a valid Request object.
pub(crate) const INVALID_REQUEST: i64 = -32600;
/// The method does not exist.
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
/// A server-internal failure.
pub(crate) const INTERNAL_ERROR: i64 = -32603;
/// MCP/LSP: the request was cancelled.
pub(crate) const REQUEST_CANCELLED: i64 = -32800;

/// An incoming request or notification. The `jsonrpc` marker is not
/// modeled, and unknown keys are tolerated, to keep the loop lenient.
#[derive(Debug, Deserialize)]
pub(crate) struct Request {
    /// Absent for a notification.
    #[serde(default)]
    pub(crate) id: Option<Value>,
    pub(crate) method: String,
    #[serde(default)]
    pub(crate) params: Option<Value>,
}

impl Request {
    /// Whether this message is a notification (no `id`, so no reply).
    #[must_use]
    pub(crate) const fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A success response for `id` carrying `result`.
#[must_use]
pub(crate) fn success(id: Value, result: Value) -> Value {
    envelope(id, "result", result)
}

/// An error response for `id` with `code`/`message`.
#[must_use]
pub(crate) fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    envelope(
        id,
        "error",
        json!({ "code": code, "message": message.into() }),
    )
}

fn envelope(id: Value, key: &str, body: Value) -> Value {
    let mut object = Map::new();
    object.insert("jsonrpc".to_owned(), Value::from("2.0"));
    object.insert("id".to_owned(), id);
    object.insert(key.to_owned(), body);
    Value::Object(object)
}
