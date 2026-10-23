//! The Developer page: the one place to debug the desktop.
//!
//! **Developer mode** is the deliberate, saved switch: it turns on heap
//! accounting, the HUD and the dev socket at every launch, and applies live
//! ([`crate::devtools::set_dev_mode`]). Everything below it is incidental
//! and lasts for this run only: a profiling switch never saves its overhead
//! or starts the dev socket as a side effect.

use std::path::PathBuf;

use super::catalog::{Content as SettingPage, Group as SettingGroup, Item as SettingItem};
use gpui::{ClipboardItem, WeakEntity};
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::SettingField;
use gpui_kit::component::{Disableable, IconName, Sizable};

use super::super::desk::Desk;
use super::click_runs;
use crate::devtools::{self, SocketState};

pub(super) fn page(handle: &WeakEntity<Desk>) -> SettingPage {
    SettingPage::new("Developer")
        .icon(IconName::Frame)
        .group(mode_group(handle))
        .group(profiling_group(handle))
        .group(debugging_group(handle))
}

fn mode_group(handle: &WeakEntity<Desk>) -> SettingGroup {
    let writer = handle.clone();
    SettingGroup::new()
        .item(
            SettingItem::switch(
                "Developer mode",
                |_| devtools::dev_mode(),
                move |on, cx| {
                    let _ = writer.update(cx, |desk, cx| desk.set_developer_mode(on, cx));
                },
                Some(false),
            )
            .description(mode_description())
            .keywords([
                "developer",
                "dev",
                "debug",
                "telemetry",
                "socket",
                "phux_dev",
            ]),
        )
        .item(socket_item())
}

fn mode_description() -> String {
    let forced = if devtools::get().forced {
        " PHUX_DEV or --dev holds it on until you quit."
    } else {
        ""
    };
    format!(
        "Turns on the performance HUD, Rust allocation counting and the dev socket in \
         every window, now and at every launch. Off removes all three.{forced}"
    )
}

/// Where the dev socket listens, for `phux perf --desktop` and agents.
fn socket_item() -> SettingItem {
    let (description, path) = match devtools::socket_state() {
        SocketState::Listening(path) => (
            format!(
                "Listening at {}. `phux perf --desktop` and agents read it.",
                devtools::display_path(&path)
            ),
            Some(path),
        ),
        SocketState::Failed(error) => (format!("Did not start: {error}"), None),
        SocketState::Off => ("Off. Developer mode starts it.".to_owned(), None),
    };
    SettingItem::new(
        "Dev socket",
        SettingField::render(move |_, _, _| {
            let path = path.clone();
            Button::new("settings-dev-socket-copy")
                .small()
                .label("Copy path")
                .disabled(path.is_none())
                .on_click(move |_, _, cx| copy_path(path.as_ref(), cx))
        }),
    )
    .description(description)
    .keywords(["socket", "perf", "agent", "dev.sock"])
}

fn copy_path(path: Option<&PathBuf>, cx: &mut gpui::App) {
    if let Some(path) = path {
        cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
    }
}

const PROFILING: &str = "Applies immediately to all desktop windows, for this run only. \
    Nothing here is saved, starts the dev socket or changes the server.";
const ALLOCATIONS: &str = "Tracks allocations and net Rust bytes from now, not existing heap \
    or native GPU memory. Frees of older blocks can make the net negative. Counting adds \
    atomic work per allocation; turn it off after profiling.";
const DIAGNOSTICS: &str = "Versions, server id, connection epoch, host and recent errors for \
    a bug report. No terminal content.";

fn profiling_group(handle: &WeakEntity<Desk>) -> SettingGroup {
    let allocations = SettingItem::switch(
        "Count Rust allocations",
        |_| phux_perf::alloc::enabled(),
        devtools::set_allocation_counting,
        None,
    )
    .description(ALLOCATIONS);
    let hud = SettingItem::switch(
        "Show performance HUD",
        |_| devtools::hud_visible(),
        |visible, cx| {
            devtools::set_hud(visible);
            cx.refresh_windows();
        },
        None,
    )
    .description("Live frame, CPU and memory readings over your terminal.");
    let monitor = command_item(handle, "Performance Monitor", "Open", "performance-monitor")
        .description("Frame times, memory and server spans in their own window.");
    let trace = command_item(handle, "Capture performance trace", "Record", "perf-trace")
        .description(
            "Five seconds of desktop and server spans in one Perfetto file; its path is copied.",
        )
        .keywords(["trace", "perfetto", "profile", "timeline", "spans"]);
    SettingGroup::new()
        .title("Profiling")
        .description(PROFILING)
        .item(allocations)
        .item(hud)
        .item(monitor)
        .item(trace)
}

fn debugging_group(handle: &WeakEntity<Desk>) -> SettingGroup {
    let diagnostics = command_item(handle, "Copy diagnostics", "Copy", "copy-diagnostics")
        .description(DIAGNOSTICS)
        .keywords(["support", "report", "bug", "version", "errors"]);
    let log = reveal_item(
        Reveal::File("Desktop log"),
        crate::diag::log_path(),
        "PHUX_DESKTOP_LOG_LEVEL=debug at launch logs more.",
    )
    .keywords(["log", "logs", "errors", "debug"]);
    let traces = reveal_item(
        Reveal::Folder("Traces folder"),
        Some(devtools::trace_dir()),
        "Where captured Perfetto traces go.",
    )
    .keywords(["trace", "perfetto", "folder"]);
    SettingGroup::new()
        .title("Debugging")
        .item(diagnostics)
        .item(log)
        .item(traces)
}

/// A row whose button runs a palette command on this window.
fn command_item(
    handle: &WeakEntity<Desk>,
    title: &'static str,
    label: &'static str,
    command: &'static str,
) -> SettingItem {
    let handle = handle.clone();
    SettingItem::new(
        title,
        SettingField::render(move |_, _, _| {
            Button::new(format!("settings-developer-{command}"))
                .small()
                .label(label)
                .on_click(click_runs(&handle, move |desk, window, cx| {
                    desk.execute(command, window, cx);
                }))
        }),
    )
}

/// What a reveal row names: a file, or a folder that may not exist yet.
#[derive(Clone, Copy)]
enum Reveal {
    File(&'static str),
    Folder(&'static str),
}

/// A file or folder, named in the description, that the button shows in the
/// file manager.
fn reveal_item(target: Reveal, path: Option<PathBuf>, note: &str) -> SettingItem {
    let (Reveal::File(title) | Reveal::Folder(title)) = target;
    let description = path.as_ref().map_or_else(
        || format!("Unavailable. {note}"),
        |path| format!("{}. {note}", devtools::display_path(path)),
    );
    SettingItem::new(
        title,
        SettingField::render(move |_, _, _| {
            let path = path.clone();
            Button::new(format!("settings-developer-reveal-{title}"))
                .small()
                .label("Reveal")
                .disabled(path.is_none())
                .on_click(move |_, _, cx| reveal(target, path.as_ref(), cx))
        }),
    )
    .description(description)
}

fn reveal(target: Reveal, path: Option<&PathBuf>, cx: &mut gpui::App) {
    let Some(path) = path else {
        return;
    };
    // A folder nothing has written to yet is still worth opening.
    if matches!(target, Reveal::Folder(_)) {
        let _ = std::fs::create_dir_all(path);
    }
    cx.reveal_path(path);
}
