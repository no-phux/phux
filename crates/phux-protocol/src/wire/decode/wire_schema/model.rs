//! The schema file's model and its line-per-field renderer.

use std::fmt::Write as _;

use super::{BTreeMap, Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Schema {
    pub(super) description: Vec<String>,
    pub(super) types: BTreeMap<String, String>,
    pub(super) frames: Vec<FrameEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FrameEntry {
    pub(super) name: String,
    pub(super) type_byte: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) retired: Vec<u32>,
    pub(super) fields: Vec<FieldEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FieldEntry {
    pub(super) id: u32,
    pub(super) name: String,
    #[serde(rename = "type")]
    pub(super) ty: String,
    pub(super) presence: Presence,
}

/// Whether a decoder accepts the frame without the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Presence {
    /// Present in every encoding; every decode without it fails.
    Required,
    /// May be absent: some encoding omits it, or a decode without it passes.
    Optional,
}

/// Compact JSON for one leaf of the rendered schema.
pub(super) fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

/// Render `schema` with one field per line, so a field change is a one-line
/// diff. Leaves go through `serde_json` for escaping.
pub(super) fn render(schema: &Schema) -> String {
    let mut out = String::from("{\n  \"description\": [\n");
    push_lines(
        &mut out,
        schema
            .description
            .iter()
            .map(|line| format!("    {}", json(line))),
    );
    out.push_str("  ],\n  \"types\": {\n");
    push_lines(
        &mut out,
        schema
            .types
            .iter()
            .map(|(name, doc)| format!("    {}: {}", json(name), json(doc))),
    );
    out.push_str("  },\n  \"frames\": [\n");
    push_lines(&mut out, schema.frames.iter().map(render_frame));
    out.push_str("  ]\n}\n");
    out
}

pub(super) fn render_frame(frame: &FrameEntry) -> String {
    let mut out = String::from("    {\n");
    let _ = writeln!(out, "      \"name\": {},", json(&frame.name));
    let _ = writeln!(out, "      \"type_byte\": {},", frame.type_byte);
    if !frame.retired.is_empty() {
        let _ = writeln!(out, "      \"retired\": {},", json(&frame.retired));
    }
    if frame.fields.is_empty() {
        out.push_str("      \"fields\": []\n    }");
        return out;
    }
    out.push_str("      \"fields\": [\n");
    push_lines(
        &mut out,
        frame
            .fields
            .iter()
            .map(|field| format!("        {}", json(field))),
    );
    out.push_str("      ]\n    }");
    out
}

/// Append `lines` joined by `,\n`, ending with a newline.
pub(super) fn push_lines(out: &mut String, lines: impl Iterator<Item = String>) {
    let lines: Vec<String> = lines.collect();
    out.push_str(&lines.join(",\n"));
    if !lines.is_empty() {
        out.push('\n');
    }
}
