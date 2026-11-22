//! The band over the terminal (kit `WindowTitlebar`) and what it says:
//! the session and terminal on screen, the tabs, the link's trouble, the
//! terminal's size while it changes, and the toolbar that shows on hover.

use crate::ui::tokens::Voice;
use phux_client_runtime::control::Status;
use phux_config_core::rune::{TabBar, TitleBar};
use phux_design::State;
use rune::{ChromeInsets, Color, El, TextStyle, el, row, text};
use rune_kit::WindowTitlebar;

use super::{Desk, Paint, Shortcut, Tokens, badge, icons, tabs, tip};
use crate::ui::controls::terminal_icon_press;

/// The terminal's size in the title bar: quiet machine figures that keep
/// their columns as the window is dragged.
fn geometry_type(t: &Tokens) -> TextStyle {
    t.ty.readout
}

/// How the link is, when it isn't simply up: a healthy window says nothing.
/// Gone is the failed tone's `lost`, its mark beside it; on the way up is
/// quiet.
fn link_trouble(desk: &Desk, t: &Tokens) -> Option<El> {
    let quiet = |word: &'static str| {
        text(word)
            .font(t.ty.caption)
            .color(t.roles.text.dim.paint())
    };
    match desk.connection.status {
        _ if desk.connection.client.is_none() && desk.link_word() != "connecting" => Some(lost(t)),
        Status::Attached => None,
        Status::Idle | Status::Connecting => Some(quiet("connecting")),
        Status::Negotiated => Some(quiet("negotiated")),
        Status::Closed | Status::Failed => Some(lost(t)),
    }
}

fn lost(t: &Tokens) -> El {
    let word = State::Lost.word().unwrap_or("lost");
    row()
        .key("link-lost")
        .gap(t.space.s1)
        .items_center()
        .label(word)
        .child(badge::mark(t, State::Lost, t.icon.size.xs).a11y_hidden())
        .child(
            text(word)
                .font(t.ty.caption)
                .color(t.roles.status.failed.paint())
                .a11y_hidden(),
        )
}

/// What is on screen, unless tabs say that already.
fn place(desk: &Desk, t: &Tokens) -> El {
    let session = desk.selected_session_name().unwrap_or_default();
    let title = desk.window_title().to_owned();
    // The window title ends in its session; here that sits in front.
    let title = title
        .strip_suffix(&format!(" — {session}"))
        .map_or(title.clone(), str::to_owned);
    // The terminal's title grows from nothing into what is left, so a narrow
    // window cuts it before it cuts the session's name.
    // With no terminal on screen the window title is the app's own name.
    let terminal =
        (desk.connection.shown.is_some() && title != session && !title.is_empty()).then(|| {
            el().w(0.0).grow(1.0).min_w(0.0).child(
                text(title)
                    .font(t.ty.label)
                    .color(t.roles.text.dim.paint())
                    .truncate(),
            )
        });
    row()
        .gap(t.space.icon_gap)
        .min_w(0.0)
        .shrink()
        .grow(8.0)
        .items_center()
        .child(
            text(session)
                .font(t.ty.label.semibold())
                .color(t.roles.text.text.paint())
                .truncate(),
        )
        .children(terminal)
}

/// With "Show toolbar on hover", a strip along the top edge that grows into
/// the toolbar while the pointer is over it, on the drawer spring. It
/// floats over the panes, so showing it never resizes a terminal.
pub fn hover_toolbar(desk: &Desk, p: &Tokens, lights: f32, trailing: f32) -> Option<El> {
    let t = p;
    let shown = desk.pointer.toolbar;
    (desk.settings.title_bar == TitleBar::Hover).then(|| {
        el().key("toolbar-reveal")
            .absolute(0.0, 0.0)
            .w_full()
            .h(if shown {
                t.space.titlebar_h
            } else {
                t.space.icon_gap
            })
            .token_motion(t.motion.drawer.enter)
            .on_hover_change(|d: &mut Desk, over, _| d.pointer.toolbar = over)
            .children(shown.then(|| {
                band(
                    desk,
                    p,
                    lights,
                    trailing,
                    if desk.flush() {
                        t.roles.layer.ground.paint()
                    } else {
                        t.roles.layer.sidebar.paint()
                    },
                )
                .transition(rune::Transition::opacity())
            }))
    })
}

/// What sits where the title bar would: the band itself, or with
/// `title-bar = "hidden"` the tabs as a row of their own (`tab-bar = "top"`)
/// or a thin strip, either of which still moves the window.
pub fn top(desk: &Desk, p: &Tokens, lights: f32, trailing: f32) -> Option<El> {
    match desk.settings.title_bar {
        TitleBar::Shown => Some(title_bar(desk, p, lights, trailing)),
        TitleBar::Hover => None,
        TitleBar::Hidden => {
            // Clear of the traffic lights, which sit here while the sidebar is folded.
            let lead = if desk.sidebar_shown() { 0.0 } else { lights };
            let tabs = tab_row(desk, p, TabBar::Top).map(|row| row.pl(p.space.card_gap.max(lead)));
            let strip = || el().key("window-drag").w_full().h(p.space.s2);
            Some(tabs.unwrap_or_else(strip).window_drag())
        }
    }
}

/// The tabs on a row of their own at `at`: under the panes with
/// `tab-bar = "bottom"`, or along the top with the title bar hidden.
pub fn tab_row(desk: &Desk, p: &Tokens, at: TabBar) -> Option<El> {
    let s = desk.settings;
    let here = s.tab_bar == at && (at == TabBar::Bottom || s.title_bar == TitleBar::Hidden);
    let strip = here.then(|| tabs::strip(desk, p)).flatten()?;
    Some(
        row()
            .key(if at == TabBar::Bottom {
                "tab-bar-bottom"
            } else {
                "tab-bar-top"
            })
            .w_full()
            .h(p.space.titlebar_h)
            .px(p.space.card_gap)
            .items_center()
            .child(
                row()
                    .grow(1.0)
                    .min_w(0.0)
                    .h_full()
                    .items_center()
                    .on_layout(|d: &mut Desk, rect| d.connection.tabs.room = rect[2])
                    .child(strip),
            ),
    )
}

/// The band over the terminal (kit `WindowTitlebar`): draggable, under the
/// traffic lights when the sidebar is folded, clear of the caption buttons
/// Rune draws on Linux (`trailing`), and holding what a status line would.
pub fn title_bar(desk: &Desk, p: &Tokens, lights: f32, trailing: f32) -> El {
    band(desk, p, lights, trailing, Color::TRANSPARENT)
}

fn band(desk: &Desk, p: &Tokens, lights: f32, trailing: f32, fill: Color) -> El {
    let t = p;
    let s = &t.space;
    // Only while a resize is fresh; at rest the bar says nothing about it.
    // The words keep their room, so tabs never shift as they fade.
    let geometry = desk
        .last_size()
        .map(|(cols, rows)| format!("{cols}×{rows}"))
        .unwrap_or_default();
    let sizing = desk.size_glance().is_some();
    let folded = !desk.sidebar_shown();
    let sidebar = terminal_icon_press(
        &icons::SIDEBAR,
        "Sidebar",
        "sidebar-toggle",
        t,
        Desk::toggle_sidebar,
    )
    .expanded(desk.sidebar_shown());
    let settings = terminal_icon_press(
        &icons::SETTINGS,
        "Settings",
        "settings-button",
        t,
        Desk::toggle_settings,
    );
    // Where a ⌘-click goes, as a browser shows it: in the room the tabs
    // leave, so pointing at a link never squeezes them.
    let link = desk.pointer.hover.as_ref().map(|(_, link)| {
        text(link.url.clone())
            .font(t.ty.caption)
            .color(t.roles.accent.accent_text.paint())
            .truncate()
    });
    // Everything between the sidebar's button and the window's own words:
    // its width is the room the tabs are cut from.
    let lane = row()
        .key("titlebar-lane")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .gap(s.s2)
        .items_center()
        .on_layout(|d: &mut Desk, rect| {
            if d.settings.tab_bar == TabBar::Top {
                d.connection.tabs.room = rect[2];
            }
        })
        .children(
            (desk.settings.tab_bar == TabBar::Top)
                .then(|| tabs::strip(desk, p))
                .flatten()
                .or_else(|| Some(place(desk, t))),
        )
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .justify_end()
                .items_center()
                .children(link),
        );
    // The traffic lights sit over the sidebar while it shows.
    let mut insets = ChromeInsets::default();
    insets.leading = if folded { lights } else { 0.0 };
    insets.trailing = trailing;
    insets.height = s.titlebar_h;
    let mut bar = WindowTitlebar::from_insets(insets)
        .height(s.titlebar_h)
        .leading_padding(if folded { s.s2 } else { s.s0_5 })
        .trailing_padding(s.s3)
        .gap(s.s2)
        .fill(fill)
        .leading(tip(sidebar, "Sidebar", Shortcut::ToggleSidebar))
        .tabs(lane);
    if let Some(trouble) = link_trouble(desk, t) {
        bar = bar.trailing(trouble);
    }
    if let Some(pill) = super::super::updates::pill(desk, p) {
        bar = bar.trailing(pill);
    }
    bar.trailing(
        text(geometry)
            .font(geometry_type(t))
            .color(t.roles.text.dim.paint())
            .opacity(if sizing { 1.0 } else { 0.0 })
            .a11y_hidden(),
    )
    .trailing(
        row()
            .gap(s.s0_5)
            .items_center()
            .child(super::super::news::bell(desk, p))
            .child(tip(settings, "Settings", Shortcut::Settings)),
    )
    .build(&t.kit)
}
