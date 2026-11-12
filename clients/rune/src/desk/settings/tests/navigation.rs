//! Settings navigation, persistence and search use the real Rune runtime.

use rune::{Key, KeyEvent, Mods};

use super::super::Settings;
use super::*;

fn settings_desk() -> Desk {
    let mut desk = Desk::new(&Resolved::offline("settings"));
    desk.prefs = Prefs::memory();
    desk.settings = Settings::load(&desk.prefs);
    desk.settings_open = true;
    desk
}

fn event(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        mods: Mods::default(),
    }
}

fn click(runtime: &mut Runtime<Desk>, key: &str) {
    assert!(
        runtime.accessibility_action(key, Action::Click, None),
        "{key}"
    );
    runtime.frame(0.0);
}

#[test]
fn category_navigation_search_clear_and_no_results_are_real_controls() {
    let mut runtime = runtime(settings_desk());
    assert!(runtime.find("settings-theme").is_some());
    assert!(runtime.find("set-copy-select").is_none());
    click(&mut runtime, "settings-terminal");
    assert!(runtime.find("set-copy-select").is_some());
    assert!(runtime.find("settings-theme").is_none());
    runtime.update(|d, _| d.settings_ui.query = rune::TextEdit::new("cLiPbOaRd"));
    runtime.frame(0.0);
    assert!(
        runtime.find("set-copy-select").is_some(),
        "descriptions are searched without case"
    );
    assert!(runtime.find("text-size").is_none());
    runtime.update(|d, _| d.settings_ui.query = rune::TextEdit::new("no-such-preference"));
    runtime.frame(0.0);
    assert!(runtime.find("settings-no-results").is_some());
    click(&mut runtime, "settings-clear");
    assert!(runtime.find("text-size").is_some());
    runtime.update(|d, _| d.settings_ui.query = rune::TextEdit::new("System"));
    runtime.frame(0.0);
    assert!(
        runtime.find("settings-theme").is_some(),
        "current values search across categories"
    );
}

#[test]
fn arrows_outside_a_control_never_change_the_theme() {
    let mut runtime = runtime(settings_desk());
    let before = runtime.state.settings.theme;
    for key in [Key::Left, Key::Right, Key::Up, Key::Down] {
        runtime.key(event(key));
    }
    assert_eq!(runtime.state.settings.theme, before);
}

#[test]
fn useful_search_aliases_find_real_controls_without_changing_preferences() {
    let mut runtime = runtime(settings_desk());
    let before = runtime.state.settings;
    let font_size = runtime.state.font_size;
    for (query, key) in [
        ("font", "text-size"),
        ("zoom", "text-size"),
        ("typography", "text-size"),
        ("colors", "settings-theme"),
        ("colours", "settings-theme"),
        ("clipboard", "set-copy-select"),
    ] {
        runtime.update(|d, _| d.settings_ui.query = rune::TextEdit::new(query));
        runtime.frame(0.0);
        assert!(runtime.find(key).is_some(), "{query} finds {key}");
    }
    if cfg!(target_os = "macos") {
        runtime.update(|d, _| d.settings_ui.query = rune::TextEdit::new("alt"));
        runtime.frame(0.0);
        assert!(runtime.find("set-option-meta").is_some());
    }
    assert_eq!(runtime.state.settings, before);
    assert_eq!(runtime.state.font_size, font_size);
}

#[test]
fn theme_palette_returns_to_settings_on_accept_and_cancel() {
    let mut runtime = runtime(settings_desk());
    let before = runtime.state.settings.theme;
    click(&mut runtime, "settings-theme");
    assert!(runtime.state.palette_open);
    assert!(!runtime.state.settings_open);
    runtime.update(|d, _| {
        d.palette_query = rune::TextEdit::new("Nord");
        d.palette_edited();
    });
    assert_eq!(runtime.state.settings.theme, crate::theme::find("nord"));
    runtime.update(|d, tx| {
        d.palette_key(&event(Key::Escape), tx);
    });
    assert_eq!(runtime.state.settings.theme, before);
    assert!(runtime.state.settings_open);
    runtime.frame(0.0);
    click(&mut runtime, "settings-theme");
    runtime.update(|d, tx| {
        d.palette_query = rune::TextEdit::new("Nord");
        d.palette_edited();
        d.palette_run(None, tx);
    });
    assert!(runtime.state.settings_open);
    assert_eq!(
        runtime.state.prefs.get::<String>("theme").as_deref(),
        Some("nord")
    );
    runtime.state.choose_theme(None);
    assert_eq!(
        runtime.state.prefs.get::<String>("theme").as_deref(),
        Some("")
    );
    assert_eq!(Settings::load(&runtime.state.prefs).theme, None);
    assert_eq!(
        Settings::load(&Prefs::memory()).theme,
        crate::theme::omarchy()
    );
}

#[test]
fn preferences_persist_immediately_and_font_limits_remain() {
    let mut desk = settings_desk();
    assert!(!desk.settings.confirm_close);
    desk.toggle_notifications();
    desk.toggle_option_as_meta();
    desk.toggle_copy_on_select();
    desk.toggle_confirm_close();
    assert_eq!(Settings::load(&desk.prefs), desk.settings);
    desk.zoom(Some(100.0));
    assert_eq!(desk.font_size, MAX_FONT_SIZE);
    assert_eq!(desk.prefs.get::<f32>("font_size"), Some(MAX_FONT_SIZE));
    desk.zoom(Some(-10.0));
    assert_eq!(desk.font_size, MIN_FONT_SIZE);
}

#[test]
fn soft_search_is_synchronous_and_cannot_outlive_its_field() {
    let mut desk = settings_desk().with_phone(true);
    desk.settings_category(Category::Terminal);
    desk.settings_ui.query.focused = true;
    let mut runtime = runtime(desk);
    runtime.update(|d, tx| d.settings_soft(&event(Key::Text("copy".into())), tx));
    assert_eq!(runtime.state.settings_ui.query.text, "copy");
    runtime.update(|d, tx| d.settings_soft(&event(Key::Backspace), tx));
    assert_eq!(runtime.state.settings_ui.query.text, "cop");
    runtime.update(|d, tx| d.settings_soft(&event(Key::Escape), tx));
    assert!(runtime.state.settings_ui.query.text.is_empty());
    runtime.update(|d, tx| d.settings_soft(&event(Key::Enter), tx));
    assert!(!runtime.state.settings_search_focused());
    runtime.update(|d, tx| d.settings_soft(&event(Key::Text("must not arrive".into())), tx));
    assert!(runtime.state.settings_ui.query.text.is_empty());
    runtime.frame(0.0);
    runtime.key(event(Key::Escape));
    runtime.frame(0.0);
    assert!(runtime.state.settings_ui.category.is_none());
    assert!(runtime.state.settings_open);
    runtime.key(event(Key::Escape));
    runtime.frame(0.0);
    assert!(!runtime.state.settings_open);
}

#[test]
fn diagnostics_and_update_warnings_survive_unavailable_install_and_filtering() {
    let mut desk = settings_desk();
    desk.updater.message = Some("Signature check failed".into());
    desk.settings_ui.query = rune::TextEdit::new("diagnostics");
    let mut runtime = runtime(desk);
    assert!(runtime.state.updater.install.is_err());
    assert!(runtime.find("copy-diagnostics").is_some());
    let tree = runtime.accessibility_json();
    assert_eq!(find(&tree, "settings-update-warning")["role"], "alert");
    assert!(
        find(&tree, "settings-update-warning")["name"]
            .as_str()
            .unwrap()
            .contains("Signature")
    );
    runtime.update(|d, _| d.settings_ui.query = rune::TextEdit::new("no results"));
    runtime.frame(0.0);
    assert!(runtime.find("settings-no-results").is_some());
    assert!(runtime.find("settings-update-warning").is_some());
}

#[test]
fn installed_update_preferences_persist_and_protocol_warnings_survive_search() {
    let mut desk = settings_desk();
    desk.settings_category(Category::About);
    desk.updater.automatic = true;
    desk.updater.manage_phux = true;
    desk.updater.install = Ok("/not-an-installed-app".into());
    desk.updater.target = Some(("/not-a-cli".into(), "/tmp/not-a-server".into()));
    desk.updater.release = Some(crate::updates::Release {
        version: "999.0.0".into(),
        sha256: String::new(),
        size: 1,
        protocol: "999.0".into(),
    });
    let mut runtime = runtime(desk);
    assert!(runtime.find("set-update-auto").is_some());
    assert!(runtime.find("set-update-phux").is_some());
    click(&mut runtime, "set-update-auto");
    click(&mut runtime, "set-update-phux");
    assert_eq!(
        runtime.state.prefs.get::<bool>("update_automatic"),
        Some(false)
    );
    assert_eq!(runtime.state.prefs.get::<bool>("update_phux"), Some(false));
    runtime.update(|d, _| d.settings_ui.query = rune::TextEdit::new("clipboard"));
    runtime.frame(0.0);
    assert!(runtime.find("settings-update-warning").is_some());
    assert!(runtime.find("set-update-auto").is_none());
}

#[test]
fn config_file_keys_win_and_cannot_be_changed_in_the_app() {
    let mut desk = Desk::settings_test_fixture();
    desk.file = phux_config_core::RuneCfg {
        copy_on_select: Some(true),
        theme: Some(String::new()),
        ..Default::default()
    };
    desk.settings = Settings::load(&desk.prefs).over(&desk.file);
    assert!(desk.settings.copy_on_select);
    assert_eq!(desk.settings.theme, None);
    desk.toggle_copy_on_select();
    assert!(desk.settings.copy_on_select, "the file owns this switch");
    assert_eq!(desk.prefs.get::<bool>("copy_on_select"), None);
    // A key the file leaves alone is still the app's to remember.
    desk.toggle_confirm_close();
    assert_eq!(desk.prefs.get::<bool>("confirm_close"), Some(true));
}

/// A stray close cannot end an agent: its terminal asks first until the
/// setting is chosen, and turning it off by hand stops the asking.
#[test]
fn closing_an_agents_terminal_asks_until_the_setting_is_chosen() {
    use phux_client_core::layout::wire::HandleMap;
    use phux_client_runtime::control::{AgentSessionDescriptor, Topology};
    use phux_protocol::ids::ResourceId;

    let (agent, shell) = (ResourceId::local(1), ResourceId::local(2));
    let mut desk = settings_desk();
    desk.topology = Some(Topology {
        sessions: Vec::new(),
        resources: Vec::new(),
        panes: Vec::new(),
        agent_sessions: vec![AgentSessionDescriptor {
            id: ResourceId::local(9),
            parent: Some(agent.clone()),
            provider: None,
            native_id: None,
            state: None,
        }],
        documents: Vec::new(),
        satellite_sessions: Vec::new(),
        focused_session: 0,
        focused_pane: shell.clone(),
        authority: None,
        inbox: None,
        handles: HandleMap::new(),
    });
    assert!(desk.asks_to_close(&agent) && !desk.asks_to_close(&shell));
    desk.toggle_confirm_close();
    assert!(
        desk.asks_to_close(&agent) && desk.asks_to_close(&shell),
        "on"
    );
    desk.toggle_confirm_close();
    assert!(!desk.asks_to_close(&agent), "turned off by hand");
}

#[test]
fn the_general_palette_previews_a_highlighted_theme_and_puts_it_back() {
    let mut runtime = runtime(settings_desk());
    let before = runtime.state.settings.theme;
    let search = |d: &mut Desk, query: &str| {
        d.palette_query = rune::TextEdit::new(query);
        d.palette_edited();
    };
    runtime.update(|d, _| {
        d.open_palette(crate::palette::Mode::All);
        search(d, "Theme: Nord");
    });
    assert_eq!(runtime.state.settings.theme, crate::theme::find("nord"));
    // A row that is no theme shows the theme the palette opened with.
    runtime.update(|d, _| search(d, "New Terminal"));
    assert_eq!(runtime.state.settings.theme, before);
    runtime.update(|d, tx| {
        search(d, "Theme: Nord");
        d.palette_key(&event(Key::Escape), tx);
    });
    assert_eq!(runtime.state.settings.theme, before);
    runtime.update(|d, tx| {
        d.open_palette(crate::palette::Mode::All);
        search(d, "Theme: Nord");
        d.palette_run(None, tx);
    });
    assert_eq!(runtime.state.settings.theme, crate::theme::find("nord"));
}
