//! Route GPUI focus, keys, and pointer events into the shared input adapter.
//! Painting supplies geometry; this module does not acquire frames or drain events.

use super::paint::{self, Observation};
use crate::input::{InputMetrics, KeyDisposition, TerminalInput};
use gpui;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn present(
    input: &gpui::Entity<TerminalInput>,
    report: &Observation,
    size_owner: bool,
    rebind: &AtomicBool,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> bool {
    let (Some(frame), Some(epoch)) = (report.frame.clone(), report.epoch) else {
        return false;
    };
    if report.error.is_some() {
        return false;
    }
    let _timed = crate::perf::PRESENT.timer();
    let (row, col, width) = paint::cursor_span(&frame);
    let metrics = InputMetrics {
        bounds: report.geometry.bounds,
        cell_width: report.geometry.cell_width,
        line_height: report.geometry.cell_height,
        scale: report.geometry.scale,
        cursor_bounds: paint::shifted(report.geometry.cell_bounds(row, col, width), report.shift),
        shift: report.shift,
    };
    let accepted = input.update(cx, |state, _| {
        state.presented_at(epoch, &frame, metrics).is_ok()
    });
    if !accepted {
        input.update(cx, |state, _| state.cancel());
        rebind.store(true, Ordering::Release);
        return true;
    }
    input.update(cx, |state, cx| state.fit(size_owner, window, cx));
    let focus = input.read(cx).focus_handle().clone();
    window.handle_input(
        &focus,
        // Terminal keys repeat instead of opening macOS's press-and-hold accent
        // picker. GPUI still sends marked text through the native IME path.
        gpui::ElementInputHandler::new(report.geometry.bounds, input.clone())
            .with_apple_press_and_hold_enabled(false),
        cx,
    );
    // Zed's terminal invalidates the IME candidate on every wakeup so the panel
    // tracks the cursor. Paint is this surface's wakeup.
    window.invalidate_character_coordinates();
    false
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

/// `keystroke` in the shell's chord form, for chords without Command.
pub(super) fn chord(keystroke: &gpui::Keystroke) -> String {
    let modifiers = &keystroke.modifiers;
    let held = [
        (modifiers.control, "ctrl"),
        (modifiers.alt, "alt"),
        (modifiers.shift, "shift"),
    ];
    held.into_iter()
        .filter_map(|(down, name)| down.then_some(name))
        .chain([keystroke.key.as_str()])
        .collect::<Vec<_>>()
        .join("+")
}

fn shortcut(event: &gpui::KeyDownEvent, key: &str) -> bool {
    let modifiers = event.keystroke.modifiers;
    // Linux Ctrl-Shift clipboard commands belong to the effective app keymap
    // and bubble there before this fallback. Do not recapture an unbound chord.
    let clipboard = modifiers.platform && !modifiers.shift && !modifiers.alt;
    clipboard && event.keystroke.key == key
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

#[cfg(test)]
mod tests {
    use super::shortcut;

    #[test]
    fn clipboard_shortcuts_do_not_capture_shell_interrupts() {
        let event = |stroke| gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse(stroke).unwrap(),
            is_held: false,
            prefer_character_input: false,
        };
        assert!(!shortcut(&event("ctrl-c"), "c"));
        assert!(shortcut(&event("cmd-c"), "c"));
        assert!(!shortcut(&event("ctrl-shift-c"), "c"));
        assert!(!shortcut(&event("ctrl-shift-v"), "v"));
        assert!(!shortcut(&event("ctrl-alt-shift-c"), "c"));
    }
}
