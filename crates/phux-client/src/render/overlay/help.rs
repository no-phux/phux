//! Help overlay (phux-5ke.4).
//!
//! Renders current keybindings as a centered modal. Dismisses on Esc
//! (or any key already bound to `show-help`, since "pressing the help
//! binding while help is up" is the universal "close it" gesture).
//!
//! A table taller than the modal is a *scroll viewport*, not a clip
//! (phux-9adu): arrows / `j` / `k` / `C-n` / `C-p` step a row,
//! `PageUp` / `PageDown` a screenful, `Home` / `End` jump to the ends,
//! and the wheel scrolls a detent — mirroring the palette's bindings
//! (phux-ep9s). An overflowing table paints a scrollbar in the right
//! border column. Because the body renders with wrapping on, the window
//! is counted in *wrapped display rows* at the current modal width
//! ([`Modal::wrapped_row_count`]), not logical lines — a chord row that
//! folds onto a second row consumes two rows of the window.
//!
//! Bindings are snapshotted at construction time — the overlay does not
//! re-read config while it's up. If the user reloads config while help
//! is open, they'll see the stale view; dismissing and re-opening picks
//! up the new bindings. This avoids the overlay holding any reference
//! into the live config, which keeps `Box<dyn RenderOverlay>` `'static`.

use std::cell::Cell;
use std::collections::BTreeMap;

use phux_config::keybind::{ResolvedAction, chord_str_matches_event};
use phux_config::{Action, KeybindingsCfg};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::select_list::WHEEL_SCROLL_ROWS;
use super::widgets::{
    ChordRow, ChordSection, KeyChordTable, Modal, centered_panel, paint_scrollbar,
};
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;

/// One row in the help table: a chord (or chord sequence) and the
/// action it resolves to. `chord_text` already includes the prefix
/// (`"C-a v"`) for prefix-table entries so the user sees the literal
/// keystrokes they need to type.
#[derive(Debug, Clone)]
struct Entry {
    chord: String,
    action: String,
}

impl From<&HardcodedBinding> for Entry {
    fn from(b: &HardcodedBinding) -> Self {
        Self {
            chord: b.chord.to_owned(),
            action: b.action.to_owned(),
        }
    }
}

/// One driver-owned, non-rebindable interaction, as advertised in the
/// help overlay's built-in sections (phux-i0e8.10.3).
///
/// Tables of these are COLOCATED with the modules that implement the
/// interaction — copy-mode keys with [`super::copy_mode`], context-menu
/// gestures with [`super::menu`], divider drag-resize with
/// [`crate::render::chrome::dividers`], sidebar clicks with
/// [`crate::render::chrome::sidebar`] — and aggregated by
/// [`HelpOverlay::from_config`] into named sections. Colocation plus a
/// per-module adjacency test (every table row must map to a real
/// `handle_key` / `handle_mouse` arm) is the anti-rot mechanism: a module
/// that changes its input handling has the table describing it in the
/// same file.
#[derive(Debug, Clone, Copy)]
pub struct HardcodedBinding {
    /// The literal gesture: a key (`"Tab"`), a mouse gesture
    /// (`"right-click"`), or a clickable label (the sidebar's `+ new`).
    pub chord: &'static str,
    /// What the gesture does.
    pub action: &'static str,
}

/// Help overlay — keybindings reference modal.
///
/// Built from a [`KeybindingsCfg`] snapshot via [`Self::from_config`].
/// Rendering composes a [`KeyChordTable`] (sections grouped prefix-table
/// → global → the built-in `Copy mode` / `Mouse & menus` /
/// `Panels & dashboards` sections) into a centered [`Modal`].
#[derive(Debug)]
pub struct HelpOverlay {
    /// Pretty-printed prefix chord (e.g. `"C-a"`), or empty if no
    /// prefix-table entries exist.
    prefix: String,
    /// Prefix-table entries — chord shown is `"<prefix> <key>"`.
    prefix_entries: Vec<Entry>,
    /// Global entries — chord shown as-is.
    global_entries: Vec<Entry>,
    /// Driver-owned interactions, grouped into named sections
    /// (`Copy mode`, `Mouse & menus`, `Panels & dashboards`). Authored at
    /// construction time from the tables colocated with the implementing
    /// modules (plus the config-resolved panel chords) so the overlay
    /// stays `'static`.
    hardcoded_sections: Vec<(String, Vec<Entry>)>,
    /// Chord that dismisses this overlay, resolved from `show-help` in
    /// `cfg.global` first, then `cfg.prefix_table` (bare key — the
    /// prefix resolver is bypassed while an overlay is active). Used in
    /// the footer hint so a user who rebound `show-help` to e.g. `?`
    /// sees `Press ? or Esc to close` rather than a stale `F1`.
    /// `None` when neither table maps a chord to `show-help`.
    show_help_chord: Option<String>,
    /// Color slots snapshotted from the active [`Theme`] at construction.
    /// Captured (not borrowed) so the overlay stays `'static`.
    theme: Theme,
    /// First visible body row — the scroll offset, in the *wrapped*
    /// display-row units the modal's paragraph scrolls by (phux-9adu).
    ///
    /// Interior-mutable because the bottom clamp needs the wrapped row
    /// count and viewport height, both known only at paint time, and
    /// [`RenderOverlay::render`] takes `&self` — the same bargain the
    /// [`SelectList`](super::select_list::SelectList) viewport makes.
    /// Pure view state: render clamps and writes it back every frame.
    scroll: Cell<usize>,
    /// Rows the body viewport held at the last paint, recorded so
    /// `PageUp` / `PageDown` can move by a real screenful. Zero until the
    /// first render (page keys then fall back to a single row).
    page: Cell<usize>,
}

impl HelpOverlay {
    /// Build the overlay from a config snapshot, styled with `theme`.
    /// Cheap; clones small strings and copies the `Theme`. The overlay
    /// owns its entries and colors — both `cfg` and `theme` may be
    /// dropped immediately after construction.
    #[must_use]
    pub fn from_config(cfg: &KeybindingsCfg, theme: &Theme) -> Self {
        // The numeric window-jump keys (0-9, each `select-window {index}`)
        // would otherwise be ten near-identical rows. Collect them aside
        // and render a single `C-a 0-9  select window by number` line so
        // the list stays scannable and the capability reads clearly.
        let mut prefix_entries: Vec<Entry> = Vec::new();
        let mut window_jump_keys: Vec<String> = Vec::new();
        for (chord, action) in &cfg.prefix_table {
            if is_indexed_select_window(action) {
                window_jump_keys.push(chord.clone());
            } else {
                prefix_entries.push(Entry {
                    chord: format!("{} {chord}", cfg.prefix),
                    action: action_label(action),
                });
            }
        }
        if let Some(entry) = compact_window_jump(&cfg.prefix, &window_jump_keys) {
            prefix_entries.push(entry);
        }
        let global_entries: Vec<Entry> = cfg
            .global
            .iter()
            .map(|(chord, action)| Entry {
                chord: chord.clone(),
                action: action_label(action),
            })
            .collect();
        let hardcoded_sections = built_in_sections(cfg);
        // Find the chord (if any) bound to `show-help`, for the footer
        // hint and the dismiss match. Resolution order: `cfg.global`
        // first, then `cfg.prefix_table` — a global chord works
        // verbatim while the overlay is up, so it wins when both are
        // bound. For a prefix-table binding (the shipped default binds
        // `?` there) we store the BARE key, not `"<prefix> ?"`: while
        // an overlay is active the prefix resolver is bypassed — only
        // the overlay's own `handle_key` runs — so the bare key is the
        // literal keystroke that dismisses, and echoing the full chord
        // in the footer would lie about the dismiss path (phux-i0e8.10.4).
        let show_help_chord = cfg
            .global
            .iter()
            .find(|(_, action)| is_show_help(action))
            .or_else(|| {
                cfg.prefix_table
                    .iter()
                    .find(|(_, action)| is_show_help(action))
            })
            .map(|(chord, _)| chord.clone());
        Self {
            prefix: if prefix_entries.is_empty() {
                String::new()
            } else {
                cfg.prefix.clone()
            },
            prefix_entries,
            global_entries,
            hardcoded_sections,
            show_help_chord,
            theme: *theme,
            scroll: Cell::new(0),
            page: Cell::new(0),
        }
    }

    /// Group the snapshotted entries into the [`KeyChordTable`]'s
    /// sections (prefix → global → the named built-in sections). Empty
    /// sections are dropped by the table itself.
    fn chord_table(&self) -> KeyChordTable {
        let to_rows = |entries: &[Entry]| {
            entries
                .iter()
                .map(|e| ChordRow::new(e.chord.clone(), e.action.clone()))
                .collect::<Vec<_>>()
        };
        let mut sections = vec![
            ChordSection::new(
                format!("Prefix bindings ({})", self.prefix),
                to_rows(&self.prefix_entries),
            ),
            ChordSection::new("Global bindings", to_rows(&self.global_entries)),
        ];
        for (title, entries) in &self.hardcoded_sections {
            sections.push(ChordSection::new(title.clone(), to_rows(entries)));
        }
        KeyChordTable::new(&self.theme, sections).empty_notice("No keybindings configured.")
    }

    /// The dismiss-hint footer, reflecting the user-bound `show-help`
    /// chord when present.
    fn footer(&self) -> String {
        self.show_help_chord.as_deref().map_or_else(
            || "Press Esc to close".to_owned(),
            |chord| format!("Press {chord} or Esc to close"),
        )
    }

    /// Scroll up by `rows`, saturating at the top.
    fn scroll_up(&self, rows: usize) {
        self.scroll.set(self.scroll.get().saturating_sub(rows));
    }

    /// Scroll down by `rows`. Deliberately unclamped here: the bottom
    /// clamp needs the wrapped row count, which only the paint path can
    /// compute — [`RenderOverlay::render`] clamps and writes back every
    /// frame, so an overshoot never survives a paint.
    fn scroll_down(&self, rows: usize) {
        self.scroll.set(self.scroll.get().saturating_add(rows));
    }

    /// Rows in a page move: the last painted viewport height, or a single
    /// row before the first paint (no viewport measured yet — better a
    /// small step than a wild one).
    fn page_rows(&self) -> usize {
        self.page.get().max(1)
    }

    /// The one-column scrollbar track: the modal's right border column,
    /// spanning the interior rows between the two corners.
    const fn scrollbar_track(modal_area: Rect) -> Rect {
        Rect::new(
            modal_area.x + modal_area.width.saturating_sub(1),
            modal_area.y.saturating_add(1),
            1,
            modal_area.height.saturating_sub(2),
        )
    }
}

impl RenderOverlay for HelpOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        let body = self.chord_table().body_lines();
        let modal = Modal::new(&self.theme, "phux help", body)
            .footer(self.footer())
            .wrap(true);

        // The body renders with wrapping on, so the scroll window must be
        // measured in *wrapped* display rows at this modal's interior
        // width — counting logical lines undercounts whenever a chord row
        // folds onto a second row (phux-9adu). `wrapped_row_count` rides
        // the same word wrapper the paragraph paints with, so the clamp,
        // the scrollbar, and the pixels always agree.
        let inner_width = modal_area.width.saturating_sub(2);
        let inner_height = usize::from(modal_area.height.saturating_sub(2));
        let total = modal.wrapped_row_count(inner_width);
        self.page.set(inner_height);
        let offset = self.scroll.get().min(total.saturating_sub(inner_height));
        self.scroll.set(offset);

        modal
            .scroll(u16::try_from(offset).unwrap_or(u16::MAX))
            .render_into(modal_area, buf);
        // Over the border the modal just drew; a no-op when the table
        // fits, so the fits case renders exactly as before (no bar).
        paint_scrollbar(
            buf,
            Self::scrollbar_track(modal_area),
            &self.theme,
            total,
            offset,
        );
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // ~70% of the viewport, min 40x10, clamped to the outer rect.
        Some(centered_panel(area, 7, 40, 10))
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        // Esc always dismisses. So does the chord the user actually bound
        // to `show-help` — pressing the open-help key again is the
        // universal toggle, even when the overlay only models "show" +
        // "dismiss". We match the *configured* chord (via
        // `chord_str_matches_event`) rather than a hardcoded F1, so a user
        // who rebound `show-help` to e.g. `?` closes it with `?`
        // (phux-ahv.7). When no `show-help` binding exists, only Esc
        // closes it.
        if key.key == PhysicalKey::Escape {
            return OverlayCommand::Dismiss;
        }
        if let Some(chord) = &self.show_help_chord
            && chord_str_matches_event(chord, key)
        {
            return OverlayCommand::Dismiss;
        }

        // Scroll navigation (phux-9adu), mirroring the SelectList
        // overlays: arrows / `j` / `k` / `C-n` / `C-p` step a row,
        // page keys move a screenful, Home/End jump to the ends. Help
        // has no query line, so `j`/`k` are always free to navigate.
        // Dismiss checks ran first, so a user who bound `show-help` to
        // one of these keys still closes the overlay with it. Press
        // only, like SelectList — a Press/Release pair must not
        // double-step.
        if key.action == KeyAction::Press {
            if key.mods.contains(ModSet::CTRL) {
                match key.key {
                    PhysicalKey::N => {
                        self.scroll_down(1);
                        return OverlayCommand::Stay;
                    }
                    PhysicalKey::P => {
                        self.scroll_up(1);
                        return OverlayCommand::Stay;
                    }
                    _ => {}
                }
            }
            match key.key {
                PhysicalKey::ArrowDown | PhysicalKey::J => self.scroll_down(1),
                PhysicalKey::ArrowUp | PhysicalKey::K => self.scroll_up(1),
                PhysicalKey::PageDown => self.scroll_down(self.page_rows()),
                PhysicalKey::PageUp => self.scroll_up(self.page_rows()),
                PhysicalKey::Home => self.scroll.set(0),
                // "As far down as possible" — the paint path clamps it to
                // the real bottom, where the wrapped row count is known.
                PhysicalKey::End => self.scroll.set(usize::MAX),
                _ => {}
            }
        }
        OverlayCommand::Stay
    }

    /// The wheel scrolls the view a detent at a time ([`WHEEL_SCROLL_ROWS`],
    /// same feel as the palette and copy-mode). Help has no cursor, so
    /// unlike [`SelectList`](super::select_list::SelectList) the wheel
    /// moves the window itself.
    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        if mouse.action != MouseAction::Press {
            return OverlayCommand::Stay;
        }
        match mouse.button {
            MouseButton::Four => self.scroll_up(WHEEL_SCROLL_ROWS),
            MouseButton::Five => self.scroll_down(WHEEL_SCROLL_ROWS),
            _ => {}
        }
        OverlayCommand::Stay
    }
}

/// Human-readable label for an [`Action`]. Bare actions show the name;
/// parameterized actions show `name(key=value, ...)` so the user sees
/// the args their binding actually carries (e.g. `split-pane(direction=vertical)`).
/// `pub(super)` so the which-key popup labels its rows identically.
pub(super) fn action_label(action: &Action) -> String {
    match action {
        Action::Bare(name) => name.clone(),
        Action::Parameterized(p) => {
            if p.args.is_empty() {
                p.action.clone()
            } else {
                let args: Vec<String> = p
                    .args
                    .iter()
                    .map(|(k, v)| format!("{k}={}", value_label(v)))
                    .collect();
                format!("{}({})", p.action, args.join(", "))
            }
        }
    }
}

fn value_label(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `true` for a `select-window` binding carrying an explicit `index` —
/// the numeric 0-9 window-jump keys, which the overlay collapses into one
/// row rather than listing individually. `pub(super)` so the which-key
/// popup applies the same collapse.
pub(super) fn is_indexed_select_window(action: &Action) -> bool {
    matches!(
        action,
        Action::Parameterized(p) if p.action == "select-window" && p.args.contains_key("index")
    )
}

/// Collapse the numeric window-jump keys into a single help row, e.g.
/// `C-a 0-9   select window by number`. `keys` arrive sorted (`BTreeMap`
/// iteration). Returns `None` when no such keys are bound.
fn compact_window_jump(prefix: &str, keys: &[String]) -> Option<Entry> {
    let first = keys.first()?;
    let last = keys.last()?;
    let chord = if keys.len() == 1 {
        format!("{prefix} {first}")
    } else {
        format!("{prefix} {first}-{last}")
    };
    Some(Entry {
        chord,
        action: "select window by number".to_owned(),
    })
}

/// The overlay's built-in sections (phux-i0e8.10.3): the driver-owned
/// interactions no keybinding table describes, aggregated from the
/// [`HardcodedBinding`] tables colocated with the implementing modules,
/// plus the `Panels & dashboards` discoverability section — whose chords
/// ARE rebindable and therefore resolve against the active config.
fn built_in_sections(cfg: &KeybindingsCfg) -> Vec<(String, Vec<Entry>)> {
    let copy_mode: Vec<Entry> = super::copy_mode::HELP_BINDINGS
        .iter()
        .map(Entry::from)
        .collect();
    // Mouse-driven surfaces share one section: the right-click menus,
    // divider drag-resize, and the sidebar's click targets.
    let mouse: Vec<Entry> = super::menu::HELP_BINDINGS
        .iter()
        .chain(crate::render::chrome::dividers::HELP_BINDINGS)
        .chain(crate::render::chrome::sidebar::HELP_BINDINGS)
        .map(Entry::from)
        .collect();
    // phux-i0e8.7's punted discoverability findings land here: the
    // sidebar is off by default and the fleet dashboard hides behind one
    // chord — advertise both, with the chord resolved from the active
    // config (the same resolver the palette and context menus use, so
    // the surfaces cannot disagree about which chord runs an action).
    let panels = vec![
        Entry {
            chord: resolved_chord(cfg, "toggle-sidebar"),
            action: "toggle-sidebar (window sidebar; off by default)".to_owned(),
        },
        Entry {
            chord: resolved_chord(cfg, "agent-fleet"),
            action: "agent-fleet (dashboard of every agent pane)".to_owned(),
        },
    ];
    vec![
        ("Copy mode".to_owned(), copy_mode),
        ("Mouse & menus".to_owned(), mouse),
        ("Panels & dashboards".to_owned(), panels),
    ]
}

/// The chord bound to the bare action `name` in `cfg`, formatted as the
/// literal keystrokes to type (prefix-table entries include the leader),
/// or `"unbound"`. Delegates to the shared
/// [`crate::attach::action_registry::bound_chord_for`] resolver.
fn resolved_chord(cfg: &KeybindingsCfg, name: &str) -> String {
    crate::attach::action_registry::bound_chord_for(
        Some(cfg),
        &ResolvedAction {
            action: name.to_owned(),
            args: BTreeMap::new(),
        },
    )
    .unwrap_or_else(|| "unbound".to_owned())
}

/// `true` when `action` resolves to `show-help` (bare or parameterized).
/// Used to find the user-bound chord for the dismiss footer hint.
fn is_show_help(action: &Action) -> bool {
    match action {
        Action::Bare(name) => name == "show-help",
        Action::Parameterized(p) => p.action == "show-help",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_config::ParamAction;
    use phux_protocol::input::key::{KeyAction, ModSet};
    use std::collections::BTreeMap;

    fn cfg() -> KeybindingsCfg {
        let mut prefix_table = BTreeMap::new();
        prefix_table.insert("d".to_owned(), Action::Bare("detach".to_owned()));
        prefix_table.insert("x".to_owned(), Action::Bare("kill-pane".to_owned()));
        let mut split_args = BTreeMap::new();
        split_args.insert(
            "direction".to_owned(),
            toml::Value::String("vertical".to_owned()),
        );
        prefix_table.insert(
            "v".to_owned(),
            Action::Parameterized(ParamAction {
                action: "split-pane".to_owned(),
                args: split_args,
            }),
        );
        let mut global = BTreeMap::new();
        global.insert("F1".to_owned(), Action::Bare("show-help".to_owned()));
        KeybindingsCfg {
            prefix: "C-a".to_owned(),
            prefix_table,
            global,
            ..KeybindingsCfg::default()
        }
    }

    fn key(k: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key: k,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    #[test]
    fn from_config_collects_prefix_and_global() {
        let overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        assert_eq!(overlay.prefix, "C-a");
        assert_eq!(overlay.prefix_entries.len(), 3);
        assert_eq!(overlay.global_entries.len(), 1);
        // phux-i0e8.10.3: the built-in sections are never empty — every
        // config gets the driver-owned interactions.
        assert_eq!(overlay.hardcoded_sections.len(), 3);
        assert!(
            overlay
                .hardcoded_sections
                .iter()
                .all(|(_, entries)| !entries.is_empty()),
            "every built-in section carries rows"
        );
        assert!(overlay.prefix_entries.iter().any(|e| e.action == "detach"));
        // show-help is bound to F1 in the test cfg.
        assert_eq!(overlay.show_help_chord.as_deref(), Some("F1"));
    }

    /// phux-i0e8.10.3: the built-in sections render under their own
    /// names with rows aggregated from the colocated module tables —
    /// including the `right-click` gesture and a copy-mode row — plus
    /// the `Panels & dashboards` discoverability rows.
    #[test]
    fn built_in_sections_render_named_with_their_rows() {
        let overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        // Tall viewport so the whole table fits without scrolling.
        let text = render_to_string(&overlay, 110, 80);
        for header in ["Copy mode", "Mouse & menus", "Panels & dashboards"] {
            assert!(text.contains(header), "missing `{header}` header:\n{text}");
        }
        // A copy-mode row (from copy_mode::HELP_BINDINGS).
        assert!(
            text.contains("copy the selection and exit"),
            "missing copy-mode row:\n{text}"
        );
        // The right-click gesture (from menu::HELP_BINDINGS).
        assert!(
            text.contains("right-click"),
            "missing right-click row:\n{text}"
        );
        // Drag-resize (dividers) and the sidebar affordances, reusing the
        // shared label consts.
        assert!(
            text.contains("drag divider"),
            "missing drag-resize row:\n{text}"
        );
        assert!(
            text.contains(crate::render::chrome::sidebar::NEW_LABEL),
            "missing sidebar `+ new` row:\n{text}"
        );
        // Panels & dashboards names both actions.
        assert!(
            text.contains("toggle-sidebar"),
            "missing toggle-sidebar row:\n{text}"
        );
        assert!(
            text.contains("agent-fleet"),
            "missing agent-fleet row:\n{text}"
        );
    }

    /// phux-i0e8.10.3: the `Panels & dashboards` chords resolve from the
    /// ACTIVE config. Under the shipped defaults that is `C-a b`
    /// (toggle-sidebar, default.toml) and `C-a A` (agent-fleet).
    #[test]
    fn panels_section_resolves_chords_from_the_shipped_defaults() {
        let cfg = phux_config::parse_str(
            phux_config::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("default config parses");
        let overlay = HelpOverlay::from_config(&cfg.keybindings, &Theme::default());
        let panels = panels_entries(&overlay);
        let chord_for = |name: &str| {
            panels
                .iter()
                .find(|e| e.action.contains(name))
                .unwrap_or_else(|| panic!("panels section names {name}"))
                .chord
                .clone()
        };
        assert_eq!(chord_for("toggle-sidebar"), "C-a b");
        assert_eq!(chord_for("agent-fleet"), "C-a A");
    }

    /// phux-i0e8.10.3: a rebound config changes the advertised chord; a
    /// config with no binding shows `unbound` rather than lying.
    #[test]
    fn panels_section_follows_rebinds_and_admits_unbound() {
        // The base test cfg binds neither action: both rows say so.
        let overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        for entry in panels_entries(&overlay) {
            assert_eq!(entry.chord, "unbound", "{}", entry.action);
        }
        // Rebound: globals win when no prefix-table binding exists.
        let mut c = cfg();
        c.global
            .insert("F5".to_owned(), Action::Bare("agent-fleet".to_owned()));
        c.prefix_table
            .insert("g".to_owned(), Action::Bare("toggle-sidebar".to_owned()));
        let overlay = HelpOverlay::from_config(&c, &Theme::default());
        let panels = panels_entries(&overlay);
        assert!(
            panels
                .iter()
                .any(|e| e.action.contains("agent-fleet") && e.chord == "F5"),
            "global rebind resolves: {panels:?}"
        );
        assert!(
            panels
                .iter()
                .any(|e| e.action.contains("toggle-sidebar") && e.chord == "C-a g"),
            "prefix-table rebind resolves with the leader shown: {panels:?}"
        );
    }

    /// The `Panels & dashboards` entries of `overlay`.
    fn panels_entries(overlay: &HelpOverlay) -> Vec<Entry> {
        overlay
            .hardcoded_sections
            .iter()
            .find(|(title, _)| title == "Panels & dashboards")
            .map(|(_, entries)| entries.clone())
            .expect("the panels section exists")
    }

    #[test]
    fn no_show_help_binding_falls_back_to_esc_only_footer() {
        // Neither table binds `show-help`: the base cfg()'s prefix
        // table carries only detach/kill-pane/split-pane, and the
        // global table is cleared here (phux-i0e8.10.4: the resolver
        // scans both).
        let mut c = cfg();
        c.global.clear();
        let overlay = HelpOverlay::from_config(&c, &Theme::default());
        assert_eq!(overlay.show_help_chord, None);
        // Tall viewport: the built-in sections (phux-i0e8.10.3) overflow
        // an 80x24 modal, which would push the footer beyond the fold.
        let text = render_to_string(&overlay, 110, 80);
        assert!(text.contains("Press Esc to close"), "text was:\n{text}");
    }

    #[test]
    fn punctuation_chords_render_verbatim() {
        // phux-9fu shipped `|` and `-` as default prefix-table keys.
        // They must render as the literal glyph the user typed in
        // TOML, not as escape sequences or raw bytes.
        let mut c = cfg();
        c.prefix_table
            .insert("|".to_owned(), Action::Bare("split-pane".to_owned()));
        c.prefix_table
            .insert("-".to_owned(), Action::Bare("split-pane".to_owned()));
        let overlay = HelpOverlay::from_config(&c, &Theme::default());
        let text = render_to_string(&overlay, 80, 24);
        assert!(text.contains("C-a |"), "missing `C-a |` row:\n{text}");
        assert!(text.contains("C-a -"), "missing `C-a -` row:\n{text}");
    }

    #[test]
    fn esc_dismisses() {
        let mut overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        assert_eq!(
            overlay.handle_key(&key(PhysicalKey::Escape)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn f1_dismisses() {
        let mut overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        assert_eq!(
            overlay.handle_key(&key(PhysicalKey::F1)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn other_keys_stay() {
        let mut overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        assert_eq!(
            overlay.handle_key(&key(PhysicalKey::A)),
            OverlayCommand::Stay
        );
    }

    #[test]
    fn dismisses_on_rebound_show_help_chord_not_just_f1() {
        // phux-ahv.7: a user who rebinds `show-help` to `?` must be able
        // to dismiss with `?`, and F1 (no longer bound) must NOT dismiss.
        let mut c = cfg();
        c.global.clear();
        c.global
            .insert("?".to_owned(), Action::Bare("show-help".to_owned()));
        let mut overlay = HelpOverlay::from_config(&c, &Theme::default());
        assert_eq!(overlay.show_help_chord.as_deref(), Some("?"));

        // `?` is Shift+/ on US ANSI (the chord parser decomposes it), so
        // build the live event accordingly.
        let mut question = key(PhysicalKey::Slash);
        question.mods = ModSet::SHIFT;
        assert_eq!(
            overlay.handle_key(&question),
            OverlayCommand::Dismiss,
            "the rebound show-help chord should dismiss"
        );

        // F1 is no longer the help binding: it must be absorbed, not
        // treated as a dismiss.
        assert_eq!(
            overlay.handle_key(&key(PhysicalKey::F1)),
            OverlayCommand::Stay,
            "F1 should no longer dismiss once show-help is rebound off it"
        );
        // Esc still always dismisses.
        assert_eq!(
            overlay.handle_key(&key(PhysicalKey::Escape)),
            OverlayCommand::Dismiss
        );
    }

    /// phux-i0e8.10.4: under the shipped defaults `show-help` lives in
    /// the PREFIX table (`C-a ?`), yet the dismiss chord is the bare
    /// `?` — the prefix resolver is bypassed while the overlay is up,
    /// so `?` alone is the literal keystroke that closes it. The footer
    /// echoes that bare key and `handle_key` dismisses on it.
    #[test]
    fn default_config_prefix_table_question_mark_footers_and_dismisses() {
        let cfg = phux_config::parse_str(
            phux_config::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("default config parses");
        let mut overlay = HelpOverlay::from_config(&cfg.keybindings, &Theme::default());
        assert_eq!(
            overlay.show_help_chord.as_deref(),
            Some("?"),
            "the prefix-table binding resolves to its BARE key"
        );

        // The footer echoes the bare key. The default table is long, so
        // jump to the end (with a paint between, as the driver does) to
        // bring the footer into the window before asserting on pixels.
        overlay.handle_key(&key(PhysicalKey::End));
        let text = render_to_string(&overlay, 110, 80);
        assert!(
            text.contains("Press ? or Esc to close"),
            "footer must echo the bare `?`:\n{text}"
        );

        // `?` is Shift+/ on US ANSI (the chord parser decomposes it).
        let mut question = key(PhysicalKey::Slash);
        question.mods = ModSet::SHIFT;
        assert_eq!(
            overlay.handle_key(&question),
            OverlayCommand::Dismiss,
            "the bare `?` dismisses under the shipped defaults"
        );
    }

    /// phux-i0e8.10.4: when `show-help` is bound in BOTH tables the
    /// global chord wins — it works verbatim while the overlay is up,
    /// so it is the least surprising one to advertise and match.
    #[test]
    fn global_show_help_wins_over_prefix_table_binding() {
        let mut c = cfg(); // global already binds F1 = show-help
        c.prefix_table
            .insert("?".to_owned(), Action::Bare("show-help".to_owned()));
        let mut overlay = HelpOverlay::from_config(&c, &Theme::default());
        assert_eq!(overlay.show_help_chord.as_deref(), Some("F1"));
        assert_eq!(
            overlay.handle_key(&key(PhysicalKey::F1)),
            OverlayCommand::Dismiss,
            "the winning global chord dismisses"
        );
    }

    #[test]
    fn numeric_window_jumps_collapse_to_one_row() {
        // phux UX: ten `select-window {index}` bindings (0-9) must render
        // as a single compact row, not ten near-identical lines.
        let mut c = cfg();
        for i in 0..10u8 {
            let mut args = BTreeMap::new();
            args.insert("index".to_owned(), toml::Value::Integer(i.into()));
            c.prefix_table.insert(
                i.to_string(),
                Action::Parameterized(ParamAction {
                    action: "select-window".to_owned(),
                    args,
                }),
            );
        }
        let overlay = HelpOverlay::from_config(&c, &Theme::default());
        // Exactly one entry carries the collapsed label, and the individual
        // C-a 0 / C-a 9 rows are gone.
        let collapsed = overlay
            .prefix_entries
            .iter()
            .filter(|e| e.action == "select window by number")
            .count();
        assert_eq!(
            collapsed, 1,
            "expected exactly one collapsed window-jump row"
        );
        assert!(
            overlay
                .prefix_entries
                .iter()
                .all(|e| e.chord != "C-a 0" && e.chord != "C-a 9"),
            "individual numeric jump rows should be collapsed away"
        );
        let text = render_to_string(&overlay, 80, 24);
        assert!(
            text.contains("C-a 0-9"),
            "expected compacted `C-a 0-9` row:\n{text}"
        );
        assert!(
            text.contains("select window by number"),
            "compacted row should make window selection clear:\n{text}"
        );
    }

    #[test]
    fn parameterized_action_label_shows_args() {
        let mut args = BTreeMap::new();
        args.insert(
            "direction".to_owned(),
            toml::Value::String("vertical".to_owned()),
        );
        let a = Action::Parameterized(ParamAction {
            action: "split-pane".to_owned(),
            args,
        });
        assert_eq!(action_label(&a), "split-pane(direction=vertical)");
    }

    #[test]
    fn bare_action_label_is_just_the_name() {
        assert_eq!(
            action_label(&Action::Bare("kill-pane".to_owned())),
            "kill-pane"
        );
    }

    /// Render `overlay` into a fresh `width × height` buffer and
    /// flatten to a `\n`-separated string with trailing spaces
    /// trimmed per row. Trimming keeps assertions terse but does
    /// not change the visible cell content.
    fn render_to_string(overlay: &HelpOverlay, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        overlay.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    #[test]
    fn render_into_buffer_does_not_panic() {
        let overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        let text = render_to_string(&overlay, 80, 24);
        assert!(
            text.contains("phux help"),
            "expected 'phux help' title in rendered buffer:\n{text}"
        );
    }

    #[test]
    fn render_surfaces_all_three_sections() {
        // Plain-string assertion (not `insta`) — keeps the test
        // robust against width-driven padding while still proving
        // that every section the overlay promises is actually
        // painted. Covers the regression that motivated phux-i08:
        // configured prefix/global chords were missing from the overlay.
        let overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        // Tall viewport: the built-in sections (phux-i0e8.10.3) overflow
        // an 80x24 modal, which would push the footer beyond the fold.
        let text = render_to_string(&overlay, 110, 80);
        // Section headers.
        assert!(
            text.contains("Prefix bindings (C-a)"),
            "missing prefix header:\n{text}"
        );
        assert!(
            text.contains("Global bindings"),
            "missing global header:\n{text}"
        );
        // At least one row from each section.
        assert!(text.contains("C-a d"), "missing detach row:\n{text}");
        assert!(text.contains("C-a x"), "missing prefix-table row:\n{text}");
        assert!(
            text.contains("C-a v"),
            "missing parameterized prefix row:\n{text}"
        );
        assert!(text.contains("F1"), "missing global row:\n{text}");
        // Footer reflects the bound chord, not a hardcoded "F1".
        assert!(
            text.contains("Press F1 or Esc to close"),
            "missing dynamic footer:\n{text}"
        );
    }

    // ---------- phux-9adu: scroll viewport ----------

    /// A config whose global section carries `n` bindings `g00`, `g01`, …
    /// mapped to `action-00`, `action-01`, … — enough rows to overflow any
    /// test modal. `BTreeMap` iteration keeps them sorted, so `action-00`
    /// is always the first table row and `action-(n-1)` the last.
    fn tall_cfg(n: usize) -> KeybindingsCfg {
        let mut global = BTreeMap::new();
        for i in 0..n {
            global.insert(format!("g{i:02}"), Action::Bare(format!("action-{i:02}")));
        }
        KeybindingsCfg {
            prefix: "C-a".to_owned(),
            prefix_table: BTreeMap::new(),
            global,
            ..KeybindingsCfg::default()
        }
    }

    #[test]
    fn long_table_scrolls_and_clamps_at_both_ends() {
        // 80x20 viewport -> 56x14 modal -> 12 interior rows; 30 bindings
        // plus header and footer overflow it several times over.
        let mut overlay = HelpOverlay::from_config(&tall_cfg(30), &Theme::default());
        let top = render_to_string(&overlay, 80, 20);
        assert!(top.contains("action-00"), "head visible at the top:\n{top}");
        assert!(
            !top.contains("agent-fleet"),
            "tail must start beyond the fold:\n{top}"
        );

        // Down-arrow steps (paint between each, as the driver does) walk
        // the window to the tail — which since phux-i0e8.10.3 is the
        // built-in `Panels & dashboards` section.
        for _ in 0..200 {
            overlay.handle_key(&key(PhysicalKey::ArrowDown));
            render_to_string(&overlay, 80, 20);
        }
        let bottom = render_to_string(&overlay, 80, 20);
        assert!(
            bottom.contains("agent-fleet"),
            "the tail must be reachable:\n{bottom}"
        );
        assert!(
            !bottom.contains("action-00"),
            "the head scrolled out of the window:\n{bottom}"
        );
        // ...and the offset clamps flush with the bottom rather than
        // running past the content.
        let clamped = overlay.scroll.get();
        overlay.handle_key(&key(PhysicalKey::ArrowDown));
        render_to_string(&overlay, 80, 20);
        assert_eq!(
            overlay.scroll.get(),
            clamped,
            "Down at the bottom must hold still"
        );

        // Up returns all the way and clamps at zero.
        for _ in 0..200 {
            overlay.handle_key(&key(PhysicalKey::ArrowUp));
        }
        let text = render_to_string(&overlay, 80, 20);
        assert_eq!(overlay.scroll.get(), 0, "Up saturates at the top");
        assert!(text.contains("action-00"), "head visible again:\n{text}");
    }

    #[test]
    fn wrapped_rows_count_toward_the_scroll_extent() {
        // One binding's action label is long enough to fold onto several
        // display rows at the modal's width. The window must budget for
        // those extra rows: were the extent counted in logical lines, End
        // would clamp short and the last rows (and the footer) would stay
        // beyond reach — the exact phux-9adu failure mode called out in
        // the bead.
        let mut c = tall_cfg(12);
        c.global.insert(
            "zz".to_owned(),
            Action::Bare(format!("run-hook({})", "very-long-argument-".repeat(10))),
        );
        let mut overlay = HelpOverlay::from_config(&c, &Theme::default());

        overlay.handle_key(&key(PhysicalKey::End));
        let text = render_to_string(&overlay, 80, 20);
        assert!(
            text.contains("Press Esc to close"),
            "End must reveal the footer beneath the folded label:\n{text}"
        );
        // The offset End clamped to exceeds anything a logical-line count
        // could produce: the folded label added display rows on top of the
        // logical extent. 12 is the interior height of the 56x14 modal an
        // 80x20 viewport centers.
        let logical_lines = overlay.chord_table().body_lines().len() + 2;
        assert!(
            overlay.scroll.get() > logical_lines.saturating_sub(12),
            "End offset {} must exceed the logical-line extent ({logical_lines} lines - 12 rows) \
             — wrapped rows were not counted",
            overlay.scroll.get(),
        );
    }

    #[test]
    fn fitting_content_is_unscrolled_and_barless() {
        // The small test cfg fits a tall viewport's modal comfortably
        // (the built-in sections add ~30 rows since phux-i0e8.10.3, so
        // 80x24 no longer fits): no scrollbar thumb, a zero offset, and
        // inert scroll keys — the rendered bytes must not change at all
        // (phux-9adu's "fits" regression guard).
        let mut overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        let before = render_to_string(&overlay, 110, 90);
        assert!(
            !before.contains('█'),
            "no scrollbar when content fits:\n{before}"
        );
        overlay.handle_key(&key(PhysicalKey::ArrowDown));
        overlay.handle_key(&key(PhysicalKey::PageDown));
        overlay.handle_key(&key(PhysicalKey::End));
        let after = render_to_string(&overlay, 110, 90);
        assert_eq!(overlay.scroll.get(), 0, "offset clamps to zero on a fit");
        assert_eq!(after, before, "scroll keys must not move a fitting table");
    }

    #[test]
    fn overflowing_table_paints_a_scrollbar() {
        let overlay = HelpOverlay::from_config(&tall_cfg(30), &Theme::default());
        let text = render_to_string(&overlay, 80, 20);
        assert!(
            text.contains('█'),
            "an overflowing table must paint a scrollbar thumb:\n{text}"
        );
    }

    #[test]
    fn home_end_and_page_keys_navigate_the_window() {
        let mut overlay = HelpOverlay::from_config(&tall_cfg(30), &Theme::default());
        // Before the first paint the viewport height is unknown; a page
        // key steps a single row rather than guessing.
        overlay.handle_key(&key(PhysicalKey::PageDown));
        assert_eq!(overlay.scroll.get(), 1, "unmeasured page = one row");
        // After a paint, a page is a real screenful (the interior height).
        render_to_string(&overlay, 80, 20);
        let page = overlay.page.get();
        assert!(page > 1, "the paint recorded a viewport height");
        overlay.handle_key(&key(PhysicalKey::PageDown));
        render_to_string(&overlay, 80, 20);
        assert_eq!(overlay.scroll.get(), 1 + page);
        overlay.handle_key(&key(PhysicalKey::PageUp));
        render_to_string(&overlay, 80, 20);
        assert_eq!(overlay.scroll.get(), 1);
        // End lands flush with the bottom, Home rewinds to the top. The
        // tail is the built-in `Panels & dashboards` section
        // (phux-i0e8.10.3).
        overlay.handle_key(&key(PhysicalKey::End));
        let text = render_to_string(&overlay, 80, 20);
        assert!(
            text.contains("agent-fleet"),
            "End reaches the tail:\n{text}"
        );
        overlay.handle_key(&key(PhysicalKey::Home));
        render_to_string(&overlay, 80, 20);
        assert_eq!(overlay.scroll.get(), 0);
    }

    #[test]
    fn vi_and_emacs_style_keys_step_the_window() {
        let mut overlay = HelpOverlay::from_config(&tall_cfg(30), &Theme::default());
        render_to_string(&overlay, 80, 20);
        // j/k are always free to navigate — help has no query line.
        overlay.handle_key(&key(PhysicalKey::J));
        assert_eq!(overlay.scroll.get(), 1);
        overlay.handle_key(&key(PhysicalKey::K));
        assert_eq!(overlay.scroll.get(), 0);
        // C-n / C-p, mirroring the palette.
        let mut ctrl_n = key(PhysicalKey::N);
        ctrl_n.mods = ModSet::CTRL;
        overlay.handle_key(&ctrl_n);
        assert_eq!(overlay.scroll.get(), 1);
        let mut ctrl_p = key(PhysicalKey::P);
        ctrl_p.mods = ModSet::CTRL;
        overlay.handle_key(&ctrl_p);
        assert_eq!(overlay.scroll.get(), 0);
        // A key release must not double-step.
        let mut release = key(PhysicalKey::ArrowDown);
        release.action = KeyAction::Release;
        overlay.handle_key(&release);
        assert_eq!(overlay.scroll.get(), 0, "release events do not scroll");
    }

    #[test]
    fn wheel_scrolls_the_view() {
        fn wheel(button: MouseButton) -> MouseEvent {
            MouseEvent {
                action: MouseAction::Press,
                button,
                mods: ModSet::empty(),
                x: 0.0,
                y: 0.0,
            }
        }
        let mut overlay = HelpOverlay::from_config(&tall_cfg(30), &Theme::default());
        assert_eq!(
            overlay.handle_mouse(&wheel(MouseButton::Five)),
            OverlayCommand::Stay
        );
        assert_eq!(
            overlay.scroll.get(),
            WHEEL_SCROLL_ROWS,
            "wheel-down scrolls a detent"
        );
        overlay.handle_mouse(&wheel(MouseButton::Four));
        assert_eq!(overlay.scroll.get(), 0, "wheel-up rewinds it");
        // Saturates at the top instead of underflowing.
        overlay.handle_mouse(&wheel(MouseButton::Four));
        assert_eq!(overlay.scroll.get(), 0);
    }

    #[test]
    fn render_centers_within_tiny_viewport() {
        // Regression for the prior `centered` clamp: a viewport smaller
        // than the modal's preferred size must still render without
        // overflowing (the shared `centered` helper clamps to the outer
        // rect). Just assert it paints something without panicking.
        let overlay = HelpOverlay::from_config(&cfg(), &Theme::default());
        let text = render_to_string(&overlay, 20, 8);
        assert!(
            text.contains("phux help"),
            "title in tiny viewport:\n{text}"
        );
    }
}
