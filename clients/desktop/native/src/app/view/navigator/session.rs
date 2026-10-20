//! A session's row and its disclosed contents. Disclosure is independent of
//! the window's selected session.

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, IntoElement, MouseButton, SharedString, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;

use crate::app::color::from_hex;
use crate::app::desk::{Desk, display_title};
use crate::app::fleet::Work;
use crate::app::skin::Look;
use crate::app::ui::list::{
    CHILD_ROW, Emphasis, ICON, ROW_GAP, ROW_INSET, row_detail, row_icon, row_label,
};
use crate::app::ui::menu::Anchor;
use crate::app::ui::status::state_dot;
use crate::app::ui::{ghost_icon, row};
use crate::app::view::ResourceDrag;

use super::rows::{catalog_pane_row, pane_row, record_row};
use super::{host_word, session_caret};

/// A host-qualified persistent identity, shared by active and inactive rows.
/// A rename preserves disclosure; equal names on different hosts do not share it.
pub(super) fn disclosure_key(work: &Work) -> String {
    format!(
        "{:?}:{:?}:{}",
        work.destination.host,
        work.destination.authority,
        work.destination
            .key
            .map_or_else(|| format!("handle:{}", work.id), |key| key.to_string())
    )
}

const HEADER_GAP: f32 = 4.;
/// The space above each of a session's rows.
const ROW_SPACING: f32 = 1.;
/// The trailing slot: room for the actions button the count yields to.
const TRAILING: f32 = 24.;

/// Whether a session has rows to disclose. An empty one has no caret.
pub(super) fn foldable(work: &Work) -> bool {
    !work.panes.is_empty() || !work.resources.is_empty()
}

/// One line per session: caret, name, and a quiet trailing count. Sessions
/// start folded; only the caret opens one, and Option-click opens or folds
/// them all, as in a Finder outline.
pub(super) fn session_block(
    desk: &Desk,
    work: &Work,
    ambiguous: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let target = work.destination.clone();
    let current = desk.work_is_active(&target);
    // Session actions target any session on the active connection, including
    // empty/inactive sessions. Selection is not mutation authority.
    let actionable = work.online && target.host == desk.remote;
    let key = disclosure_key(work);
    let foldable = foldable(work);
    let collapsed = !(foldable && desk.expanded.contains(&key));
    let moves = super::super::chrome_moves(desk, cx);
    let selector = format!("work-{}-{}", target.host_label(), work.id);
    let (detail, tip) = session_description(desk, work, ambiguous);
    let emphasis = if current {
        Emphasis::Current
    } else if work.terminals == 0 || !work.online {
        Emphasis::Quiet
    } else {
        Emphasis::Normal
    };
    let header = row(SharedString::from(selector.clone()), look)
        .debug_selector(move || selector.clone())
        .group("session")
        .gap(px(HEADER_GAP))
        .when(current, |row| row.bg(look.active))
        .child(if foldable {
            disclose(work, &key, collapsed, moves, look, cx).into_any_element()
        } else {
            // Nothing to open: the slot stays so every name starts on one column.
            row_icon(div())
                .mr(px(ROW_GAP - HEADER_GAP))
                .into_any_element()
        })
        .child(
            row_label(display_title(&target.name), emphasis, look).debug_selector({
                let target = target.clone();
                let id = work.id;
                move || format!("session-label-{}-{id}", target.host_label())
            }),
        )
        .child(trailing(desk, work, detail, actionable, look, cx))
        .when_some(
            work.destination.key.filter(|_| current),
            |row, destination| {
                let name = work.destination.name.clone();
                let wash = from_hex(&look.chrome.accent_wash);
                row.drag_over::<ResourceDrag>(move |style, _, _, _| style.bg(wash))
                    .on_drop(cx.listener(move |desk, drag: &ResourceDrag, window, cx| {
                        desk.move_resources_to(
                            drag.keys.clone(),
                            destination,
                            name.clone(),
                            window,
                            cx,
                        );
                    }))
            },
        )
        .when(actionable, |row| {
            let name = work.destination.name.clone();
            let id = work.id;
            row.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                    desk.open_session_menu(id, &name, Anchor::At(event.position), window, cx);
                }),
            )
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |desk, _, _, cx| desk.navigate_work(target.clone(), cx)));
    let disclosure = crate::app::desk::session_disclosure(&key);
    let moving = desk.disclosing(&disclosure);
    let rows = (!collapsed || moving).then(|| session_rows(desk, work, current, look, cx));
    div()
        .id(SharedString::from(format!("session-block-{key}")))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(header)
        .children(rows.map(|rows| disclosed(&key, rows, collapsed, moving, look)))
        .into_any_element()
}

/// The caret, hit across the row's full height. Option-click carries its new
/// state to every session in the list.
fn disclose(
    work: &Work,
    key: &str,
    collapsed: bool,
    moves: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let toggle_key = key.to_owned();
    let host = work.destination.host_label().to_owned();
    let id = work.id;
    div()
        .id(SharedString::from(format!("disclose-{key}")))
        .debug_selector(move || format!("disclose-work-{host}-{id}"))
        .role(gpui::Role::Button)
        .aria_label(if collapsed { "Expand session" } else { "Collapse session" })
        .cursor_pointer()
        .self_stretch()
        .flex()
        .items_center()
        // The title starts on its children's icon column.
        .mr(px(ROW_GAP - HEADER_GAP))
        .child(session_caret(key, collapsed, moves, look))
        .on_click(cx.listener(move |desk, event: &ClickEvent, _, cx| {
            cx.stop_propagation();
            if event.modifiers().alt {
                let all = super::foldable_sessions(desk);
                desk.set_sessions_expanded(all, collapsed, cx);
            } else {
                desk.toggle_session_expanded(&toggle_key, cx);
            }
        }))
}

/// What trails the name: a machine word where the name repeats, a state
/// word, the terminal count and the attention dot. On hover the actions
/// button takes their place instead of holding its own column on every row.
fn trailing(
    desk: &Desk,
    work: &Work,
    detail: Option<String>,
    actionable: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let open = desk.menu_open(&format!("session-{}", work.id));
    let selector = format!(
        "session-detail-{}-{}",
        work.destination.host_label(),
        work.id
    );
    let meta = div()
        .debug_selector(move || selector.clone())
        .h_full()
        .flex()
        .items_center()
        .justify_end()
        .gap(px(6.))
        .children(detail.map(|detail| {
            let host = work.destination.host_label().to_owned();
            let id = work.id;
            row_detail(detail, look).debug_selector(move || format!("session-words-{host}-{id}"))
        }))
        .when(work.terminals > 0, |meta| {
            meta.child(
                row_detail(work.terminals.to_string(), look)
                    .text_color(from_hex(&look.chrome.muted)),
            )
        })
        .children(work.attention.map(|state| state_dot(Some(state), None)))
        .when(actionable, |meta| {
            if open {
                meta.opacity(0.)
            } else {
                meta.group_hover("session", |style| style.opacity(0.))
            }
        });
    div()
        .relative()
        .flex_shrink_0()
        .min_w(px(TRAILING))
        .self_stretch()
        .flex()
        .justify_end()
        .child(meta)
        .child(session_actions(work, actionable, open, cx))
}

/// A session's terminals and records. Terminals that share a label carry
/// their id, the name `phux` and the CLI already use for them.
fn session_rows(
    desk: &Desk,
    work: &Work,
    current: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> Vec<AnyElement> {
    let labels: Vec<String> = work
        .panes
        .iter()
        .map(|pane| {
            if current {
                desk.pane_title(Some(&pane.descriptor.terminal_id))
            } else {
                pane.label.clone()
            }
        })
        .collect();
    let mut rows: Vec<AnyElement> = work
        .panes
        .iter()
        .zip(&labels)
        .map(|(pane, label)| {
            let repeated = labels.iter().filter(|other| *other == label).count() > 1;
            let suffix = repeated.then(|| pane.descriptor.terminal_id.to_string());
            if current {
                pane_row(desk, &pane.descriptor, label.clone(), suffix, look, cx).into_any_element()
            } else {
                catalog_pane_row(work, pane, suffix, look, cx)
            }
        })
        .collect();
    rows.extend(work.resources.iter().map(|resource| {
        if current {
            record_row(desk, resource, look, cx)
        } else {
            super::rows::catalog_record_row(work, resource, look, cx)
        }
    }));
    rows
}

/// A session's rows, unfolding from under its header: while they move their
/// height rides the chrome spring and they fade with it, so nothing is cut
/// mid-glyph. At rest they take their own height.
fn disclosed(
    key: &str,
    rows: Vec<AnyElement>,
    collapsed: bool,
    moving: bool,
    look: &Look,
) -> AnyElement {
    let full = rows.len() as f32 * (CHILD_ROW + ROW_SPACING);
    // A hairline down the caret's column ties the rows to their session.
    let guide = div()
        .absolute()
        .left(px(ROW_INSET + ICON / 2.))
        .top(px(ROW_SPACING + 4.))
        .bottom(px(4.))
        .w(px(1.))
        .bg(look.divider);
    let rows = div()
        .relative()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .children(
            rows.into_iter()
                .map(|row| div().flex_shrink_0().pt(px(ROW_SPACING)).child(row)),
        )
        // Last, so a hovered or selected row's wash does not cut it.
        .child(guide);
    if !moving {
        return rows.into_any_element();
    }
    let spring = super::super::chrome_spring(true, if collapsed { 0. } else { full });
    let spring = if collapsed {
        spring.from(full)
    } else {
        spring.from(0.)
    };
    rows.overflow_hidden()
        .with_spring(
            SharedString::from(format!("disclose-rows-{key}")),
            spring,
            move |rows, height| {
                let height = height.clamp(0., full);
                let shown = if full > 0. { height / full } else { 1. };
                rows.h(px(height)).opacity(shown * shown)
            },
        )
        .into_any_element()
}

/// The row's trailing words, only where they say something the name does
/// not, and the tooltip's full sentence.
fn session_description(desk: &Desk, work: &Work, ambiguous: bool) -> (Option<String>, String) {
    let mut words = Vec::new();
    words.extend(host_word(desk, work.destination.host.as_deref(), ambiguous));
    if work.imported {
        words.push("imported".to_owned());
    }
    if !work.online {
        words.push("offline".to_owned());
    }
    let shown = (!words.is_empty()).then(|| words.join(" · "));
    let detail = std::iter::once(crate::app::desk::counted(work.terminals, "terminal"))
        .chain(words)
        .collect::<Vec<_>>()
        .join(" · ");
    let role = if work.intake {
        " · Intake, the home for unorganized work"
    } else if work.imported {
        " · Imported, awaiting organization"
    } else {
        ""
    };
    let tip = format!(
        "{} · {} · {detail}{role}",
        work.destination.name,
        crate::app::machine_name::label(work.destination.host.as_deref())
    );
    (shown, tip)
}

fn session_actions(
    work: &Work,
    actionable: bool,
    open: bool,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let id = work.id;
    let name = work.destination.name.clone();
    let selector = format!("session-actions-{}-{id}", work.destination.host_label());
    // Over the trailing words, shown on hover; the row keeps no column for it.
    div()
        .debug_selector(move || selector.clone())
        .absolute()
        .top_0()
        .bottom_0()
        .right_0()
        .flex()
        .items_center()
        .when(!actionable, |slot| slot.invisible())
        // Shown with the pointer on its row, and while its menu is open.
        .when(actionable && !open, |slot| {
            slot.opacity(0.)
                .group_hover("session", |style| style.opacity(1.))
        })
        .child(
            ghost_icon(
                format!("session-menu-{}-{id}", work.destination.host_label()),
                IconName::Ellipsis,
                "Session actions",
            )
            .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                desk.open_session_menu(id, &name, Anchor::At(event.position()), window, cx);
            })),
        )
}
