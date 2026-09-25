//! Opt-in acceptance instrumentation, absent from production builds.

use std::sync::{Arc, Mutex, Weak};

use napi_derive::napi;
use serde_json::{Value, json};

use super::{paint::Observation, parse_view};
use phux_client_runtime::{Client, ViewId, engine::Scroll};

type PaintRecords = Vec<(u64, Weak<Mutex<Observation>>)>;
static RECORDS: Mutex<PaintRecords> = Mutex::new(Vec::new());

pub(super) fn register(id: u64, record: &Arc<Mutex<Observation>>) {
    let mut records = RECORDS.lock().unwrap_or_else(|error| error.into_inner());
    records.retain(|(_, record)| record.strong_count() != 0);
    records.push((id, Arc::downgrade(record)));
}

/// The test-only surface reads records written by successful native paint calls.
/// No runtime grid query masquerades as painted text, and no strong global
/// reference retains a view or its callbacks after custom-element teardown.
#[napi]
pub fn terminal_fixture_paints() -> Value {
    let mut records = RECORDS.lock().unwrap_or_else(|error| error.into_inner());
    records.retain(|(_, record)| record.strong_count() != 0);
    Value::Array(
        records
            .iter()
            .filter_map(|(id, record)| {
                let record = record.upgrade()?;
                let report = record.lock().unwrap_or_else(|error| error.into_inner());
                Some(report_json(*id, &report))
            })
            .collect(),
    )
}

fn report_json(id: u64, report: &Observation) -> Value {
    let geometry = report.geometry;
    let glyphs: Vec<_> = report.glyphs.iter().map(|glyph| {
        let text = report.frame.as_ref().map(|frame| String::from_utf8_lossy(frame.cell_text(glyph.row, glyph.col)).into_owned());
        let cell = report.frame.as_ref().and_then(|frame| frame.cell(glyph.row, glyph.col));
        json!({
            "row": glyph.row, "col": glyph.col, "text": text,
            "x": f32::from(glyph.bounds.origin.x), "y": f32::from(glyph.bounds.origin.y),
            "paintX": f32::from(glyph.origin.x), "paintY": f32::from(glyph.origin.y),
            "baseline": f32::from(glyph.baseline),
            "width": f32::from(glyph.bounds.size.width), "height": f32::from(glyph.bounds.size.height),
            "foreground": rgba(glyph.foreground), "flags": cell.map(|cell| cell.flags),
            "hyperlink": hyperlink(report, glyph.row, glyph.col),
        })
    }).collect();
    json!({
        "id": id, "error": report.error, "glyphs": glyphs,
        "viewId": report.view_id.map(|view| view.get().to_string()),
        "generation": report.frame.as_ref().map(|frame| frame.generation.to_string()),
        "bootstrapId": report.frame.as_ref().map(|frame| frame.bootstrap_id.to_string()),
        "cols": geometry.cols, "rows": geometry.rows,
        "cellWidth": f32::from(geometry.cell_width), "cellHeight": f32::from(geometry.cell_height),
        "baselineOffset": f32::from(geometry.baseline),
        "scale": geometry.scale,
        "prepareMicros": report.prepare_micros, "paintMicros": report.paint_micros,
        "scrollOffset": report.frame.as_ref().map(|frame| frame.scrollbar.offset.to_string()),
    })
}

fn rgba(color: super::gpui::Hsla) -> [f32; 4] {
    let color = super::gpui::Rgba::from(color);
    [color.r, color.g, color.b, color.a]
}

fn hyperlink(report: &Observation, row: u16, col: u16) -> Option<String> {
    let frame = report.frame.as_ref()?;
    let cell = frame.cell(row, col)?;
    if cell.hyperlink_len == 0 {
        return None;
    }
    let start = cell.hyperlink_offset as usize;
    let end = start.checked_add(cell.hyperlink_len as usize)?;
    Some(String::from_utf8_lossy(frame.buffer.utf8.get(start..end)?).into_owned())
}

fn client(handle: &str) -> napi::Result<Client> {
    phux_client_ffi::napi::initialize()
        .client(handle)
        .map_err(|error| napi::Error::from_reason(error.to_string()))
}

fn view(value: &str) -> napi::Result<ViewId> {
    parse_view(value).ok_or_else(|| napi::Error::from_reason("invalid view"))
}

#[napi]
pub fn terminal_fixture_create_view(handle: String, terminal: String) -> napi::Result<String> {
    let terminal = phux_client_ffi::projection::id::parse(&terminal)
        .ok_or_else(|| napi::Error::from_reason("invalid terminal"))?;
    client(&handle)?
        .create_view(&terminal)
        .map(|view| view.get().to_string())
        .map_err(engine_error)
}

#[napi]
pub fn terminal_fixture_destroy_view(handle: String, view_id: String) -> napi::Result<()> {
    client(&handle)?
        .destroy_view(view(&view_id)?)
        .map_err(engine_error)
}

#[napi]
pub fn terminal_fixture_scroll(handle: String, view_id: String, rows: i32) -> napi::Result<()> {
    client(&handle)?
        .scroll_view(view(&view_id)?, Scroll::Delta(i64::from(rows)))
        .map_err(engine_error)
}

#[napi]
pub fn terminal_fixture_select(handle: String, view_id: String, query: String) -> napi::Result<()> {
    let client = client(&handle)?;
    let view = view(&view_id)?;
    let engine = client
        .engine()
        .ok_or_else(|| napi::Error::from_reason("stopped"))?;
    let matches = engine
        .search_view(view, query, true)
        .map_err(engine_error)?;
    let found = matches
        .first()
        .ok_or_else(|| napi::Error::from_reason("no match"))?;
    engine
        .set_view_selection(view, found.start, found.end, false)
        .map_err(engine_error)
}

fn engine_error(error: phux_client_runtime::engine::EngineError) -> napi::Error {
    napi::Error::from_reason(error.to_string())
}

#[napi]
pub fn terminal_fixture_generation(
    handle: String,
    view_id: String,
) -> napi::Result<Option<String>> {
    Ok(client(&handle)?
        .acquire_view(view(&view_id)?)
        .map(|frame| frame.generation.to_string()))
}

#[napi]
pub fn terminal_fixture_clear_selection(handle: String, view_id: String) -> napi::Result<()> {
    client(&handle)?
        .engine()
        .ok_or_else(|| napi::Error::from_reason("stopped"))?
        .clear_view_selection(view(&view_id)?)
        .map_err(engine_error)
}
