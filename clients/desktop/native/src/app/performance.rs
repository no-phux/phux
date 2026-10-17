//! Window-scoped, opt-in desktop process monitoring. Dropping the view cancels
//! its timer; the task only ever holds GPUI's weak entity handle.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, FocusHandle, KeyBinding, SharedString, Task, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, TitleBar};
use phux_perf::{MetricSnapshot, MetricValue, PerfReport, Unit};

use crate::telemetry::{Sample, Sampler};

gpui::actions!(performance, [CloseMonitor]);

/// Kit installs the Root; only the window owns the returned content entity.
pub(crate) fn open(cx: &mut App) -> anyhow::Result<gpui_kit::AnyWindowHandle> {
    cx.bind_keys([KeyBinding::new(
        "cmd-w",
        CloseMonitor,
        Some("PerformanceMonitor"),
    )]);
    let mut titlebar = TitleBar::title_bar_options();
    titlebar.title = Some("phux Performance Monitor".into());
    let (handle, _) = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(940.), px(760.)), cx)),
            window_min_size: Some(size(px(760.), px(480.))),
            titlebar: Some(titlebar),
            focus: crate::diag::may_activate(),
            kind: crate::diag::window_kind(),
            show: true,
            ..TitleBar::window_options()
        },
        cx,
        |window, cx| cx.new(|cx| PerformanceMonitor::new(window, cx)),
    )?;
    Ok(handle)
}

struct PerformanceMonitor {
    sampler: Sampler,
    latest: Sample,
    sampled_at: Instant,
    interval: Option<PerfReport>,
    interval_seconds: Option<f64>,
    paused: bool,
    copy_status: Option<SharedString>,
    focus: FocusHandle,
    timer: Option<Task<()>>,
}

impl PerformanceMonitor {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut sampler = Sampler::new();
        let latest = sampler.sample();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut monitor = Self {
            sampler,
            latest,
            sampled_at: Instant::now(),
            interval: None,
            interval_seconds: None,
            paused: false,
            copy_status: None,
            focus,
            timer: None,
        };
        monitor.start_timer(cx);
        monitor
    }

    fn start_timer(&mut self, cx: &mut Context<Self>) {
        self.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |monitor, cx| {
                        monitor.sample();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn sample(&mut self) {
        let latest = self.sampler.sample();
        let now = Instant::now();
        self.interval_seconds = Some(now.duration_since(self.sampled_at).as_secs_f64());
        self.interval = Some(latest.report.delta(&self.latest.report));
        self.latest = latest;
        self.sampled_at = now;
        self.copy_status = None;
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        self.copy_status = None;
        if self.paused {
            // Cancelling, rather than keeping a paused loop, means no wakeups.
            self.timer = None;
        } else {
            // A fresh baseline excludes time spent paused from CPU and rates.
            self.sampler = Sampler::new();
            self.latest = self.sampler.sample();
            self.sampled_at = Instant::now();
            self.interval = None;
            self.interval_seconds = None;
            self.start_timer(cx);
        }
        cx.notify();
    }

    fn copy_json(&mut self, cx: &mut Context<Self>) {
        let value = serde_json::json!({
            "scope": "desktop_process",
            "paused": self.paused,
            "cpu_percent": self.latest.cpu_percent,
            "resident_bytes": self.latest.resident_bytes,
            "peak_resident_bytes": self.latest.peak_resident_bytes,
            "report": &self.latest.report,
            "interval_seconds": self.interval_seconds,
            "interval_report": &self.interval,
        });
        match serde_json::to_string_pretty(&value) {
            Ok(json) => {
                cx.write_to_clipboard(ClipboardItem::new_string(json));
                self.copy_status = Some("JSON copied".into());
            }
            Err(error) => {
                self.copy_status = Some(format!("Could not copy JSON: {error}").into());
            }
        }
        cx.notify();
    }

    fn metrics(&self, cx: &App) -> impl IntoElement {
        let mut rows = div().flex().flex_col().w_full();
        let mut previous_layer = "";
        for metric in &self.latest.report.metrics {
            let layer = metric.name.split('.').next().unwrap_or("other");
            if layer != previous_layer {
                rows = rows.child(
                    div()
                        .px_3()
                        .py_2()
                        .mt_2()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .bg(cx.theme().muted)
                        .child(
                            match layer {
                                "desktop" => "Desktop · rendering and UI",
                                "runtime" => "Runtime · publications and requests",
                                "kernel" => "Kernel · desktop client terminal state",
                                _ => layer,
                            }
                            .to_owned(),
                        ),
                );
                previous_layer = layer;
            }
            let interval_metric = self.interval.as_ref().and_then(|r| r.metric(&metric.name));
            let [rate, mean, p95] = metric_values(metric, interval_metric, self.interval_seconds);
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(metric.name.clone()),
                    )
                    .child(value_cell(rate))
                    .child(value_cell(mean))
                    .child(value_cell(p95)),
            );
        }
        rows
    }
}

impl gpui::Render for PerformanceMonitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cpu = self
            .latest
            .cpu_percent
            .filter(|value| value.is_finite())
            .map_or_else(unavailable, |value| format!("{value:.1}%"));
        let interval_label = self.interval_seconds.map_or_else(
            || "Interval metrics · waiting for next sample".to_owned(),
            |seconds| format!("Interval metrics · {seconds:.2} s"),
        );
        let status = if self.paused {
            "Paused · readings frozen"
        } else {
            "Live · sampling every second"
        };

        div()
            .id("performance-monitor")
            .key_context("PerformanceMonitor")
            .track_focus(&self.focus)
            .on_action(|_: &CloseMonitor, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_left()
            .child(TitleBar::new().child(
                div().text_sm().child("phux Performance Monitor"),
            ))
            .child(
                div()
                    .p_4()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .flex_shrink_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Desktop process"))
                                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!(
                                        "PID {} · {status}", self.latest.report.pid,
                                    ))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(Button::new("performance-pause").small().label(if self.paused { "Resume" } else { "Pause" }).on_click(cx.listener(|monitor, _, _, cx| monitor.toggle_pause(cx))))
                                    .child(Button::new("performance-copy").small().ghost().label("Copy JSON").on_click(cx.listener(|monitor, _, _, cx| monitor.copy_json(cx)))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(summary_card("CPU", cpu, "One core = 100%", cx))
                            .child(summary_card("Current memory", memory(self.latest.resident_bytes), "Resident set (RSS)", cx))
                            .child(summary_card("Peak memory", memory(self.latest.peak_resident_bytes), "Process lifetime high-water mark", cx)),
                    )
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Desktop app only — excludes the server and terminal child processes. — means unavailable or not sampled yet."))
                    .when_some(self.copy_status.clone(), |this, message| this.child(div().text_sm().text_color(cx.theme().muted_foreground).child(message)))
                    .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(interval_label))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Rates and timing distributions cover the last active interval. Gauges show current values. Copy JSON includes cumulative totals; p95 is a histogram estimate.")),
            )
            .child(
                div()
                    .mx_4()
                    .flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().child("Metric"))
                    .child(value_cell("Rate / current".to_owned()))
                    .child(value_cell("Mean".to_owned()))
                    .child(value_cell("p95".to_owned())),
            )
            .child(
                div()
                    .id("performance-metrics-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_4()
                    .child(self.metrics(cx)),
            )
    }
}

fn summary_card(
    title: &'static str,
    value: String,
    detail: &'static str,
    cx: &App,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
        .child(
            div()
                .text_2xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail),
        )
}

fn value_cell(value: String) -> impl IntoElement {
    div().w(px(120.)).flex_shrink_0().text_right().child(value)
}

fn unavailable() -> String {
    "—".to_owned()
}

fn memory(bytes: Option<u64>) -> String {
    bytes.map_or_else(unavailable, |value| {
        format!("{:.1} MiB", value as f64 / 1_048_576.)
    })
}

fn quantity(value: u64, unit: Unit) -> String {
    match unit {
        Unit::Micros if value >= 1_000 => format!("{:.2} ms", value as f64 / 1_000.),
        Unit::Micros => format!("{value} µs"),
        Unit::Bytes if value >= 1_048_576 => format!("{:.1} MiB", value as f64 / 1_048_576.),
        Unit::Bytes if value >= 1_024 => format!("{:.1} KiB", value as f64 / 1_024.),
        Unit::Bytes => format!("{value} B"),
        Unit::Count => value.to_string(),
    }
}

fn rate(value: u64, unit: Unit, seconds: f64) -> String {
    let per_second = value as f64 / seconds;
    match unit {
        Unit::Bytes if per_second >= 1_048_576. => format!("{:.1} MiB/s", per_second / 1_048_576.),
        Unit::Bytes if per_second >= 1_024. => format!("{:.1} KiB/s", per_second / 1_024.),
        Unit::Bytes => format!("{per_second:.1} B/s"),
        Unit::Micros => format!("{per_second:.1} µs/s"),
        Unit::Count => format!("{per_second:.1}/s"),
    }
}

fn metric_values(
    current: &MetricSnapshot,
    interval: Option<&MetricSnapshot>,
    seconds: Option<f64>,
) -> [String; 3] {
    if let MetricValue::Gauge(value) = &current.value {
        return [quantity(*value, current.unit), unavailable(), unavailable()];
    }
    let Some((interval, seconds)) = interval.zip(seconds.filter(|seconds| *seconds > 0.)) else {
        return [unavailable(), unavailable(), unavailable()];
    };
    match &interval.value {
        MetricValue::Histogram(histogram) => {
            let stat = |value: u64| {
                if histogram.is_empty() {
                    unavailable()
                } else {
                    quantity(value, current.unit)
                }
            };
            [
                rate(histogram.count, Unit::Count, seconds),
                stat(histogram.mean()),
                stat(histogram.percentile(95)),
            ]
        }
        MetricValue::Counter(value) => [
            rate(*value, current.unit, seconds),
            unavailable(),
            unavailable(),
        ],
        MetricValue::Gauge(value) => [quantity(*value, current.unit), unavailable(), unavailable()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, WindowHandle};
    use gpui_kit::component::Root;

    fn monitor(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<PerformanceMonitor>) {
        let handle = cx.update(|cx| {
            gpui_kit::init(cx);
            open(cx).unwrap().downcast::<Root>().unwrap()
        });
        let monitor = handle
            .update(cx, |root, _, _| {
                root.view()
                    .clone()
                    .downcast::<PerformanceMonitor>()
                    .unwrap()
            })
            .unwrap();
        (handle, monitor)
    }

    #[gpui::test]
    fn pause_cancels_sampling_and_resume_starts_a_fresh_interval(cx: &mut TestAppContext) {
        let (_, monitor) = monitor(cx);
        monitor.update(cx, |monitor, cx| {
            assert!(monitor.timer.is_some());
            assert!(monitor.latest.cpu_percent.is_none());
            monitor.sampled_at = Instant::now() - Duration::from_secs(30);
            monitor.sample();
            assert!(monitor.interval.is_some());
            assert!(monitor.interval_seconds.unwrap() >= 30.);

            monitor.toggle_pause(cx);
            assert!(monitor.paused);
            assert!(monitor.timer.is_none());
            // A stale timestamp represents the entire time spent paused.
            monitor.sampled_at = Instant::now() - Duration::from_secs(120);
            monitor.toggle_pause(cx);
            assert!(!monitor.paused);
            assert!(monitor.timer.is_some());
            assert!(monitor.latest.cpu_percent.is_none());
            assert!(monitor.interval.is_none());
            assert!(monitor.interval_seconds.is_none());
            assert!(monitor.sampled_at.elapsed() < Duration::from_secs(1));
        });
    }

    #[gpui::test]
    fn command_w_releases_the_monitor_and_its_sampling_task(cx: &mut TestAppContext) {
        let (handle, monitor) = monitor(cx);
        let weak = monitor.downgrade();
        drop(monitor);
        cx.run_until_parked();
        cx.simulate_keystrokes(*handle, "cmd-w");
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "sampling must not retain a closed monitor"
        );
    }

    #[test]
    fn idle_and_unsampled_histograms_do_not_fabricate_zero_latency() {
        let metric = MetricSnapshot {
            name: "desktop.paint".to_owned(),
            unit: Unit::Micros,
            value: MetricValue::Histogram(Default::default()),
        };
        assert_eq!(metric_values(&metric, None, None), ["—", "—", "—"]);
        assert_eq!(
            metric_values(&metric, Some(&metric), Some(2.)),
            ["0.0/s", "—", "—"],
        );
    }
}
