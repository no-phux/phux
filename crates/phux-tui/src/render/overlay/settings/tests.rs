use super::*;

use crate::test_util::press_text as press;

fn ctrl(key: PhysicalKey) -> KeyEvent {
    let mut ev = press(key, None);
    ev.mods = ModSet::CTRL;
    ev
}

fn type_text(page: &mut SettingsOverlay, text: &str) {
    for ch in text.chars() {
        page.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
    }
}

/// A page over a fresh temp config holding `toml`.
fn page_over(toml: &str) -> (tempfile::TempDir, SettingsOverlay) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, toml).expect("write config");
    let page = SettingsOverlay::open(path, &Theme::default())
        .with_display_path("~/.config/phux/config.toml");
    (dir, page)
}

/// A page whose config `extends` a `team.toml` layer setting
/// `sidebar.width = 50`, the user file holding `extra` after it.
fn layered(extra: &str) -> (tempfile::TempDir, SettingsOverlay) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("team.toml"), "[sidebar]\nwidth = 50\n").expect("layer");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, format!("extends = [\"team.toml\"]\n{extra}")).expect("config");
    let mut page = SettingsOverlay::open(path, &Theme::default());
    page.focus("sidebar.width");
    (dir, page)
}

/// Open the editor on the selected row, replace its text, and commit.
fn edit(page: &mut SettingsOverlay, text: &str) -> OverlayCommand {
    page.handle_key(&press(PhysicalKey::Enter, None));
    page.handle_key(&ctrl(PhysicalKey::U));
    type_text(page, text);
    page.handle_key(&press(PhysicalKey::Enter, None))
}

fn file(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("config.toml")).expect("read config")
}

use crate::test_util::rendered as render_text;

/// The bottom border row of the rendered modal.
fn bottom_border(page: &SettingsOverlay, cols: u16) -> String {
    render_text(page, cols, 24)
        .lines()
        .rfind(|l| l.contains('\u{2570}'))
        .expect("bottom border")
        .to_owned()
}

#[test]
fn legend_tiers_shrink_with_the_box_and_keep_a_gap() {
    let (_dir, page) = page_over("");
    let wide = bottom_border(&page, 120);
    assert!(wide.contains(SETTINGS_KEYS[0]), "{wide}");

    let mid = bottom_border(&page, 61);
    assert!(!mid.contains(SETTINGS_KEYS[0]), "{mid}");
    assert!(
        mid.contains("\u{23ce} edit") && mid.contains("esc"),
        "{mid}"
    );
    let status_end = mid.find("24").expect("status") + 2;
    let keys_start = mid.find('\u{23ce}').expect("legend");
    let gap = mid[status_end..keys_start].chars().count();
    assert!(gap >= 3, "gap {gap} in {mid}");
}

#[test]
fn settings_legend_picks_the_longest_tier_that_fits() {
    assert_eq!(settings_legend(120, "1 of 20"), SETTINGS_KEYS[0]);
    assert_eq!(settings_legend(60, "1 of 20"), SETTINGS_KEYS[1]);
    assert_eq!(settings_legend(30, "1 of 20"), SETTINGS_KEYS[2]);
    assert_eq!(settings_legend(10, "1 of 20"), SETTINGS_KEYS[2]);
}

#[test]
fn opens_on_the_first_section_with_the_schema_and_theme_rows() {
    let (_dir, page) = page_over("");
    assert!(page.snapshot.is_ok());
    assert_eq!(page.section, 0);
    let rows = page.visible_specs();
    assert!(rows.iter().all(|s| s.section == SettingSection::Defaults));
    assert!(
        page.specs.iter().any(|s| s.key == "theme.accent"),
        "theme slots are rows"
    );
    assert!(page.specs.iter().any(|s| s.key == "sidebar.width"));

    let (_dir, page) = page_over("[sidebar]\nwidth = 40\nenabled = false\n");
    assert_eq!(page.overridden_in(SettingSection::Sidebar), 2);
    assert_eq!(page.overridden_in(SettingSection::Chrome), 0);
}

#[test]
fn toggling_a_bool_writes_the_file_and_requests_a_reload() {
    let (dir, mut page) = page_over("# my config\n");
    page.focus("sidebar.enabled");
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Enter, None)),
        OverlayCommand::ReloadConfig
    );
    let text = file(&dir);
    assert!(
        text.starts_with("# my config\n"),
        "comments survive: {text}"
    );
    assert!(text.contains("[sidebar]\nenabled = false"), "{text}");
    assert!(
        matches!(page.status, Some(Status::Saved(_))),
        "{:?}",
        page.status
    );
    assert!(matches!(
        page.origin_at("sidebar.enabled"),
        Some(LayerSource::User(_))
    ));
    // And back.
    page.handle_key(&press(PhysicalKey::Space, None));
    assert!(file(&dir).contains("enabled = true"));
}

#[test]
fn cycling_a_choice_steps_through_its_variants_and_wraps() {
    let (dir, mut page) = page_over("");
    page.focus("sidebar.position");
    assert_eq!(
        page.handle_key(&press(PhysicalKey::ArrowRight, None)),
        OverlayCommand::ReloadConfig
    );
    assert!(file(&dir).contains("position = \"right\""));
    page.handle_key(&press(PhysicalKey::ArrowRight, None));
    assert!(
        file(&dir).contains("position = \"left\""),
        "wraps: {}",
        file(&dir)
    );
    page.handle_key(&press(PhysicalKey::ArrowLeft, None));
    assert!(file(&dir).contains("position = \"right\""));
}

#[test]
fn arrows_step_an_integer_within_its_bounds() {
    let (dir, mut page) = page_over("[chrome]\ncompact-cols = 0\n");
    page.focus("chrome.compact-cols");
    assert_eq!(
        page.handle_key(&press(PhysicalKey::ArrowLeft, None)),
        OverlayCommand::Stay,
        "already at the lower bound"
    );
    page.handle_key(&press(PhysicalKey::ArrowRight, None));
    assert!(file(&dir).contains("compact-cols = 1"), "{}", file(&dir));
}

#[test]
fn editor_commits_a_valid_integer_and_refuses_an_invalid_one() {
    let (dir, mut page) = page_over("");
    page.focus("sidebar.width");
    page.handle_key(&press(PhysicalKey::Enter, None));
    assert_eq!(
        page.editor_text(),
        Some("20"),
        "the editor opens on the effective value (the shipped 20 columns)"
    );
    page.handle_key(&press(PhysicalKey::Escape, None));
    assert_eq!(edit(&mut page, "32"), OverlayCommand::ReloadConfig);
    assert!(page.editor.is_none());
    assert!(file(&dir).contains("width = 32"), "{}", file(&dir));

    assert_eq!(edit(&mut page, "wide"), OverlayCommand::Stay);
    assert!(
        matches!(&page.status, Some(Status::Refused(m)) if m.starts_with("exp")),
        "{:?}",
        page.status
    );
    assert!(
        page.editor.is_some(),
        "the editor stays open to fix the text"
    );
    assert!(file(&dir).contains("width = 32"), "file untouched");
    page.handle_key(&press(PhysicalKey::Escape, None));
    assert!(page.editor.is_none());
}

#[test]
fn a_value_the_checker_rejects_is_refused_with_its_reason() {
    let (dir, mut page) = page_over("");
    page.focus("keybindings.prefix");
    assert_eq!(edit(&mut page, "not a chord"), OverlayCommand::Stay);
    match &page.status {
        Some(Status::Refused(msg)) => assert!(msg.contains("keybindings.prefix"), "{msg}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(!file(&dir).contains("prefix"), "nothing written");
}

#[test]
fn reset_removes_the_override_and_undo_restores_it() {
    let (dir, mut page) = page_over("[sidebar]\nwidth = 40 # wide\n");
    page.focus("sidebar.width");
    assert!(page.origin_at("sidebar.width").is_some());
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Delete, None)),
        OverlayCommand::ReloadConfig
    );
    assert!(!file(&dir).contains("width"), "{}", file(&dir));
    assert!(page.origin_at("sidebar.width").is_none());
    assert_eq!(
        page.value_at("sidebar.width"),
        Some(toml::Value::Integer(20)),
        "the shipped default (20 columns) shows through"
    );

    assert_eq!(
        page.handle_key(&ctrl(PhysicalKey::Z)),
        OverlayCommand::ReloadConfig
    );
    assert!(
        file(&dir).contains("width = 40"),
        "undo restores: {}",
        file(&dir)
    );

    // A second undo has nothing left.
    assert_eq!(page.handle_key(&ctrl(PhysicalKey::Z)), OverlayCommand::Stay);
    assert!(matches!(page.status, Some(Status::Note(_))));

    // Resetting a key already at its default is a note, not a write.
    let (dir, mut page) = page_over("");
    page.focus("sidebar.width");
    assert_eq!(page.handle_key(&ctrl(PhysicalKey::R)), OverlayCommand::Stay);
    assert!(matches!(page.status, Some(Status::Note(_))));
    assert_eq!(file(&dir), "");
}

#[test]
fn reset_is_refused_for_a_key_set_by_an_extends_layer() {
    let (dir, mut page) = layered("");
    assert_eq!(
        page.value_at("sidebar.width"),
        Some(toml::Value::Integer(50))
    );
    assert!(matches!(
        page.origin_at("sidebar.width"),
        Some(LayerSource::Extended(_))
    ));
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Delete, None)),
        OverlayCommand::Stay
    );
    match &page.status {
        Some(Status::Refused(msg)) => assert!(msg.contains("team.toml"), "{msg}"),
        other => panic!("expected a refusal naming the layer, got {other:?}"),
    }
    let (badge, _) = page.badge("sidebar.width");
    assert_eq!(badge, "team");
    // Overriding it from the page is still allowed.
    page.handle_key(&press(PhysicalKey::ArrowRight, None));
    assert!(file(&dir).contains("width = 51"), "{}", file(&dir));

    // A reset that uncovers the layer's value says so.
    let (_dir, mut page) = layered("[sidebar]\nwidth = 40\n");
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Delete, None)),
        OverlayCommand::ReloadConfig
    );
    match &page.status {
        Some(Status::Saved(msg)) => {
            assert!(msg.contains("team.toml") && msg.contains("50"), "{msg}");
        }
        other => panic!("expected a saved note naming the layer, got {other:?}"),
    }
}

#[test]
fn theme_slots_edit_colors_with_validation_and_a_preview() {
    let (dir, mut page) = page_over("");
    page.focus("theme.accent");
    assert_eq!(
        page.default_at(page.selected_spec().unwrap()),
        Some(toml::Value::String(crate::render::theme::color_to_string(
            Theme::default().accent
        )))
    );
    assert_eq!(edit(&mut page, "#ff0000"), OverlayCommand::ReloadConfig);
    assert!(
        file(&dir).contains("[theme]\naccent = \"#ff0000\""),
        "{}",
        file(&dir)
    );

    assert_eq!(edit(&mut page, "notacolor"), OverlayCommand::Stay);
    assert!(matches!(page.status, Some(Status::Refused(_))));
    assert!(
        file(&dir).contains("#ff0000"),
        "file untouched by the refusal"
    );
}

#[test]
fn argv_and_optional_kinds_round_trip_unset() {
    let (dir, mut page) = page_over("");
    page.focus("voice.transcriber");
    edit(&mut page, "curl -sf 'a b' \"c\"");
    assert!(
        file(&dir).contains("transcriber = [\"curl\", \"-sf\", \"a b\", \"c\"]"),
        "{}",
        file(&dir)
    );
    edit(&mut page, "unset");
    assert!(!file(&dir).contains("transcriber"), "{}", file(&dir));

    page.focus("experimental.predictive-echo");
    page.handle_key(&press(PhysicalKey::Space, None));
    assert!(file(&dir).contains("predictive-echo = true"));
    page.handle_key(&press(PhysicalKey::Space, None));
    assert!(file(&dir).contains("predictive-echo = false"));
    page.handle_key(&press(PhysicalKey::Space, None));
    assert!(!file(&dir).contains("predictive-echo"), "third step unsets");
}

#[test]
fn letters_never_write_the_file() {
    // `h`/`l` are letters here, not value steps.
    let (dir, mut page) = page_over("");
    page.focus("sidebar.enabled");
    for ch in ['h', 'l', 'j', 'k', ' '] {
        let key = match ch {
            'h' => PhysicalKey::H,
            'l' => PhysicalKey::L,
            'j' => PhysicalKey::J,
            'k' => PhysicalKey::K,
            _ => PhysicalKey::Space,
        };
        page.query = "x".to_owned();
        assert_eq!(
            page.handle_key(&press(key, Some(&ch.to_string()))),
            OverlayCommand::Stay
        );
        assert_eq!(file(&dir), "", "`{ch}` inside a query must not write");
    }
    page.query.clear();
    page.handle_key(&press(PhysicalKey::L, Some("l")));
    page.handle_key(&press(PhysicalKey::H, Some("h")));
    assert_eq!(file(&dir), "", "letters filter even with an empty query");
    assert_eq!(page.query, "lh");
}

#[test]
fn filtering_flattens_sections_and_escape_clears_before_closing() {
    let (_dir, mut page) = page_over("");
    type_text(&mut page, "which");
    let keys: Vec<&str> = page.visible_specs().iter().map(|s| s.key).collect();
    assert!(keys.contains(&"keybindings.which-key"), "{keys:?}");
    assert!(keys.contains(&"keybindings.which-key-delay-ms"), "{keys:?}");
    assert!(page.matches_in(SettingSection::Keybindings) >= 2);
    assert_eq!(page.matches_in(SettingSection::Voice), 0);
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Escape, None)),
        OverlayCommand::Stay
    );
    assert!(page.query.is_empty());
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Escape, None)),
        OverlayCommand::Dismiss
    );
}

#[test]
fn tab_steps_sections_and_shift_tab_steps_back() {
    let (_dir, mut page) = page_over("");
    page.handle_key(&press(PhysicalKey::Tab, None));
    assert_eq!(
        SettingSection::ALL[page.section],
        SettingSection::Keybindings
    );
    let mut back = press(PhysicalKey::Tab, None);
    back.mods = ModSet::SHIFT;
    page.handle_key(&back);
    assert_eq!(SettingSection::ALL[page.section], SettingSection::Defaults);
    page.handle_key(&back);
    assert_eq!(
        SettingSection::ALL[page.section],
        SettingSection::Limits,
        "wraps"
    );
}

#[test]
fn a_config_that_does_not_load_shows_the_error_instead_of_rows() {
    let (_dir, mut page) = page_over("this is [not toml");
    assert!(page.snapshot.is_err());
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Enter, None)),
        OverlayCommand::Stay
    );
    let text = render_text(&page, 90, 24);
    assert!(text.contains("config does not load"), "{text}");
}

mod ui;
