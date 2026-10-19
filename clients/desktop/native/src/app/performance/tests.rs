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
fn a_release_finishing_after_pause_keeps_all_displayed_readings_frozen(cx: &mut TestAppContext) {
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
fn baseline_compares_the_displayed_snapshot_without_resetting_counters(cx: &mut TestAppContext) {
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
