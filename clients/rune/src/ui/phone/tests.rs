use super::*;

fn contrast(a: Color, b: Color) -> f32 {
    crate::theme::contrast(a, b)
}

/// A desktop-hosted phone with a terminal on screen and `tabs` tabs.
fn phone_desk(tabs: u8) -> Desk {
    let mut desk = crate::test_support::settings_window().with_phone(true);
    desk.settings_open = false;
    desk.connection.tabs.list = (0..tabs)
        .map(|i| crate::desk::Tab {
            locked: false,
            id: [i; 16],
            name: format!("tab {i}"),
            resources: Vec::new(),
            active: i == 0,
        })
        .collect();
    desk
}

fn runtime(desk: Desk, size: (f32, f32)) -> rune::Runtime<Desk> {
    let mut rt = rune::Runtime::new(desk, crate::ui::view, size, rune::Env::new(false, 1.0));
    for frame in 0..3 {
        rt.frame(f64::from(frame));
    }
    rt
}

/// Every node of the accessibility tree, flattened.
fn nodes(tree: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    out.push(tree.clone());
    for child in tree["children"].as_array().into_iter().flatten() {
        nodes(child, out);
    }
}

#[test]
fn controls_take_their_ground_not_the_os_appearance() {
    let desk = phone_desk(1);
    for ground in [Color::BLACK, Color::WHITE] {
        let t = Tokens::for_phone(&desk, Tokens::dark_ground(ground), None, Scale::GridCapped);
        let text = t.roles.text.text.paint();
        assert_ne!(crate::theme::on_fill(text), crate::theme::on_fill(ground));
        assert!(contrast(text, ground) >= 4.5);
        // §1.10's state ink reads on the control surface, too.
        assert!(contrast(text, t.roles.layer.raised.paint()) >= 4.5);
        assert!(contrast(t.roles.text.dim.paint(), t.roles.layer.raised.paint()) >= 4.5);
    }
}

#[test]
fn a_phone_takes_its_sheets_from_the_theme_or_the_ground_never_the_system() {
    let mut desk = crate::test_support::settings_window().with_phone(true);
    desk.settings.theme = None;
    desk.settings_open = false;
    // No terminal yet: the black one starts on, so a light-mode phone
    // does not open on a white page between a dark launch and terminal.
    assert_eq!(terminal_ground(&desk), Color::BLACK);
    assert_eq!(edge_color(&desk), Color::BLACK);
    let sheets = page_look(&desk);
    assert_eq!(
        crate::theme::on_fill(sheets.roles.layer.raised.paint()),
        Color::WHITE
    );
    let light = crate::theme::all().find(|theme| !theme.dark);
    desk.settings.theme = light;
    let sheets = page_look(&desk);
    assert_eq!(
        crate::theme::on_fill(sheets.roles.layer.raised.paint()),
        Color::BLACK
    );
    desk.open_phone_sessions();
    assert_eq!(
        edge_color(&desk),
        sheets.roles.layer.ground.paint(),
        "UIKit continues the navigation page's theme"
    );
}

#[test]
fn the_active_tab_is_brought_whole_into_the_strip() {
    let mut desk = crate::test_support::settings_window().with_phone(true);
    desk.settings_open = false;
    desk.connection.tabs.list = (0..6u8)
        .map(|i| crate::desk::Tab {
            locked: false,
            id: [i; 16],
            name: format!("a tab with a long name {i}"),
            resources: Vec::new(),
            active: i == 5,
        })
        .collect();
    let mut rt = rune::Runtime::new(
        desk,
        crate::ui::view,
        (393.0, 852.0),
        rune::Env::new(false, 1.0),
    );
    rt.frame(0.0);
    let strip = rt.presented("phone-tabs").expect("the strip").0;
    let before = rt.presented("phone-tab-5").expect("the last tab").0;
    assert!(
        before[0] + before[2] > strip[0] + strip[2],
        "six tabs overflow the strip"
    );
    rt.update(|d, tx| d.reveal_active_tab(tx));
    for frame in 1..120 {
        rt.frame(f64::from(frame) / 60.0);
    }
    let tab = rt.presented("phone-tab-5").expect("the last tab").0;
    assert!(
        tab[0] >= strip[0] && tab[0] + tab[2] <= strip[0] + strip[2] + 0.5,
        "the active tab spans {}..{}, the strip {}..{}",
        tab[0],
        tab[0] + tab[2],
        strip[0],
        strip[0] + strip[2]
    );
}

/// §1.6 and audit row 29: every control the phone draws is a 44-point
/// target in the accessibility tree. The drawn keyboard's letters keep the
/// system keyboard's width; they are 44 tall.
#[test]
fn every_phone_control_is_a_touch_target() {
    let mut desk = phone_desk(3);
    desk.phone.as_mut().expect("a phone").keyboard = true;
    let rt = runtime(desk, (320.0, 568.0));
    let mut all = Vec::new();
    nodes(&rt.accessibility_json(), &mut all);
    let mut checked = 0;
    for node in &all {
        let Some(key) = node["key"].as_str() else {
            continue;
        };
        let pressable = node["actions"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a == "click"));
        if !key.starts_with("phone-") || !pressable {
            continue;
        }
        let b = node["bounds"].as_array().expect("bounds");
        let (w, h) = (b[2].as_f64().unwrap(), b[3].as_f64().unwrap());
        assert!(h >= 44.0, "{key} is {h} tall");
        if !key.starts_with("phone-cap-") {
            assert!(w >= 44.0, "{key} is {w} wide");
        }
        checked += 1;
    }
    for key in [
        "phone-back",
        "phone-place",
        "phone-news",
        "phone-more",
        "phone-tab-0",
        "phone-tab-new",
        "phone-command",
        "phone-key-Escape",
        "phone-pad",
        "phone-keyboard-toggle",
        "phone-cap-113",
    ] {
        assert!(
            all.iter().any(|n| n["key"] == key),
            "{key} is in the accessibility tree"
        );
    }
    assert!(checked >= 20, "only {checked} controls");
}

/// Status is colour, mark and word: a tab with a terminal that needs you
/// says so to a screen reader, not only with a mark.
#[test]
fn a_waiting_tab_names_its_state() {
    let mut desk = phone_desk(2);
    let id = phux_protocol::ids::ResourceId::local(7);
    desk.connection.tabs.list[1].resources = vec![id.clone()];
    desk.activities.apply(
        &phux_client_runtime::control::Event::AgentMetadata {
            terminal_id: id,
            value: Some(br#"{"name":"claude","kind":"claude","state":"blocked"}"#.to_vec()),
        },
        &[],
    );
    let rt = runtime(desk, (393.0, 852.0));
    let mut all = Vec::new();
    nodes(&rt.accessibility_json(), &mut all);
    let name = |key: &str| {
        all.iter()
            .find(|n| n["key"] == key)
            .and_then(|n| n["name"].as_str().map(str::to_owned))
    };
    assert_eq!(name("phone-tab-0").as_deref(), Some("Tab 1"));
    assert_eq!(name("phone-tab-1").as_deref(), Some("Tab 2, needs you"));
}

/// The phone paints no glass of its own outside the session dock (§1.9),
/// and Reduce Transparency from the host takes even that away.
#[test]
fn reduce_transparency_from_the_host_drops_the_dock_glass() {
    let mut rt = runtime(phone_desk(2), (320.0, 568.0));
    let blurs = |rt: &rune::Runtime<Desk>| {
        rt.instances
            .iter()
            .filter(|i| i.params[1] == rune::render::KIND_BACKDROP_BLUR)
            .count()
    };
    assert_eq!(blurs(&rt), 0, "the terminal's chrome is opaque");
    rt.update(|d, tx| {
        d.open_phone_sessions();
        tx.instant();
    });
    rt.frame(4.0);
    rt.frame(5.0);
    assert_eq!(blurs(&rt), 1, "the dock is the phone's one glass");
    let before = page_look(&rt.state).version;
    rt.update(|d, tx| {
        assert!(d.set_accessibility(true, "UICTContentSizeCategoryL"));
        tx.instant();
    });
    rt.frame(6.0);
    rt.frame(7.0);
    let after = page_look(&rt.state).version;
    assert_ne!(before, after, "memos see the change");
    assert_eq!(blurs(&rt), 0, "the dock falls back to its opaque layer");
}

/// The host's text size reaches the phone's type: pages follow it freely,
/// the chrome beside the grid stops at its cap.
#[test]
fn the_reported_text_size_scales_the_phone_type() {
    let mut desk = phone_desk(2);
    let base = ground_look(&desk).ty.label.size;
    assert!(desk.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL"));
    let chrome = ground_look(&desk).ty.label.size;
    let page = page_look(&desk).ty.label.size;
    let cap = crate::desk::accessibility::GRID_CAP;
    assert!((chrome - base * cap).abs() < 0.01, "{chrome}");
    assert!(page > chrome);
}
