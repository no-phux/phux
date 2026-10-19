//! Process-wide, live profiling controls. Never persist profiling overhead or
//! turn on the development socket as a side effect of a settings switch.

use gpui::WeakEntity;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{IconName, Sizable};

use super::super::desk::Desk;
use super::click_runs;

pub(super) fn page(handle: &WeakEntity<Desk>) -> SettingPage {
    let monitor = handle.clone();
    SettingPage::new("Performance")
        .icon(IconName::Frame)
        .group(SettingGroup::new().title("Desktop profiling")
            .description("Applies immediately to all desktop windows. No restart, environment variable or server change. These controls are for this run only.")
            .item(SettingItem::new("Count Rust allocations", SettingField::switch(
                |_| phux_perf::alloc::enabled(),
                crate::devtools::set_allocation_counting,
            ))
                .description("Tracks allocations and net Rust bytes from now, not existing heap or native GPU memory. Frees of older blocks can make the net negative. Counting adds atomic work per allocation; turn it off after profiling."))
            .item(SettingItem::new("Show performance HUD", SettingField::switch(
                |_| crate::devtools::hud_visible(),
                |visible, cx| { crate::devtools::set_hud(visible); cx.refresh_windows(); },
            ))
                .description("Live frame, CPU and memory readings over your terminal."))
            .item(SettingItem::render(move |_, _, _| {
                Button::new("settings-performance-monitor").small().primary()
                    .label("Open Performance Monitor")
                    .on_click(click_runs(&monitor, |desk, window, cx| desk.execute("performance-monitor", window, cx)))
            })))
}
