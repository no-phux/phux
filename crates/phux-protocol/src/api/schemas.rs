//! Field constraints attached to the actual public DTOs. Runtime additionally
//! enforces byte lengths and server facts/authority; schema is not a grant.
use schemars::{Schema, SchemaGenerator};
use serde_json::json;

pub(super) fn retry_key(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"string", "pattern":"^[0-9a-fA-F]{32}$", "not":{"const":"00000000000000000000000000000000"}})
}
pub(super) fn durable_key(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"string", "pattern":"^[a-zA-Z2-7]{25}[aAeEiImMqQuUyY4]$", "not":{"pattern":"^[aA]{26}$"}})
}
pub(super) fn argv(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"array", "maxItems":super::MAX_TERMINAL_ITEMS,
        "prefixItems":[{"type":"string", "minLength":1, "pattern":r"^[^\u0000]*$"}],
        "items":{"type":"string", "pattern":r"^[^\u0000]*$"}})
}
pub(super) fn cwd(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":["string","null"], "pattern":r"^/[^\u0000]*$"})
}
pub(super) fn environment(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"object", "maxProperties":super::MAX_TERMINAL_ITEMS,
        "propertyNames":{"type":"string", "minLength":1, "pattern":r"^[^=\u0000]+$", "not":{"anyOf":[{"pattern":"^PHUX_"},{"const":"TERM"}]}},
        "additionalProperties":{"type":"string", "pattern":r"^[^\u0000]*$"}})
}
pub(super) fn name(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":["string","null"], "minLength":1, "maxLength":super::MAX_TERMINAL_NAME_BYTES, "pattern":r"^[^\u0000]*$"})
}
pub(super) fn ratio(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"number", "exclusiveMinimum":0, "exclusiveMaximum":1})
}

/// Remaining validation is about host facts, UTF-8 byte counts, and authority.
#[must_use]
pub fn semantic_validation() -> serde_json::Value {
    json!([
        "name must contain non-whitespace and fit 256 UTF-8 bytes (schema counts characters)",
        "cwd must exist and be an enterable directory on the executing host at launch",
        "placement entry/resource/instance/activation must still match in the destination session",
        "resource kind, arrangement size/depth and bounded store serialization limits are enforced at commit",
        "authenticated per-dispatch CREATE+ORGANIZE or INVENTORY authority is required; schema grants nothing",
        "idempotency identity must have identical normalized payload while a receipt is retained"
    ])
}
