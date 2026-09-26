//! Route GPUI focus, keys, and pointer events into the shared input adapter.
//! Painting supplies geometry; this module does not acquire frames or drain events.

use super::paint::Observation;
use crate::input::{InputMetrics, KeyDisposition, TerminalInput};
use gpuix_native::native_extensions::gpui;
use phux_client_runtime::publication::CursorWidth;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn present(
    input: &gpui::Entity<TerminalInput>,
    report: &Observation,
    rebind: &AtomicBool,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let Some(frame) = report.frame.clone() else {
        return;
    };
    if report.error.is_some() {
        return;
    }
    let width = match frame.cursor.width {
        CursorWidth::Narrow => 1,
        CursorWidth::Wide | CursorWidth::WideTail => 2,
    };
    let metrics = InputMetrics {
        bounds: report.geometry.bounds,
        cell_width: report.geometry.cell_width,
        line_height: report.geometry.cell_height,
        cursor_bounds: report
            .geometry
            .cell_bounds(frame.cursor.row, frame.cursor.col, width),
    };
    let accepted = input.update(cx, |state, _| state.presented(&frame, metrics).is_ok());
    if !accepted {
        input.update(cx, |state, _| state.cancel());
        rebind.store(true, Ordering::Release);
        return;
    }
    let focus = input.read(cx).focus_handle().clone();
    window.handle_input(
        &focus,
        gpui::ElementInputHandler::new(report.geometry.bounds, input.clone()),
        cx,
    );
}

pub(super) fn on_key_down(
    input: &gpui::Entity<TerminalInput>,
    event: &gpui::KeyDownEvent,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    if shortcut(event, "c") {
        input.update(cx, |state, cx| {
            let _ = state.copy_selection(window, cx);
        });
        cx.stop_propagation();
        return;
    }
    if shortcut(event, "v") {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        input.update(cx, |state, cx| {
            let _ = state.paste_text(&text, window, cx);
        });
        cx.stop_propagation();
        return;
    }
    let result = input.update(cx, |state, cx| state.key_down(event, window, cx));
    if result != Ok(KeyDisposition::Platform) {
        cx.stop_propagation();
    }
}

pub(super) fn on_key_up(
    input: &gpui::Entity<TerminalInput>,
    event: &gpui::KeyUpEvent,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let result = input.update(cx, |state, cx| state.key_up(event, window, cx));
    if result != Ok(KeyDisposition::Platform) {
        cx.stop_propagation();
    }
}

fn shortcut(event: &gpui::KeyDownEvent, key: &str) -> bool {
    event.keystroke.modifiers.platform
        && !event.keystroke.modifiers.shift
        && event.keystroke.key == key
}

pub(super) fn paint_preedit(
    input: &gpui::Entity<TerminalInput>,
    origin: gpui::Point<gpui::Pixels>,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let (text, _) = input.read(cx).preedit();
    if text.is_empty() {
        return;
    }
    let line = window.text_system().shape_line(
        text.to_owned().into(),
        gpui::px(14.),
        &[gpui::TextRun {
            len: text.len(),
            font: gpui::font("Menlo"),
            color: gpui::rgb(0xffe08a).into(),
            ..Default::default()
        }],
        None,
    );
    let _ = line.paint(
        origin,
        gpui::px(14.),
        gpui::TextAlign::Left,
        None,
        window,
        cx,
    );
}

pub(super) fn note_rebind(flag: &Arc<AtomicBool>) -> bool {
    flag.swap(false, Ordering::AcqRel)
}
