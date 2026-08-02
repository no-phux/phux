//! JSON-RPC 2.0 envelope types for the MCP stdio transport.
//!
//! Hand-rolled over `serde_json` (no framework dep, per ADR-0022 §5). The
//! MCP stdio transport is newline-delimited JSON: one JSON value per line
//! on stdin/stdout. A *request* carries an `id` and expects a response; a
//! *notification* omits `id` and gets none.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Standard JSON-RPC error code: the payload was not valid JSON.
pub(crate) const PARSE_ERROR: i64 = -32700;
/// Standard JSON-RPC error code: the JSON was not a valid Request object.
pub(crate) const INVALID_REQUEST: i64 = -32600;
/// Standard JSON-RPC error code: the method does not exist.
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
/// Standard JSON-RPC error code: a server-internal failure.
pub(crate) const INTERNAL_ERROR: i64 = -32603;
/// MCP/Language Server Protocol error code for a cancelled request.
pub(crate) const REQUEST_CANCELLED: i64 = -32800;

/// An incoming JSON-RPC message (request or notification).
///
/// `id` is absent for notifications. `params` is optional and method-shaped;
/// we deserialize it per method rather than into a fixed type so unknown
/// fields are tolerated.
///
/// The `jsonrpc` protocol marker is deliberately not modeled: serde ignores
/// unknown keys by default (no `deny_unknown_fields`), and a missing/odd
/// `jsonrpc` is treated leniently to keep the loop robust.
#[derive(Debug, Deserialize)]
pub(crate) struct Request {
    /// Request id. Absent ⇒ notification (no reply is sent).
    #[serde(default)]
    pub(crate) id: Option<Value>,
    /// The method name (e.g. `"tools/call"`).
    pub(crate) method: String,
    /// Method parameters, if any.
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

/// A JSON-RPC success response.
#[derive(Debug, Serialize)]
pub(crate) struct SuccessResponse {
    /// Always `"2.0"`.
    pub(crate) jsonrpc: &'static str,
    /// Echoes the request id.
    pub(crate) id: Value,
    /// The method's result payload.
    pub(crate) result: Value,
}

/// A JSON-RPC error response.
#[derive(Debug, Serialize)]
pub(crate) struct ErrorResponse {
    /// Always `"2.0"`.
    pub(crate) jsonrpc: &'static str,
    /// Echoes the request id (or `null` when the id could not be parsed).
    pub(crate) id: Value,
    /// The structured error.
    pub(crate) error: ErrorObject,
}

/// The `error` member of an [`ErrorResponse`].
#[derive(Debug, Serialize)]
pub(crate) struct ErrorObject {
    /// One of the `*_ERROR` / `*_FOUND` / `*_REQUEST` codes above.
    pub(crate) code: i64,
    /// Human-readable, single-sentence diagnostic.
    pub(crate) message: String,
}

/// Build a success response value for `id` carrying `result`.
#[must_use]
pub(crate) fn success(id: Value, result: Value) -> Value {
    serde_json::to_value(SuccessResponse {
        jsonrpc: "2.0",
        id,
        result,
    })
    .unwrap_or(Value::Null)
}

/// Build an error response value for `id` with `code`/`message`.
#[must_use]
pub(crate) fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    serde_json::to_value(ErrorResponse {
        jsonrpc: "2.0",
        id,
        error: ErrorObject {
            code,
            message: message.into(),
        },
    })
    .unwrap_or(Value::Null)
}
