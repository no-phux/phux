//! Window-scoped, opt-in desktop process monitoring. Dropping the view cancels
//! its timer; the task only ever holds GPUI's weak entity handle.

mod formatting;
mod memory_view;
mod rendering;

#[cfg(test)]
mod tests;

use formatting::*;
use rendering::*;

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
}
