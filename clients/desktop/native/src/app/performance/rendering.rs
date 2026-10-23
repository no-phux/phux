//! Monitor layout, charts, controls and metric rows.

use super::*;

impl PerformanceMonitor {
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

    pub(super) fn trend(
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
            .child(toolbar_slot(
                "performance-pause",
                76.,
                Button::new("performance-pause")
                    .small()
                    .label(if self.paused { "Resume" } else { "Pause" })
                    .on_click(cx.listener(|monitor, _, _, cx| monitor.toggle_pause(cx))),
            ))
            .child(toolbar_slot(
                "performance-hud",
                94.,
                Button::new("performance-hud")
                    .small()
                    .ghost()
                    .label(if crate::devtools::hud_visible() {
                        "Hide HUD"
                    } else {
                        "Show HUD"
                    })
                    .on_click(cx.listener(|monitor, _, _, cx| monitor.toggle_hud(cx))),
            ))
            .child(toolbar_slot(
                "performance-trace",
                100.,
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
            ))
            .child(toolbar_slot(
                "performance-copy",
                94.,
                Button::new("performance-copy")
                    .small()
                    .ghost()
                    .label("Copy JSON")
                    .on_click(cx.listener(|monitor, _, _, cx| monitor.copy_json(cx))),
            ))
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

/// Reserve the longest label so state changes never move neighboring actions.
fn toolbar_slot(selector: &'static str, width: f32, button: Button) -> impl IntoElement {
    div()
        .debug_selector(move || selector.into())
        .w(px(width))
        .flex_shrink_0()
        .child(button.w_full())
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
            .line_height(gpui::relative(crate::app::ui::typography::LEADING))
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
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Performance"))
                                    .child(div().truncate().text_sm().text_color(cx.theme().muted_foreground).child(format!(
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

pub(super) fn section_title(title: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(title.into())
}

pub(super) fn matches_query(name: &str, query: &str) -> bool {
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

pub(super) fn trend_points(
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

pub(super) fn note(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

pub(super) fn summary_card(
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
