//! The chrome's layout table: component widths and heights the token
//! schema has no role for (`layout` names only the phone column and
//! the reader measure). One table, owned here, so a size is decided once
//! and views never write geometry as a literal or as a multiple of a
//! spacing step.

use phux_design::tokens::{LAYOUT, SPACE};

/// How large the overlays are.
pub(crate) struct Overlay {
    /// The command palette; the phone column's measure.
    pub(crate) palette_w: f32,
    /// Where the palette sits under the window's top: clear of the title
    /// bar, or in a short window this share of its height, never closer
    /// than the least.
    pub(crate) palette_top: f32,
    pub(crate) palette_top_share: f32,
    pub(crate) palette_top_min: f32,
    /// The palette's field row: room for the query, a chip and a count.
    pub(crate) palette_header_h: f32,
    /// The path picker, narrower, at the cursor.
    pub(crate) paths_w: f32,
    pub(crate) paths_h: f32,
    /// How tall the palette's rows may grow before they scroll: the list's
    /// insets and eleven and a half 32 pt rows, the half saying more lies
    /// below.
    pub(crate) palette_rows_h: f32,
    pub(crate) paths_rows_h: f32,
    /// The inbox drawer.
    pub(crate) drawer_w: f32,
    /// A focused pane's ask card.
    pub(crate) ask_w: f32,
    /// The narrowest pane an ask card still fits.
    pub(crate) ask_min_w: f32,
    pub(crate) find_w: f32,
    pub(crate) find_min_w: f32,
    pub(crate) toast_w: f32,
    /// The share of the window the popup takes, each way.
    pub(crate) popup_share: f32,
    pub(crate) news_w: f32,
    /// The chief's composer field: room for a dictated paragraph.
    pub(crate) chief_field_h: f32,
    /// The most of the window a held link takes before it scrolls.
    pub(crate) link_h: f32,
}

pub(crate) const OVERLAY: Overlay = Overlay {
    palette_w: LAYOUT.content_max.phone_column,
    palette_top: 96.0,
    palette_top_share: 0.18,
    palette_top_min: SPACE.s6,
    palette_header_h: SPACE.s12 + SPACE.s1,
    paths_w: 460.0,
    paths_h: 340.0,
    palette_rows_h: 2.0 * SPACE.s2 + 11.5 * SPACE.s8,
    paths_rows_h: 240.0,
    drawer_w: 400.0,
    ask_w: 340.0,
    ask_min_w: 160.0,
    find_w: 340.0,
    find_min_w: 160.0,
    toast_w: 420.0,
    popup_share: 0.8,
    news_w: 440.0,
    chief_field_h: 96.0,
    link_h: 160.0,
};

/// The tallest a sheet grows before its body scrolls.
pub(crate) const SHEET_MAX_H: f32 = 620.0;

/// The changes sheet.
pub(crate) struct Changes {
    pub(crate) width: f32,
    pub(crate) list_w: f32,
    /// The files and the diff each scroll inside this.
    pub(crate) body_h: f32,
}

pub(crate) const CHANGES: Changes = Changes {
    width: 940.0,
    list_w: 290.0,
    body_h: 440.0,
};

/// The pairing panel.
pub(crate) struct Pair {
    /// The QR plate's side: large enough for a phone camera at arm's length.
    pub(crate) qr: f32,
    /// The narrowest the code's copy column gets before it wraps under the QR.
    pub(crate) side_min: f32,
}

pub(crate) const PAIR: Pair = Pair {
    qr: 200.0,
    side_min: 180.0,
};

/// The machines sheet.
pub(crate) struct Machines {
    pub(crate) sheet_w: f32,
    /// Where the sheet sits when the window has room.
    pub(crate) sheet_top: f32,
    /// What a row's swatches and checklist indent by: past its colour dot.
    pub(crate) nested: f32,
}

pub(crate) const MACHINES: Machines = Machines {
    sheet_w: 420.0,
    sheet_top: OVERLAY.palette_top,
    nested: 40.0,
};

/// Settings (840×600 with a rail).
pub(crate) struct Settings {
    pub(crate) sheet_w: f32,
    pub(crate) sheet_h: f32,
    pub(crate) rail_w: f32,
    /// Where a phone row's title wraps above its value.
    pub(crate) phone_title_min: f32,
    pub(crate) phone_value_max: f32,
}

pub(crate) const SETTINGS: Settings = Settings {
    sheet_w: 840.0,
    sheet_h: 600.0,
    rail_w: 190.0,
    phone_title_min: 100.0,
    phone_value_max: 140.0,
};

/// Widths a Settings link row's value keeps.
pub(crate) struct SettingsLink {
    pub(crate) value_max: f32,
    pub(crate) phone_title_min: f32,
    pub(crate) phone_value_max: f32,
}

pub(crate) const SETTINGS_LINK: SettingsLink = SettingsLink {
    value_max: 120.0,
    phone_title_min: 80.0,
    phone_value_max: 140.0,
};

/// The shortcut recorder: room for a chord's caps.
pub(crate) struct Recorder {
    pub(crate) recorder_w: f32,
}

pub(crate) const RECORDER: Recorder = Recorder { recorder_w: 150.0 };

/// The update's corner card.
pub(crate) struct Updates {
    pub(crate) card_w: f32,
}

pub(crate) const UPDATES: Updates = Updates { card_w: 320.0 };

/// The tab strip, in points on the 4 point grid.
pub(crate) struct Tabs {
    /// The widest a tab grows.
    pub(crate) widest: f32,
    /// The active tab stays readable however many crowd it.
    pub(crate) active_least: f32,
    /// Narrower than this, a resting tab is its name alone.
    pub(crate) roomy: f32,
    /// The rename dialog.
    pub(crate) rename_w: f32,
}

pub(crate) const TABS: Tabs = Tabs {
    widest: SPACE.s4 * 13.0,
    active_least: SPACE.s4 * 8.0,
    roomy: SPACE.s4 * 8.0 + SPACE.s2,
    rename_w: SPACE.s10 * 9.0,
};

/// The status surfaces: placeholders, notices and banners.
pub(crate) struct Status {
    /// An indeterminate wait bar's width.
    pub(crate) progress_w: f32,
    /// A placeholder's measure: its title, words and actions.
    pub(crate) placeholder_w: f32,
    /// The reconnect banner, and the same with an update in it.
    pub(crate) banner_w: f32,
    pub(crate) update_banner_w: f32,
    /// A phone's message measure: comfortable lines on a 393-point screen,
    /// the whole width less margins on a 320-point one.
    pub(crate) phone_measure: f32,
}

pub(crate) const STATUS: Status = Status {
    progress_w: SPACE.s16 * 3.0,
    placeholder_w: SPACE.s16 * 8.0,
    banner_w: SPACE.s16 * 7.0,
    update_banner_w: SPACE.s16 * 9.0,
    phone_measure: SPACE.s10 * 7.5,
};

/// The phone's own components.
pub(crate) struct Phone {
    /// A terminal tab's pill: two of the grid's largest steps, so a long
    /// name truncates.
    pub(crate) tab_max_w: f32,
    /// The minimap: a phone screen's 3:2 thumbnail on the 4-point grid.
    pub(crate) minimap: (f32, f32),
    /// The text scale from which a page's title sits on its own line under
    /// Done: the iOS accessibility sizes (`AccessibilityM` and up).
    pub(crate) stacked_title_scale: f32,
    /// A long-press menu: about a system context menu's width, narrower on
    /// a small phone's column.
    pub(crate) menu_w: f32,
}

pub(crate) const PHONE: Phone = Phone {
    tab_max_w: SPACE.s16 * 2.0,
    minimap: (SPACE.s12 * 2.0, SPACE.s16),
    stacked_title_scale: 1.4,
    menu_w: SPACE.s16 * 4.0,
};
