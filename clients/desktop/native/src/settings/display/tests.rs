//! Display preference defaults and persistence regressions.
use super::*;

#[test]
fn pane_glass_is_independent_and_preserves_the_saved_strength_across_styles() {
    assert!(
        parse_display(&json!({})).pane_glass,
        "old layouts keep their appearance"
    );
    for value in [json!(null), json!("false"), json!(0)] {
        assert!(parse_display(&json!({"paneGlass": value})).pane_glass);
    }
    for skin_id in ["glass", "classic"] {
        let prefs = DisplayPrefs {
            skin_id: skin_id.into(),
            pane_glass: false,
            terminal_opacity: 0.65,
            ..default_display()
        };
        assert_eq!(parse_display(&display_value(&prefs)), prefs);
        assert_eq!(prefs.painted_terminal_opacity(), 1.);
        assert_eq!(prefs.terminal_opacity, 0.65);
    }
}

#[test]
fn agent_fleet_is_opt_in_and_round_trips_without_coercion() {
    for value in [
        json!({}),
        json!({"agentFleet": "true"}),
        json!({"agentFleet": 1}),
        json!({"agentFleet": null}),
    ] {
        assert!(!parse_display(&value).agent_fleet);
    }
    let on = DisplayPrefs {
        agent_fleet: true,
        ..default_display()
    };
    assert_eq!(display_value(&on)["agentFleet"], true);
    assert_eq!(parse_display(&display_value(&on)), on);
}

#[test]
fn tab_placement_round_trips_without_changing_other_preferences() {
    for position in [TabPosition::Top, TabPosition::Bottom, TabPosition::Side] {
        let prefs = DisplayPrefs {
            tab_position: position,
            sidebar_visible: false,
            sidebar_width: 320.,
            font_size: 17.,
            ..default_display()
        };
        let value = display_value(&prefs);
        assert_eq!(value["tabPosition"], position.as_str());
        assert_eq!(parse_display(&value), prefs);
    }
}

#[test]
fn sidebar_grouping_round_trips_and_defaults_to_projects() {
    for grouping in [
        SidebarGrouping::Project,
        SidebarGrouping::Machine,
        SidebarGrouping::None,
    ] {
        let prefs = DisplayPrefs {
            sidebar_grouping: grouping,
            ..default_display()
        };
        let value = display_value(&prefs);
        assert_eq!(value["sidebarGrouping"], grouping.as_str());
        assert_eq!(parse_display(&value), prefs);
    }
    for value in [json!("host"), json!(3), json!(null)] {
        let parsed = parse_display(&json!({"sidebarGrouping": value, "fontSize": 15}));
        assert_eq!(parsed.sidebar_grouping, SidebarGrouping::Project);
        assert_eq!(parsed.font_size, 15.);
    }
}

#[test]
fn sidebar_host_filters_round_trip_without_changing_the_catalog_or_other_prefs() {
    let mut prefs = default_display();
    assert!(prefs.sidebar_shows_host(None));
    assert!(prefs.sidebar_shows_host(Some("mini")));
    prefs.set_sidebar_host_visible(None, false);
    prefs.set_sidebar_host_visible(Some("mini".into()), false);
    prefs.set_sidebar_host_visible(Some("mini".into()), false);
    assert_eq!(prefs.sidebar_hidden_hosts.len(), 2, "idempotent exclusions");
    assert_eq!(parse_display(&display_value(&prefs)), prefs);
    assert!(!prefs.sidebar_shows_host(None));
    assert!(!prefs.sidebar_shows_host(Some("mini")));
    assert!(
        prefs.sidebar_shows_host(Some("new-host")),
        "new hosts default visible"
    );
    prefs.set_sidebar_host_visible(None, true);
    assert!(prefs.sidebar_shows_host(None));
    assert!(!prefs.sidebar_shows_host(Some("mini")));
    assert!(parse_display(&json!({})).sidebar_hidden_hosts.is_empty());
    let parsed = parse_display(&json!({"sidebarHiddenHosts": [null, "mini", 3, true]}));
    assert_eq!(parsed.sidebar_hidden_hosts, [None, Some("mini".into())]);
}

#[test]
fn window_background_round_trips_and_defaults_to_blur() {
    for background in [WindowBackground::Blur, WindowBackground::Solid] {
        let prefs = DisplayPrefs {
            window_background: background,
            terminal_opacity: 0.8,
            ..default_display()
        };
        let value = display_value(&prefs);
        assert_eq!(value["windowBackground"], background.as_str());
        assert_eq!(parse_display(&value), prefs);
    }
    for value in [
        None,
        Some(json!("glass")),
        Some(json!(1)),
        Some(json!(null)),
    ] {
        let mut display = json!({"terminalOpacity": 0.7});
        if let Some(value) = value {
            display["windowBackground"] = value;
        }
        let parsed = parse_display(&display);
        assert_eq!(parsed.window_background, WindowBackground::Blur);
        assert_eq!(parsed.terminal_opacity, 0.7);
    }
}

#[test]
fn a_solid_window_paints_the_terminal_opaque_and_keeps_the_saved_opacity() {
    let blur = DisplayPrefs {
        terminal_opacity: 0.7,
        ..default_display()
    };
    assert_eq!(blur.painted_terminal_opacity(), 0.7);
    let solid = DisplayPrefs {
        window_background: WindowBackground::Solid,
        ..blur
    };
    assert_eq!(solid.painted_terminal_opacity(), 1.0);
    assert_eq!(parse_display(&display_value(&solid)).terminal_opacity, 0.7);
}

#[test]
fn old_or_unrecognized_tab_placement_preserves_the_rest_of_the_display() {
    for value in [
        None,
        Some(json!("unknown")),
        Some(json!(null)),
        Some(json!(42)),
    ] {
        let mut display = json!({"sidebarVisible": false, "fontSize": 18});
        if let Some(value) = value {
            display["tabPosition"] = value;
        }
        let parsed = parse_display(&display);
        assert_eq!(parsed.tab_position, TabPosition::Top);
        assert!(!parsed.sidebar_visible);
        assert_eq!(parsed.font_size, 18.);
    }
}

#[test]
fn the_laser_cursor_defaults_on_and_round_trips_off() {
    assert!(parse_display(&json!({})).cursor_laser);
    let off = DisplayPrefs {
        cursor_laser: false,
        ..default_display()
    };
    let value = display_value(&off);
    assert_eq!(value["cursorLaser"], false);
    assert_eq!(parse_display(&value), off);
}

#[test]
fn skin_and_plain_terminal_round_trip_and_unknown_skins_fall_back() {
    let parsed = parse_display(&json!({}));
    assert_eq!(parsed.skin_id, "glass");
    assert!(!parsed.plain);
    assert!(parsed.animations);
    let plain = DisplayPrefs {
        skin_id: "terminal".to_owned(),
        plain: true,
        animations: false,
        ..default_display()
    };
    let value = display_value(&plain);
    assert_eq!(value["skinId"], "terminal");
    assert_eq!(value["plainTerminal"], true);
    assert_eq!(value["animations"], false);
    assert_eq!(parse_display(&value), plain);
    assert_eq!(parse_display(&json!({"skinId": "neon"})).skin_id, "glass");
    assert!(!parse_display(&json!({"plainTerminal": "yes"})).plain);
    assert!(!parsed.native_agents);
    let native = DisplayPrefs {
        native_agents: true,
        ..default_display()
    };
    assert_eq!(display_value(&native)["nativeAgents"], true);
    assert_eq!(parse_display(&display_value(&native)), native);
    assert!(!parse_display(&json!({"nativeAgents": "yes"})).native_agents);
    assert!(parse_display(&json!({"animations": "no"})).animations);
}
