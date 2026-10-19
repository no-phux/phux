//! Window-scoped, opt-in desktop process monitoring. Dropping the view cancels
//! its timer; the task only ever holds GPUI's weak entity handle.

mod memory_view;

#[cfg(feature = "ui-fixtures")]
use gpui::AppContext as _;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, KeyBinding, SharedString,
    Subscription, Task, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::LineChart;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, TitleBar};
use phux_perf::{MetricSnapshot, MetricValue, PerfReport, Unit};

use crate::telemetry::{Sample, Sampler};

gpui::actions!(performance, [CloseMonitor, ToggleSampling, FilterMetrics]);

/// Two minutes of small numeric samples, never a history of full reports.
const HISTORY_LIMIT: usize = 120;

#[derive(Clone, Copy)]
struct Reading {
    second: u64,
    cpu: Option<f64>,
    rss: Option<f64>,
    footprint: Option<f64>,
    native_in_use: Option<f64>,
}

impl Reading {
    fn new(sample: &Sample) -> Self {
        Self {
            second: sample.report.uptime_ms / 1000,
            cpu: sample.cpu_percent,
            rss: sample.resident_bytes.map(|bytes| bytes as f64 / 1_048_576.),
            footprint: sample
                .footprint_bytes
                .map(|bytes| bytes as f64 / 1_048_576.),
            native_in_use: sample
                .native_heap
                .map(|heap| heap.in_use_bytes as f64 / 1_048_576.),
        }
    }
}

fn remember(history: &mut VecDeque<Reading>, sample: &Sample) {
    if history.len() == HISTORY_LIMIT {
        history.pop_front();
    }
    history.push_back(Reading::new(sample));
}

/// Kit installs the Root; only the window owns the returned content entity.
pub(crate) fn open(cx: &mut App) -> anyhow::Result<gpui_kit::AnyWindowHandle> {
    open_with_focus(crate::diag::may_activate(), cx)
}

fn open_with_focus(focus: bool, cx: &mut App) -> anyhow::Result<gpui_kit::AnyWindowHandle> {
    cx.bind_keys([
        KeyBinding::new("cmd-w", CloseMonitor, Some("PerformanceMonitor")),
        KeyBinding::new("ctrl-w", CloseMonitor, Some("PerformanceMonitor")),
        KeyBinding::new("escape", CloseMonitor, Some("PerformanceMonitor")),
        KeyBinding::new(
            "space",
            ToggleSampling,
            Some("PerformanceMonitor && !Input"),
        ),
        KeyBinding::new("cmd-f", FilterMetrics, Some("PerformanceMonitor")),
        KeyBinding::new("ctrl-f", FilterMetrics, Some("PerformanceMonitor")),
    ]);
    let mut titlebar = TitleBar::title_bar_options();
    titlebar.title = Some("phux Performance Monitor".into());
    let (handle, _) = gpui_kit::open_window(
        WindowOptions {
            app_id: Some("dev.phux.desktop".into()),
            window_bounds: Some(WindowBounds::centered(size(px(940.), px(760.)), cx)),
            window_min_size: Some(size(px(760.), px(480.))),
            titlebar: Some(titlebar),
            focus,
            kind: crate::diag::window_kind(),
            show: true,
            ..TitleBar::window_options()
        },
        cx,
        |window, cx| cx.new(|cx| PerformanceMonitor::new(window, cx)),
    )?;
    Ok(handle)
}

/// Native scene captures only; not evidence of compositor presentation or glass.
#[cfg(feature = "ui-fixtures")]
pub(super) fn preview(capture: Option<std::path::PathBuf>, cx: &mut App) -> anyhow::Result<()> {
    let handle = open_with_focus(false, cx)?
        .downcast::<gpui_kit::component::Root>()
        .expect("Kit Root");
    let Some(directory) = capture else {
        return Ok(());
    };
    std::fs::create_dir_all(&directory)?;
    cx.spawn(async move |cx| {
        for (tab, name) in ["overview", "memory", "metrics"].into_iter().enumerate() {
            handle
                .update(cx, |root, window, cx| {
                    let monitor = root
                        .view()
                        .clone()
                        .downcast::<PerformanceMonitor>()
                        .expect("monitor content");
                    monitor.update(cx, |monitor, cx| {
                        monitor.tab = tab;
                        cx.notify();
                    });
                    window.refresh();
                })
                .expect("preview window");
            cx.background_executor().timer(Duration::from_secs(3)).await;
            // An unfocused fixture may be occluded by the user's windows.
            // Build its current scene explicitly rather than capture an old one.
            cx.update_window(*handle, |_, window, cx| window.draw(cx).clear(cx))
                .expect("draw current preview scene");
            #[cfg(target_os = "macos")]
            if tab == 0 {
                // Attribute a settled native window before PNG readback buffers
                // contaminate the sample. Only this disposable fixture is read.
                let map = smol::unblock(|| {
                    std::process::Command::new("/usr/bin/vmmap")
                        .args(["-summary", &std::process::id().to_string()])
                        .output()
                })
                .await
                .expect("native memory map");
                assert!(
                    map.status.success(),
                    "vmmap: {}",
                    String::from_utf8_lossy(&map.stderr)
                );
                std::fs::write(directory.join("memory-before-capture.txt"), map.stdout)
                    .expect("write native memory attribution");
            }
            handle
                .update(cx, |_, window, _| {
                    window
                        .render_to_image()
                        .expect("monitor scene capture")
                        .save(directory.join(format!("{name}.png")))
                        .expect("write monitor PNG");
                })
                .expect("capture window");
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
    Ok(())
}

struct PerformanceMonitor {
    sampler: Sampler,
    latest: Sample,
    frames: crate::devtools::frames::Summary,
    sampled_at: Instant,
    interval: Option<PerfReport>,
    interval_seconds: Option<f64>,
    paused: bool,
    copy_status: Option<SharedString>,
    history: VecDeque<Reading>,
    tab: usize,
    recording: bool,
    reclaiming: bool,
    baseline: Reading,
    query: Entity<InputState>,
    _query: Subscription,
    server: (
        Option<crate::devtools::server::ServerSample>,
        Option<String>,
    ),
    focus: FocusHandle,
    timer: Option<Task<()>>,
    /// Keeps the server poller reading while the monitor is open.
    _server: Option<crate::devtools::server::Watch>,
}

impl PerformanceMonitor {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut sampler = Sampler::new();
        let latest = sampler.sample();
        let mut history = VecDeque::with_capacity(HISTORY_LIMIT);
        remember(&mut history, &latest);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Filter metrics…"));
        let subscription = cx.subscribe(&query, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let baseline = Reading::new(&latest);
        let mut monitor = Self {
            sampler,
            latest,
            frames: crate::devtools::frames::last_second(),
            sampled_at: Instant::now(),
            interval: None,
            interval_seconds: None,
            paused: false,
            copy_status: None,
            history,
            tab: 0,
            recording: false,
            reclaiming: false,
            baseline,
            query,
            _query: subscription,
            server: crate::devtools::get().poller.latest(),
            focus,
            timer: None,
            _server: crate::devtools::get()
                .cli
                .as_ref()
                .map(|cli| crate::devtools::get().poller.watch(cli)),
        };
        monitor.start_timer(cx);
        monitor
    }

    fn toggle_hud(&mut self, cx: &mut Context<Self>) {
        crate::devtools::set_hud(!crate::devtools::hud_visible());
        cx.notify();
    }

    fn capture_trace(&mut self, cx: &mut Context<Self>) {
        if self.recording {
            return;
        }
        self.recording = true;
        self.copy_status = Some("Recording a 5-second trace of the desktop and server…".into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(|| {
                crate::devtools::capture_trace(Duration::from_secs(5), true)
            })
            .await;
            let _ = this.update(cx, |monitor, cx| {
                monitor.recording = false;
                monitor.copy_status = Some(match result {
                    Ok(trace) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            trace.path.display().to_string(),
                        ));
                        let server = trace.server_error.map_or_else(String::new, |error| {
                            format!(" Server did not take part: {error}")
                        });
                        format!(
                            "Trace saved, path copied: {} ({} events). Open it in ui.perfetto.dev.{server}",
                            crate::devtools::display_path(&trace.path),
                            trace.events,
                        )
                        .into()
                    }
                    Err(error) => format!("Trace failed: {error:#}").into(),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn server_row(&self, cx: &App) -> impl IntoElement {
        let (sample, error) = &self.server;
        let cpu = sample
            .as_ref()
            .and_then(|sample| sample.cpu_percent)
            .map_or_else(unavailable, |value| format!("{value:.1}%"));
        let rss = memory(sample.as_ref().and_then(|sample| sample.report.rss_bytes));
        let heap = sample
            .as_ref()
            .and_then(|sample| sample.report.alloc)
            .map_or_else(unavailable, |alloc| signed_memory(alloc.live_bytes));
        let detail = match (&sample, error) {
            (_, Some(error)) => error.clone(),
            (Some(sample), None) => format!("PID {} · via `phux perf --json`", sample.report.pid),
            (None, None) if crate::devtools::get().cli.is_none() => {
                "No phux CLI is known to this desktop".to_owned()
            }
            (None, None) => "Waiting for the first reading".to_owned(),
        };
        let detail_tip: SharedString = detail.clone().into();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Server process"),
                    )
                    .child(
                        div()
                            .id("performance-server-status")
                            .flex_1()
                            .min_w_0()
                            .text_right()
                            .truncate()
                            .tooltip(move |window, cx| {
                                Tooltip::new(detail_tip.clone()).build(window, cx)
                            })
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(summary_card("Server CPU", cpu, "One core = 100%", cx))
                    .child(summary_card("Server memory", rss, "Resident set (RSS)", cx))
                    .child(summary_card(
                        "Server heap",
                        heap,
                        "Rust bytes; enable counting on the server",
                        cx,
                    )),
            )
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
        self.frames = crate::devtools::frames::last_second();
        self.server = crate::devtools::get().poller.latest();
        remember(&mut self.history, &self.latest);
        self.sampled_at = now;
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        self.copy_status = None;
        if self.paused {
            // Cancelling, rather than keeping a paused loop, means no wakeups.
            self.timer = None;
            self._server = None;
        } else {
            // A fresh baseline excludes time spent paused from CPU and rates.
            self.sampler = Sampler::new();
            self.latest = self.sampler.sample();
            self.frames = crate::devtools::frames::last_second();
            self.sampled_at = Instant::now();
            self.interval = None;
            self.interval_seconds = None;
            // Do not join the chart across an unobserved pause.
            self.history.clear();
            self._server = crate::devtools::get()
                .cli
                .as_ref()
                .map(|cli| crate::devtools::get().poller.watch(cli));
            self.server = crate::devtools::get().poller.latest();
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
            "physical_footprint_bytes": self.latest.footprint_bytes,
            "native_heap": self.latest.native_heap,
            "baseline": {
                "uptime_seconds": self.baseline.second,
                "rss_mib": self.baseline.rss,
                "footprint_mib": self.baseline.footprint,
                "native_in_use_mib": self.baseline.native_in_use,
            },
            "peak_resident_bytes": self.latest.peak_resident_bytes,
            "frames": self.frames,
            "server": self.server.0.as_ref().map(|sample| serde_json::json!({
                "cpu_percent": sample.cpu_percent,
                "report": sample.report,
            })),
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
        let query = self.query.read(cx).value().to_lowercase();
        let metrics: Vec<_> = self
            .latest
            .report
            .metrics
            .iter()
            .filter(|metric| matches_query(&metric.name, &query))
            .collect();
        if metrics.is_empty() {
            return rows.child(note(
                "No matching metrics. Try paint, kernel or runtime.",
                cx,
            ));
        }
        for metric in metrics {
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
                        .child(metric_layer_title(layer).to_owned()),
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

    fn overview(&self, cx: &App) -> impl IntoElement {
        let cpu = self
            .latest
            .cpu_percent
            .filter(|value| value.is_finite())
            .map_or_else(unavailable, |value| format!("{value:.1}%"));
        div().flex().flex_col().gap_4()
            .child(div().flex().gap_3()
                .child(summary_card("Desktop CPU", cpu, "One core = 100%", cx))
                .child(summary_card("App memory", memory(memory_view::app_memory_bytes(&self.latest)), memory_view::app_memory_detail(), cx))
                .child(summary_card("Since baseline", memory_view::memory_change(&Reading::new(&self.latest), &self.baseline), "Change since opening or setting a baseline", cx)))
            .child(div().flex().gap_3()
                .child(self.trend("CPU · % of one core", "performance-cpu", |r| r.cpu, cx))
                .child(self.trend("App memory · MiB", "performance-memory", memory_view::app_memory, cx)))
            .child(self.server_row(cx))
            .child(note("Desktop and server are separate processes. Terminal child processes are not included. Charts keep at most 120 readings; hover to inspect. — means unavailable.", cx))
    }

    fn trend(
        &self,
        title: &'static str,
        id: &'static str,
        value: fn(&Reading) -> Option<f64>,
        cx: &App,
    ) -> impl IntoElement {
        // Only connect contiguous successful readings; a failed query is a gap,
        // not zero, and must not fabricate an apparent memory release.
        let points = trend_points(&self.history, value);
        let waiting = points.len() < 2;
        div()
            .flex_1()
            .min_w_0()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .gap_2()
            .child(section_title(title))
            .child(
                div()
                    .h(px(160.))
                    .w_full()
                    .when(waiting, |this| {
                        this.child(note("Waiting for two available readings…", cx))
                    })
                    .when(!waiting, |this| {
                        this.child(
                            LineChart::new(points)
                                .id(id)
                                .x(|point: &(u64, f64)| format!("{} s", point.0))
                                .y(|point| point.1)
                                .linear()
                                .stroke(cx.theme().primary)
                                .x_tick_count(3)
                                .y_axis(true)
                                .y_tick_count(3)
                                .y_tick_format(|value| format!("{value:.0}"))
                                .tooltip_value(|_, value| format!("{value:.1}").into())
                                .appear(false),
                        )
                    }),
            )
    }

    fn metric_details(&self, cx: &App) -> impl IntoElement {
        let label = self.interval_seconds.map_or_else(
            || "Waiting for next sample".to_owned(),
            |seconds| format!("Last active interval · {seconds:.2} s"),
        );
        div().flex().flex_col().gap_2()
            .child(Input::new(&self.query).small())
            .child(section_title(label))
            .child(note("Rates and timing distributions cover the last active interval. Gauges show current values. Copy JSON includes cumulative totals; p95 is a histogram estimate.", cx))
            .child(div().flex().gap_3().px_3().py_2().text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(div().flex_1().child("Metric"))
                .child(value_cell("Rate / current".to_owned()))
                .child(value_cell("Mean".to_owned()))
                .child(value_cell("p95".to_owned())))
            .child(self.metrics(cx))
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("performance-toolbar")
            .debug_selector(|| "performance-toolbar".into())
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("performance-pause")
                    .small()
                    .label(if self.paused { "Resume" } else { "Pause" })
                    .on_click(cx.listener(|monitor, _, _, cx| monitor.toggle_pause(cx))),
            )
            .child(
                Button::new("performance-hud")
                    .small()
                    .ghost()
                    .label(if crate::devtools::hud_visible() {
                        "Hide HUD"
                    } else {
                        "Show HUD"
                    })
                    .on_click(cx.listener(|monitor, _, _, cx| monitor.toggle_hud(cx))),
            )
            .child(
                Button::new("performance-trace")
                    .small()
                    .ghost()
                    .disabled(self.recording)
                    .label(if self.recording {
                        "Recording…"
                    } else {
                        "5 s Trace"
                    })
                    .on_click(cx.listener(|monitor, _, _, cx| monitor.capture_trace(cx))),
            )
            .child(
                Button::new("performance-copy")
                    .small()
                    .ghost()
                    .label("Copy JSON")
                    .on_click(cx.listener(|monitor, _, _, cx| monitor.copy_json(cx))),
            )
    }

    fn status_line(&self, cx: &App) -> impl IntoElement {
        let status = self.copy_status.clone().unwrap_or_else(|| {
            "Space to pause · ⌘F / Ctrl-F to filter metrics · Esc to close".into()
        });
        let tooltip = status.clone();
        div()
            .id("performance-status")
            .h(px(32.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .truncate()
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(status)
    }
}

impl gpui::Render for PerformanceMonitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_action(cx.listener(|monitor, _: &ToggleSampling, _, cx| monitor.toggle_pause(cx)))
            .on_action(cx.listener(|monitor, _: &FilterMetrics, window, cx| {
                monitor.tab = 2;
                let focus = monitor.query.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                cx.notify();
            }))
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
                                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Performance"))
                                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!(
                                        "PID {} · {status}", self.latest.report.pid,
                                    ))),
                            )
                            .child(self.toolbar(cx)),
                    )
                    .child(TabBar::new("performance-tabs").segmented()
                        .selected_index(self.tab)
                        .children([Tab::new().label("Overview"), Tab::new().label("Memory"), Tab::new().label("Metrics")])
                        .on_click(cx.listener(|monitor, index: &usize, window, cx| {
                            monitor.tab = *index;
                            window.focus(&monitor.focus, cx);
                            cx.notify();
                        })))
                    // Reserve a status line so copying or recording cannot move content.
                    .child(self.status_line(cx)),
            )
            .child(
                div()
                    .id("performance-metrics-scroll")
                    .debug_selector(|| "performance-metrics-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_4()
                    .child(match self.tab {
                        1 => self.memory_details(cx).into_any_element(),
                        2 => self.metric_details(cx).into_any_element(),
                        _ => self.overview(cx).into_any_element(),
                    }),
            )
    }
}

fn section_title(title: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(title.into())
}

fn matches_query(name: &str, query: &str) -> bool {
    query.split_whitespace().all(|term| name.contains(term))
}

fn metric_layer_title(layer: &str) -> &str {
    match layer {
        "desktop" => "Desktop · rendering and UI",
        "runtime" => "Runtime · publications and requests",
        "kernel" => "Kernel · desktop client terminal state",
        _ => layer,
    }
}

fn trend_points(
    history: &VecDeque<Reading>,
    value: fn(&Reading) -> Option<f64>,
) -> Vec<(u64, f64)> {
    let mut points: Vec<_> = history
        .iter()
        .rev()
        .map_while(|reading| value(reading).map(|v| (reading.second, v)))
        .collect();
    points.reverse();
    points
}

fn note(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
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

fn signed_memory(bytes: i64) -> String {
    let magnitude = memory(Some(bytes.unsigned_abs()));
    if bytes < 0 {
        format!("-{magnitude}")
    } else {
        magnitude
    }
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

    #[test]
    fn metric_queries_match_every_word_and_allow_empty_searches() {
        assert!(matches_query("desktop.paint", "desktop paint"));
        assert!(matches_query("desktop.paint", ""));
        assert!(!matches_query("desktop.paint", "runtime paint"));
    }

    #[gpui::test]
    fn a_release_finishing_after_pause_keeps_all_displayed_readings_frozen(
        cx: &mut TestAppContext,
    ) {
        let (_, monitor) = monitor(cx);
        monitor.update(cx, |monitor, cx| {
            monitor.reclaiming = true;
            monitor.toggle_pause(cx);
            let frozen_report = monitor.latest.report.clone();
            let frozen_frames = monitor.frames;
            monitor.finish_release(Some(1024), cx);
            assert!(!monitor.reclaiming);
            assert!(monitor.paused);
            assert!(monitor.timer.is_none());
            assert_eq!(monitor.latest.report, frozen_report);
            assert_eq!(monitor.frames, frozen_frames);
            assert!(monitor.copy_status.as_ref().unwrap().contains("1.0 KiB"));
        });
    }

    #[gpui::test]
    fn baseline_compares_the_displayed_snapshot_without_resetting_counters(
        cx: &mut TestAppContext,
    ) {
        let (_, monitor) = monitor(cx);
        monitor.update(cx, |monitor, cx| {
            monitor.latest.footprint_bytes = Some(200 * 1_048_576);
            monitor.latest.resident_bytes = Some(400 * 1_048_576);
            monitor.set_baseline(cx);
            assert_eq!(monitor.history.len(), 1);
            let report = monitor.latest.report.clone();
            monitor.latest.footprint_bytes = Some(240 * 1_048_576);
            monitor.latest.resident_bytes = Some(440 * 1_048_576);
            assert_eq!(
                memory_view::memory_change(&Reading::new(&monitor.latest), &monitor.baseline),
                "+40.0 MiB"
            );
            monitor.set_baseline(cx);
            assert_eq!(
                memory_view::memory_change(&Reading::new(&monitor.latest), &monitor.baseline),
                "+0.0 MiB"
            );
            assert_eq!(monitor.latest.report, report);
        });
    }

    #[gpui::test]
    fn filtering_metrics_keeps_spaces_in_the_field_instead_of_pausing(cx: &mut TestAppContext) {
        let (handle, monitor) = monitor(cx);
        let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
        cx.simulate_keystrokes("cmd-f");
        cx.run_until_parked();
        cx.simulate_input("desktop");
        cx.simulate_keystrokes("space");
        cx.simulate_input("paint");
        monitor.read_with(cx, |monitor, cx| {
            assert_eq!(monitor.tab, 2);
            assert!(
                !monitor.paused,
                "typing spaces must not trigger the monitor shortcut"
            );
            assert_eq!(monitor.query.read(cx).value().as_ref(), "desktop paint");
        });
    }

    #[gpui::test]
    fn history_is_bounded_and_resume_discards_the_unobserved_gap(cx: &mut TestAppContext) {
        let (_, monitor) = monitor(cx);
        monitor.update(cx, |monitor, cx| {
            for second in 0..150 {
                monitor.latest.report.uptime_ms = second * 1000;
                monitor.latest.resident_bytes = Some(second * 1_048_576);
                remember(&mut monitor.history, &monitor.latest);
            }
            assert_eq!(monitor.history.len(), HISTORY_LIMIT);
            assert_eq!(monitor.history.front().unwrap().second, 30);
            assert_eq!(monitor.history.back().unwrap().rss, Some(149.));
            monitor.toggle_pause(cx);
            let frozen = monitor.latest.report.clone();
            monitor.copy_json(cx);
            assert_eq!(monitor.latest.report, frozen);
            monitor.toggle_pause(cx);
            assert!(monitor.history.is_empty());
        });
    }

    #[test]
    fn failed_readings_break_trends_instead_of_becoming_zero() {
        let history = VecDeque::from([
            Reading {
                second: 1,
                cpu: Some(50.),
                rss: None,
                footprint: None,
                native_in_use: None,
            },
            Reading {
                second: 2,
                cpu: None,
                rss: None,
                footprint: None,
                native_in_use: None,
            },
            Reading {
                second: 3,
                cpu: Some(0.),
                rss: None,
                footprint: None,
                native_in_use: None,
            },
        ]);
        assert_eq!(trend_points(&history, |r| r.cpu), [(3, 0.)]);
        assert!(trend_points(&history, |r| r.rss).is_empty());
    }

    #[gpui::test]
    fn every_tab_fits_a_short_window_and_keyboard_pauses(cx: &mut TestAppContext) {
        let (handle, monitor) = monitor(cx);
        let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
        cx.simulate_resize(size(px(760.), px(480.)));
        for tab in 0..3 {
            monitor.update(cx, |monitor, cx| {
                monitor.tab = tab;
                monitor.sample();
                monitor.sample();
                cx.notify();
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            let scroll = cx.debug_bounds("performance-metrics-scroll").unwrap();
            assert!(
                scroll.size.height > px(200.),
                "the header must leave usable content space"
            );
            let copy = cx.debug_bounds("performance-toolbar").unwrap();
            assert!(
                copy.right() <= px(760.),
                "toolbar must fit at minimum width"
            );
        }
        cx.simulate_keystrokes("space");
        monitor.read_with(cx, |monitor, _| assert!(monitor.paused));
        cx.simulate_keystrokes("space");
        monitor.read_with(cx, |monitor, _| assert!(!monitor.paused));
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
