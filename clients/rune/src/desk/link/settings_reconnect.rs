//! A replaced connection must not replace window-local Settings or phone state.

use super::*;

#[test]
fn reconnect_keeps_settings_navigation_search_return_context_and_phone_form() {
    let mut desk = Desk::new(&Resolved::offline("settings reconnect")).with_phone(true);
    desk.settings_open = true;
    desk.settings_category(crate::desk::SettingsCategory::Developer);
    desk.settings_ui.query = rune::TextEdit::new("logs");
    desk.settings_ui.query.focused = true;
    desk.settings_ui.return_from_themes = true;
    desk.renew();
    assert!(desk.phone.is_some());
    assert!(desk.settings_open);
    assert_eq!(
        desk.settings_ui.category,
        Some(crate::desk::SettingsCategory::Developer)
    );
    assert_eq!(desk.settings_ui.query.text, "logs");
    assert!(desk.settings_search_focused());
    assert!(desk.settings_ui.return_from_themes);
}
