use super::*;

fn file_text(page: &SettingsOverlay) -> String {
    std::fs::read_to_string(&page.path).unwrap_or_default()
}

fn mouse(action: MouseAction, button: MouseButton, (x, y): (u16, u16)) -> MouseEvent {
    MouseEvent {
        action,
        button,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(y),
    }
}

/// A left press and release on `at`: one click.
fn click(page: &mut SettingsOverlay, at: (u16, u16)) -> OverlayCommand {
    page.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, at));
    page.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, at))
}

/// The cell where `needle` first paints at 100x30, searching rows from
/// `from_row`.
fn find(page: &SettingsOverlay, needle: &str, from_row: usize) -> (u16, u16) {
    let text = render_text(page, 100, 30);
    text.lines()
        .enumerate()
        .skip(from_row)
        .find_map(|(y, line)| {
            let at = line.find(needle)?;
            let x = line[..at].chars().count();
            Some((u16::try_from(x).ok()?, u16::try_from(y).ok()?))
        })
        .unwrap_or_else(|| panic!("`{needle}` is not painted:\n{text}"))
}

/// The tab row at 100x30: under the query line.
const TAB_ROW: usize = 6;

#[test]
fn clicking_a_tab_shows_its_section_and_a_row_selects_it() {
    let (_dir, mut page) = page_over("");
    let sidebar = find(&page, "Sidebar", TAB_ROW);
    assert_eq!(
        usize::from(sidebar.1),
        TAB_ROW,
        "the tabs sit under the query"
    );
    assert_eq!(click(&mut page, sidebar), OverlayCommand::Stay);
    assert_eq!(SettingSection::ALL[page.section], SettingSection::Sidebar);
    let position = find(&page, "position", TAB_ROW + 1);
    assert_eq!(click(&mut page, position), OverlayCommand::Stay);
    assert_eq!(
        page.selected_spec().map(|s| s.key),
        Some("sidebar.position")
    );
    assert_eq!(file_text(&page), "", "selecting writes nothing");
}

#[test]
fn clicking_a_tab_while_filtering_clears_the_query() {
    let (_dir, mut page) = page_over("");
    type_text(&mut page, "width");
    let sidebar = find(&page, "Sidebar", TAB_ROW);
    click(&mut page, sidebar);
    assert!(page.query.is_empty());
    assert_eq!(SettingSection::ALL[page.section], SettingSection::Sidebar);
}

#[test]
fn clicking_a_checkbox_toggles_it_and_writes_the_file() {
    let (dir, mut page) = page_over("");
    page.focus("sidebar.width");
    let checkbox = find(&page, "\u{25a0} true", TAB_ROW + 1);
    assert_eq!(click(&mut page, checkbox), OverlayCommand::ReloadConfig);
    assert!(file(&dir).contains("enabled = false"), "{}", file(&dir));
    assert_eq!(
        page.selected_spec().map(|s| s.key),
        Some("sidebar.enabled"),
        "the press selects the row it lands on"
    );
    let unchecked = find(&page, "\u{25a1} false", TAB_ROW + 1);
    assert_eq!(unchecked, checkbox, "the box flips in place");
    // Space toggles it back, as Enter does.
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Space, None)),
        OverlayCommand::ReloadConfig
    );
    assert!(file(&dir).contains("enabled = true"), "{}", file(&dir));
}

#[test]
fn clicking_a_cycle_steps_it_forward() {
    let (dir, mut page) = page_over("");
    page.focus("sidebar.position");
    let cycle = find(&page, " left ", TAB_ROW + 1);
    let at = (cycle.0 + 1, cycle.1);
    assert_eq!(click(&mut page, at), OverlayCommand::ReloadConfig);
    assert!(
        file(&dir).contains("position = \"right\""),
        "{}",
        file(&dir)
    );
}

/// Layout stability: selecting rows and toggling a value moves nothing
/// else — not the tabs, not the other rows, not the box.
#[test]
fn selection_and_toggles_never_move_the_tabs_or_rows() {
    let (_dir, mut page) = page_over("");
    page.focus("sidebar.width");
    let area = Rect::new(0, 0, 100, 30);
    let bounds = page.bounds(area);
    let marks = |page: &SettingsOverlay| {
        (
            find(page, "Chrome", TAB_ROW),
            find(page, "Notifications", TAB_ROW),
            find(page, "position", TAB_ROW + 1),
            find(page, "hosts-refresh-secs", TAB_ROW + 1),
        )
    };
    let before = marks(&page);
    page.handle_key(&press(PhysicalKey::ArrowDown, None));
    assert_eq!(marks(&page), before);
    let checkbox = find(&page, "\u{25a0} true", TAB_ROW + 1);
    click(&mut page, checkbox);
    assert_eq!(
        marks(&page),
        before,
        "the section's override dot has its cell"
    );
    assert_eq!(page.bounds(area), bounds);
}

#[test]
fn the_value_editor_is_a_line_editor() {
    let (dir, mut page) = page_over("");
    page.focus("sidebar.width");
    page.handle_key(&press(PhysicalKey::Enter, None));
    // The cursor walks back into the text, and typing lands there.
    page.handle_key(&press(PhysicalKey::ArrowLeft, None));
    type_text(&mut page, "1");
    assert_eq!(page.editor_text(), Some("210"));
    page.handle_paste("x\ny");
    assert_eq!(page.editor_text(), Some("21x y0"), "a paste is one line");
    // While it is open the list holds still under it.
    page.handle_key(&press(PhysicalKey::ArrowDown, None));
    assert_eq!(page.selected_spec().map(|s| s.key), Some("sidebar.width"));
    page.handle_key(&ctrl(PhysicalKey::U));
    type_text(&mut page, "33");
    assert_eq!(
        page.handle_key(&press(PhysicalKey::Enter, None)),
        OverlayCommand::ReloadConfig
    );
    assert!(file(&dir).contains("width = 33"), "{}", file(&dir));
}

#[test]
fn render_roomy_layout_is_stable() {
    let (_dir, mut page) = page_over("[sidebar]\nwidth = 40\n");
    page.focus("sidebar.width");
    insta::assert_snapshot!(render_text(&page, 100, 30));
}

/// The selected row carries the selection colors, the active section's
/// tab is the filled one, and the selected row's checkbox has ratcn's
/// focus (its fill differs from the row's).
#[test]
fn the_selection_paints_the_row_the_tab_and_the_focused_control() {
    let (_dir, mut page) = page_over("");
    page.focus("sidebar.enabled");
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    page.render(area, &mut buf);
    let theme = Theme::default();
    let (x, y) = find(&page, "enabled", TAB_ROW + 1);
    assert_eq!(buf[(x, y)].bg, theme.selection_bg, "the selected row");
    assert_ne!(buf[(x, y + 1)].bg, theme.selection_bg, "the next row");
    let (tx, ty) = find(&page, "Sidebar", TAB_ROW);
    assert_eq!(buf[(tx, ty)].bg, theme.selection_bg, "the active tab");
    let (cx, cy) = find(&page, "\u{25a0} true", TAB_ROW + 1);
    assert_eq!(cy, y);
    assert_ne!(buf[(cx, cy)].bg, theme.selection_bg, "the focused box");
}

/// More sections than fit the tab row: the selected last section still
/// paints, so it can be seen and clicked.
#[test]
fn the_tab_row_keeps_the_selected_section_in_view() {
    let (_dir, mut page) = page_over("");
    page.focus("limits.metadata-value-bytes");
    let text = render_text(&page, 100, 30);
    let tabs = text.lines().nth(TAB_ROW).unwrap_or_default();
    assert!(tabs.contains("Limits"), "{text}");
    assert!(!tabs.contains("Defaults"), "{text}");
}

#[test]
fn render_compact_layout_is_stable() {
    let (_dir, mut page) = page_over("");
    page.focus("keybindings.which-key");
    // 60x16 is starved on both axes at the shipped breakpoints: full
    // bleed, single column, the short detail panel. The selection is the
    // same row the two-column layout would show, header or not.
    let text = render_text(&page, 60, 16);
    assert!(text.contains("keybindings.which-key  bool"), "{text}");
    insta::assert_snapshot!(text);
}
