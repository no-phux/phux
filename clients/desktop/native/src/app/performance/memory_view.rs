//! Actionable memory readings: real malloc inventory, baseline comparisons,
//! opt-in Rust counters and a deliberate off-thread allocator relief request.

use super::*;

pub(super) fn app_memory_bytes(sample: &Sample) -> Option<u64> {
    if cfg!(target_os = "macos") {
        sample.footprint_bytes
    } else {
        sample.resident_bytes
    }
}

pub(super) fn app_memory(reading: &Reading) -> Option<f64> {
    if cfg!(target_os = "macos") {
        reading.footprint
    } else {
        reading.rss
    }
}

pub(super) const fn app_memory_detail() -> &'static str {
    if cfg!(target_os = "macos") {
        "Physical footprint · Activity Monitor accounting"
    } else {
        "Resident set · shared pages included"
    }
}

pub(super) fn memory_change(current: &Reading, baseline: &Reading) -> String {
    app_memory(current)
        .zip(app_memory(baseline))
        .map_or_else(unavailable, |(now, before)| {
            format!("{:+.1} MiB", now - before)
        })
}

impl PerformanceMonitor {
    pub(super) fn memory_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let heap = self.latest.native_heap;
        div().flex().flex_col().gap_4()
            .child(div().flex().gap_3()
                .child(summary_card("App memory", memory(app_memory_bytes(&self.latest)), app_memory_detail(), cx))
                .child(summary_card("Native heap in use", memory(heap.map(|h| h.in_use_bytes)), "Allocated blocks across malloc zones", cx))
                .child(summary_card("Malloc capacity", memory(heap.map(|h| h.capacity_bytes)), "Reserved space · not resident RAM", cx)))
            .child(self.memory_actions(cx))
            .child(self.rust_accounting(cx))
            .child(div().flex().gap_3()
                .child(self.trend("App memory · MiB", "performance-footprint", app_memory, cx))
                .when(heap.is_some(), |this| this.child(self.trend("Native heap in use · MiB", "performance-native-heap", |r| r.native_in_use, cx))))
            .child(section_title("Read the numbers, not just the peak"))
            .child(note(format!("{} since baseline ({} s ago). RSS {} · lifetime peak RSS {}. A rising peak alone does not prove a leak. Set a baseline, repeat the same workload, then compare after it settles.", memory_change(&Reading::new(&self.latest), &self.baseline), self.latest.report.uptime_ms.saturating_div(1000).saturating_sub(self.baseline.second), memory(self.latest.resident_bytes), memory(self.latest.peak_resident_bytes)), cx))
            .child(note("Native heap includes Rust/System and native malloc allocations, even those made before profiling. Capacity includes reusable and possibly nonresident pages. These values overlap with app memory: do not add them or treat footprint minus heap as GPU usage. Native malloc readings and page release are currently macOS-only.", cx))
            .child(note(format!("Window surfaces, Metal buffers and other VM mappings are not fully represented by malloc. For native region attribution on macOS, run `vmmap -summary {}`. Compare the same window size, scale, panes and workload.", self.latest.report.pid), cx))
    }

    fn memory_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().items_center().gap_2()
            .child(Button::new("performance-baseline").small().label("Set baseline")
                .tooltip("Compare the current reading with later samples; resets chart history, not counters")
                .on_click(cx.listener(|monitor, _, _, cx| monitor.set_baseline(cx))))
            .child(Button::new("performance-reclaim").small().ghost()
                .label(if self.reclaiming { "Releasing…" } else { "Release unused heap" })
                .disabled(self.paused || self.reclaiming || self.latest.native_heap.is_none())
                .tooltip("Ask macOS malloc to return unused pages. Live memory stays intact. Resume sampling first.")
                .on_click(cx.listener(|monitor, _, _, cx| monitor.release_unused(cx))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child(memory_change(&Reading::new(&self.latest), &self.baseline)))
    }

    fn rust_accounting(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = phux_perf::alloc::enabled();
        div().p_3().rounded_lg().border_1().border_color(cx.theme().border)
            .flex().flex_col().gap_2()
            .child(div().flex().items_center().justify_between()
                .child(section_title(format!("Rust allocation trend · {}", self.latest.report.alloc.map_or_else(unavailable, |a| signed_memory(a.live_bytes)))))
                .child(Button::new("performance-count-allocations").small()
                    .label(if enabled { "Stop counting" } else { "Count allocations" })
                    .on_click(cx.listener(|monitor, _, _, cx| {
                        crate::devtools::set_allocation_counting(!phux_perf::alloc::enabled(), cx);
                        if !monitor.paused { monitor.sample(); }
                        cx.notify();
                    }))))
            .child(note("Applies immediately across desktop windows; no restart or dev socket. Counts net Rust bytes from enable time, so freeing older blocks may make it negative. It excludes native allocations and adds per-allocation overhead while enabled.", cx))
    }

    pub(super) fn set_baseline(&mut self, cx: &mut Context<Self>) {
        self.baseline = Reading::new(&self.latest);
        self.history.clear();
        remember(&mut self.history, &self.latest);
        self.copy_status =
            Some("Baseline set to the current reading; chart history restarted.".into());
        cx.notify();
    }

    fn release_unused(&mut self, cx: &mut Context<Self>) {
        if self.reclaiming || self.paused || self.latest.native_heap.is_none() {
            return;
        }
        self.reclaiming = true;
        self.copy_status = Some("Asking the allocator to release unused pages…".into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let released = smol::unblock(phux_perf::alloc::native::release_unused).await;
            let _ = this.update(cx, |monitor, cx| {
                monitor.finish_release(released, cx);
            });
        })
        .detach();
    }

    pub(super) fn finish_release(&mut self, released: Option<u64>, cx: &mut Context<Self>) {
        self.reclaiming = false;
        if !self.paused {
            self.sample();
        }
        self.copy_status = Some(released.map_or_else(
            || "Native allocator relief is unavailable on this platform.".to_owned(),
            |bytes| format!("Allocator returned {}. Live allocations are untouched; footprint may not fall by the same amount.", quantity(bytes, Unit::Bytes)),
        ).into());
        cx.notify();
    }
}
