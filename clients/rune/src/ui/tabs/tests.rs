use super::*;

fn named(count: u8, active: u8) -> Vec<Tab> {
    (0..count)
        .map(|i| Tab {
            locked: false,
            id: [i; 16],
            name: format!("a tab with a long name {i}"),
            resources: Vec::new(),
            active: i == active,
        })
        .collect()
}

fn laid_out(count: u8, active: u8, window: (f32, f32)) -> rune::Runtime<Desk> {
    let mut desk = crate::test_support::settings_window();
    desk.settings_open = false;
    // `auto` puts the tabs in the title bar while the sidebar is folded.
    desk.sidebar_open = false;
    desk.connection.tabs.list = named(count, active);
    let mut rt =
        crate::test_support::runtime(desk, crate::ui::view, window, rune::Env::new(false, 1.0));
    // The bar reports its room on the first frame; the tabs fit it after.
    for frame in 0..4 {
        rt.frame(f64::from(frame));
    }
    rt
}

#[test]
fn a_crowd_of_tabs_never_runs_under_the_window_controls() {
    for (count, window) in [
        (3, (760.0, 500.0)),
        (12, (760.0, 500.0)),
        (30, (640.0, 400.0)),
    ] {
        let rt = laid_out(count, 1, window);
        assert!(
            rt.state.connection.tabs.room > 0.0,
            "the bar reported its room"
        );
        let add = rt.presented("tab-new").expect("the new-tab button").0;
        let bell = rt.presented("news-button").expect("the bell").0;
        // Every tab is inside the strip, which ends before the button:
        // none is drawn past its own box.
        let last = rt.presented(&format!("tab-{}", count - 1));
        if let Some((last, ..)) =
            last.filter(|_| !FIT.crowded(rt.state.connection.tabs.room, count.into()))
        {
            assert!(
                last[0] + last[2] <= add[0] + 0.5,
                "{count} tabs: the last ends at {}, new-tab starts at {}",
                last[0] + last[2],
                add[0]
            );
        }
        assert!(
            add[0] + add[2] <= bell[0],
            "{count} tabs: new-tab ends at {}, the bell starts at {}",
            add[0] + add[2],
            bell[0]
        );
    }
}

#[test]
fn the_tab_bar_goes_where_the_file_puts_it() {
    use phux_config_core::rune::{TabBar, TitleBar};
    fn at(tab_bar: TabBar, title_bar: TitleBar, open: bool) -> (Option<f32>, bool) {
        let mut desk = crate::test_support::settings_window();
        desk.settings_open = false;
        desk.sidebar_open = open;
        desk.settings.tab_bar = tab_bar;
        desk.settings.title_bar = title_bar;
        desk.connection.tabs.list = named(3, 0);
        let window = (900.0, 500.0);
        let env = rune::Env::new(false, 1.0);
        let mut rt = crate::test_support::runtime(desk, crate::ui::view, window, env);
        for frame in 0..4 {
            rt.frame(f64::from(frame));
        }
        let tab = rt.presented("tab-0").map(|(rect, ..)| rect[1]);
        (tab, rt.find("settings-button").is_some())
    }
    let bars = |tab_bar, title_bar| at(tab_bar, title_bar, true);
    let folded = |tab_bar| at(tab_bar, TitleBar::Shown, false).0;
    let (top, titled) = bars(TabBar::Top, TitleBar::Shown);
    let (bottom, _) = bars(TabBar::Bottom, TitleBar::Shown);
    assert!(titled && bottom.expect("bottom tabs") > top.expect("top tabs") + 300.0);
    assert_eq!(bars(TabBar::Hidden, TitleBar::Shown).0, None);
    let (row, titled) = bars(TabBar::Top, TitleBar::Hidden);
    assert!(!titled && row.expect("tabs on a row of their own") < 40.0);
    // `auto`: the docked sidebar's rows are the tabs; folded, the
    // title bar holds them.
    assert_eq!(bars(TabBar::Auto, TitleBar::Shown).0, None);
    assert_eq!(folded(TabBar::Auto), top);
}

#[test]
fn the_active_tab_keeps_its_name_in_a_crowd() {
    let rt = laid_out(12, 5, (900.0, 500.0));
    let active = rt.presented("tab-5").expect("the active tab").0;
    let resting = rt.presented("tab-0").expect("a resting tab").0;
    assert!(active[2] >= FIT.active_least - 0.5, "active: {active:?}");
    assert!(resting[2] < active[2], "resting: {resting:?}");
    assert!(resting[2] >= FIT.narrowest - 0.5, "resting: {resting:?}");
}

#[test]
fn tabs_share_the_room_and_stop_at_their_widest() {
    let Fit {
        widest,
        narrowest,
        active_least,
        ..
    } = FIT;
    assert_eq!(FIT.share(0.0, 3), widest, "before the bar is laid out");
    assert_eq!(FIT.share(2000.0, 2), widest);
    let even = FIT.share(600.0, 3);
    assert!((active_least..widest).contains(&even), "{even}");
    // Past an even split the active tab keeps its least first.
    let squeezed = FIT.share(500.0, 8);
    assert!((narrowest..active_least).contains(&squeezed), "{squeezed}");
    assert_eq!(FIT.share(100.0, 20), narrowest);
    assert!(FIT.crowded(300.0, 20) && !FIT.crowded(900.0, 4));
}
