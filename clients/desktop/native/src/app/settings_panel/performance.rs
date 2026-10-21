//! Process-wide, live profiling controls. Never persist profiling overhead or
//! turn on the development socket as a side effect of a settings switch.

use super::catalog::{Content as SettingPage, Group as SettingGroup, Item as SettingItem};
use gpui::WeakEntity;
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::SettingField;
use gpui_kit::component::{IconName, Sizable};

use super::super::desk::Desk;
use super::click_runs;

pub(super) fn page(handle: &WeakEntity<Desk>) -> SettingPage {
    let monitor = handle.clone();
    SettingPage::new("Performance")
        .icon(IconName::Frame)
        .group(SettingGroup::new().title("Desktop profiling")
            .description("Applies immediately to all desktop windows. No restart, environment variable or server change. These controls are for this run only.")
            .item(SettingItem::switch("Count Rust allocations",
                |_| phux_perf::alloc::enabled(),
                crate::devtools::set_allocation_counting,
                None,
            )
                .description("Tracks allocations and net Rust bytes from now, not existing heap or native GPU memory. Frees of older blocks can make the net negative. Counting adds atomic work per allocation; turn it off after profiling."))
            .item(SettingItem::switch("Show performance HUD",
                |_| crate::devtools::hud_visible(),
                |visible, cx| { crate::devtools::set_hud(visible); cx.refresh_windows(); },
                None,
            )
                .description("Live frame, CPU and memory readings over your terminal."))
            .item(SettingItem::new("Performance Monitor", SettingField::render(move |_, _, _| {
                Button::new("settings-performance-monitor").small()
                    .label("Open")
                    .on_click(click_runs(&monitor, |desk, window, cx| desk.execute("performance-monitor", window, cx)))
            }))
                .description("Frame times, memory and server spans in their own window.")))
}
