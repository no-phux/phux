//! Host-path rows, status messages, and the path picker panel.

use super::*;

/// A row of the path picker: the parent directory, or a host path.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PathItem {
    Parent(String),
    Row { path: String, kind: String },
}

impl PathItem {
    /// The directory Tab / Right browses into, if this row is one.
    pub(crate) fn directory(&self) -> Option<String> {
        match self {
            Self::Parent(directory) => Some(directory.clone()),
            Self::Row { path, kind } => paths::directory_for_terminal(kind, path),
        }
    }
}

/// The picker's rows in display order, which is also the palette's row index.
pub(crate) fn path_items(desk: &Desk) -> Vec<PathItem> {
    let Some(picker) = desk.picker.as_ref() else {
        return Vec::new();
    };
    picker
        .parent
        .clone()
        .map(PathItem::Parent)
        .into_iter()
        .chain(picker.rows.iter().map(|row| PathItem::Row {
            path: row.path.clone(),
            kind: row.kind.clone(),
        }))
        .collect()
}

/// Where the path picker says what state it is in. Loading, failure and an
/// empty answer read in the results; the footer keeps paging status and keys.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PathPresentation {
    /// Heads listed rows: why the last insertion failed.
    pub(super) heading: Option<String>,
    /// Fills the results when nothing is listed.
    pub(super) empty: String,
    /// Qualifies listed rows, such as a truncated answer.
    pub(super) status: Option<String>,
}

pub(super) fn path_presentation(picker: &paths::PickerState, has_rows: bool) -> PathPresentation {
    let notice = if picker.pending.is_some() {
        Some("Searching host paths…".to_owned())
    } else {
        picker.failure.as_deref().map(paths::display_path)
    };
    let message = (!picker.message.is_empty()).then(|| paths::display_path(&picker.message));
    if has_rows {
        return PathPresentation {
            heading: notice,
            empty: String::new(),
            status: message,
        };
    }
    PathPresentation {
        heading: None,
        empty: notice.or(message).unwrap_or_else(|| "No paths.".to_owned()),
        status: None,
    }
}

pub(super) fn path_panel(
    desk: &mut Desk,
    colors: &Palette,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(state) = desk.palette_state.clone() else {
        return div().into_any_element();
    };
    let Some(picker) = desk.picker.clone() else {
        return div().into_any_element();
    };
    let items = path_items(desk);
    let PathPresentation {
        heading,
        empty,
        status,
    } = path_presentation(&picker, !items.is_empty());
    let targets: Vec<Target> = items.iter().cloned().map(Target::Path).collect();
    let desk_handle = cx.weak_entity();
    let confirm_handle = desk_handle.clone();
    let terminal_handle = desk_handle.clone();
    let muted = from_hex(&colors.muted);
    let rows = items.into_iter().map(move |item| {
        let terminal_handle = terminal_handle.clone();
        let (icon, label, directory) = match &item {
            PathItem::Parent(_) => (IconName::ArrowUp, ".. (parent)".to_owned(), None),
            PathItem::Row { path, kind } => (
                if kind == "directory" {
                    IconName::Folder
                } else {
                    IconName::File
                },
                paths::display_path(path),
                item.directory(),
            ),
        };
        CommandItem::new().label(label.clone()).child(move |_, cx| {
            let terminal_handle = terminal_handle.clone();
            div()
                .group("path-row")
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(8.))
                .child(
                    Icon::new(icon.clone())
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .children(directory.clone().map(|directory| {
                    Button::new(format!("open-{directory}"))
                        .invisible()
                        .group_hover("path-row", |style| style.visible())
                        .label("Open terminal")
                        .ghost()
                        .xsmall()
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let directory = directory.clone();
                            let _ = terminal_handle.update(cx, |desk, cx| {
                                desk.open_path_terminal(directory, window, cx)
                            });
                        })
                }))
        })
    });
    let root = picker.root.clone();
    let mut group = CommandGroup::new().items(rows);
    if let Some(heading) = heading {
        group = group.label(heading);
    }
    let command = Command::new(&state)
        .filterable(false)
        .bordered(false)
        .placeholder(format!("Search in {root}…"))
        .max_h(px(420.))
        .group(group)
        .empty(move |_, _, cx| {
            div()
                .debug_selector(|| "path-results-notice".into())
                .py(px(36.))
                .px(px(24.))
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.))
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground))
                .child({
                    let text = empty.clone();
                    div()
                        .debug_selector(move || format!("path-notice: {text}"))
                        .child(empty.clone())
                })
        })
        .footer(move |_, _, _| {
            div()
                .debug_selector(|| "path-footer".into())
                .px(px(12.))
                .py(px(10.))
                .flex()
                .justify_between()
                .gap(px(12.))
                .text_xs()
                .text_color(muted)
                .children(status.clone().map(|status| {
                    div()
                        .debug_selector({
                            let text = status.clone();
                            move || format!("path-status: {text}")
                        })
                        .min_w_0()
                        .truncate()
                        .child(status)
                }))
                .child(div().ml_auto().flex_shrink_0().child("Tab: open terminal"))
        })
        .on_query(move |query, window, cx| {
            let _ = desk_handle.update(cx, |desk, cx| desk.palette_query(query, window, cx));
        })
        .on_confirm(move |index: IndexPath, window, cx| {
            if let Some(target) = targets.get(index.row).cloned() {
                let _ = confirm_handle.update(cx, |desk, cx| desk.pick(target, window, cx));
            }
        });
    sheet(look, 640.)
        .overflow_hidden()
        .key_context("PathPicker")
        .child(command)
        .into_any_element()
}
