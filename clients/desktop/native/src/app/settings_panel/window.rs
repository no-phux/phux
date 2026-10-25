//! A settings utility window, not a workspace modal. Preferences still belong
//! to the originating desk; text, selection and slider state belong here.

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Context, Entity, FocusHandle, KeyBinding, Subscription, Window,
    WindowBounds, WindowOptions, div, px, relative, size,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TitleBar};

use super::{Page, SettingsControls, build_controls, content, native_settings};
use crate::app::actions::Run;
use crate::app::desk::Desk;

gpui::actions!(
    settings_window,
    [CloseSettings, SearchSettings, EscapeSettings]
);

const SIDEBAR: f32 = 204.;

pub(crate) struct SettingsWindow {
    pub(crate) owner: AnyWindowHandle,
    desk: Entity<Desk>,
    pub(crate) page: Page,
    query: Entity<InputState>,
    controls: SettingsControls,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

pub(crate) fn open(
    desk: Entity<Desk>,
    owner: AnyWindowHandle,
    page: Option<Page>,
    cx: &mut App,
) -> anyhow::Result<(AnyWindowHandle, Entity<SettingsWindow>)> {
    cx.bind_keys([
        KeyBinding::new("cmd-w", CloseSettings, Some("PhuxSettings")),
        KeyBinding::new("ctrl-w", CloseSettings, Some("PhuxSettings")),
        KeyBinding::new("cmd-f", SearchSettings, Some("PhuxSettings")),
        KeyBinding::new("ctrl-f", SearchSettings, Some("PhuxSettings")),
        KeyBinding::new("escape", EscapeSettings, Some("PhuxSettings")),
    ]);
    let mut titlebar = TitleBar::title_bar_options();
    titlebar.title = Some("Phux Settings".into());
    gpui_kit::open_window(
        WindowOptions {
            app_id: Some("dev.phux.desktop".into()),
            window_bounds: Some(WindowBounds::centered(size(px(860.), px(680.)), cx)),
            window_min_size: Some(size(px(700.), px(480.))),
            titlebar: Some(titlebar),
            focus: crate::diag::may_activate(),
            kind: crate::diag::window_kind(),
            show: true,
            ..TitleBar::window_options()
        },
        cx,
        move |window, cx| cx.new(|cx| SettingsWindow::new(desk, owner, page, window, cx)),
    )
}

impl SettingsWindow {
    fn new(
        desk: Entity<Desk>,
        owner: AnyWindowHandle,
        page: Option<Page>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search Settings"));
        let updates = desk.read(cx).updates.clone();
        let subscriptions = vec![
            cx.observe(&desk, |_, _, cx| cx.notify()),
            cx.subscribe(&query, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe(&updates, |_, _, cx| cx.notify()),
        ];
        let controls = desk.update(cx, |desk, cx| build_controls(desk, window, cx));
        Self {
            owner,
            desk,
            page: page.unwrap_or(Page::Appearance),
            query,
            controls,
            focus,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn reveal(
        &mut self,
        page: Option<Page>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(page) = page {
            self.page = page;
            self.query
                .update(cx, |query, cx| query.set_value("", window, cx));
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn escape(&mut self, _: &EscapeSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.read(cx).value().is_empty() {
            window.remove_window();
        } else {
            self.query
                .update(cx, |query, cx| query.set_value("", window, cx));
            cx.notify();
        }
    }

    fn run(&mut self, action: &Run, window: &mut Window, cx: &mut Context<Self>) {
        if action.id == "find" {
            self.query.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        if local_command(&action.id, window, cx) {
            return;
        }
        let owner = self.owner;
        let desk = self.desk.clone();
        let id = action.id.clone();
        cx.defer(move |cx| {
            let _ = owner.update(cx, |_, window, cx| {
                window.activate_window();
                desk.update(cx, |desk, cx| desk.execute(&id, window, cx));
            });
        });
    }

    fn sidebar(&self, pages: &[Page], cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-sidebar")
            .debug_selector(|| "settings-sidebar".into())
            .w(px(SIDEBAR))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(12.))
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(Input::new(&self.query).prefix(IconName::Search).small())
            .child(
                div()
                    .id("settings-categories")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(pages.iter().copied().map(|page| {
                        let selector = format!("settings-category-{page:?}");
                        div().debug_selector(move || selector.clone()).child(
                            Button::new(gpui::SharedString::from(format!(
                                "settings-category-{page:?}"
                            )))
                            .accessibility_label(page.sidebar_title())
                            .ghost()
                            .small()
                            .h(px(28.))
                            .w_full()
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(Icon::new(page.icon()).size(px(14.)).flex_shrink_0())
                                    .child(
                                        div()
                                            .debug_selector(move || {
                                                format!("settings-category-label-{page:?}")
                                            })
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(13.))
                                            .child(page.sidebar_title()),
                                    ),
                            )
                            .toggled(self.page == page)
                            .when(self.page == page, |button| {
                                button
                                    .bg(cx.theme().accent)
                                    .text_color(cx.theme().accent_foreground)
                            })
                            .on_click(cx.listener(
                                move |settings, _, window, cx| {
                                    settings.page = page;
                                    window.focus(&settings.focus, cx);
                                    cx.notify();
                                },
                            )),
                        )
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Changes save automatically"),
            )
    }
}

/// Native menus dispatch Run too. Editing must stay in the focused settings
/// control; window commands must never minimize or fullscreen the workspace.
fn local_command(id: &str, window: &mut Window, cx: &mut App) -> bool {
    use gpui_kit::component::input::{Copy, Paste, SelectAll};
    match id {
        "settings" => {}
        "close" | "close-tab" => window.remove_window(),
        "minimize" => window.minimize_window(),
        "maximize" => window.zoom_window(),
        "fullscreen" => window.toggle_fullscreen(),
        "copy" => window.dispatch_action(Box::new(Copy), cx),
        "paste" => window.dispatch_action(Box::new(Paste), cx),
        "select-all" => window.dispatch_action(Box::new(SelectAll), cx),
        _ => return false,
    }
    true
}

impl gpui::Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query.read(cx).value().to_lowercase();
        let prefs = self.desk.read(cx).prefs.clone();
        self.controls.sync(&prefs, window, cx);
        let desk = self.desk.read(cx);
        let pages: Vec<_> = Page::ALL
            .into_iter()
            .map(|page| {
                (
                    page,
                    content(desk, page, &self.desk.downgrade(), Some(&self.controls), cx),
                )
            })
            .filter(|(page, content)| content.matches(*page, &query))
            .collect();
        let visible: Vec<_> = pages.iter().map(|(page, _)| *page).collect();
        if !visible.contains(&self.page)
            && let Some(page) = visible.first()
        {
            self.page = *page;
        }
        let page = self.page;
        let body = match pages.into_iter().find(|(candidate, _)| *candidate == page) {
            Some((_, content)) => {
                let settings = native_settings(
                    format!("settings-page-{page:?}-{query}").into(),
                    content
                        .filtered(
                            page,
                            &query,
                            window.viewport_size().width.as_f32() - SIDEBAR
                                < super::catalog::STACK_CONTROLS_BELOW,
                        )
                        .title(page.title())
                        .description(page.description()),
                )
                .with_group_variant(GroupBoxVariant::Fill);
                div()
                    .debug_selector(|| "settings-content".into())
                    .size_full()
                    .child(settings)
                    .into_any_element()
            }
            None => div()
                .debug_selector(|| "settings-empty".into())
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .child("No matching settings")
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Try a different search, or press Escape to clear it."),
                )
                .into_any_element(),
        };
        let context = self.desk.read(cx).session.clone();
        div()
            .id("settings-window")
            .debug_selector(|| "settings-window".into())
            .key_context("Desk PhuxSettings")
            .track_focus(&self.focus)
            .on_action(|_: &CloseSettings, window, _| window.remove_window())
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(|settings, _: &SearchSettings, window, cx| {
                settings
                    .query
                    .update(cx, |input, cx| input.focus(window, cx))
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_left()
            .line_height(relative(crate::app::ui::typography::LEADING))
            .child(
                TitleBar::new().child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child("Settings")
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("For this window · {context}")),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(&visible, cx))
                    .child(div().flex_1().min_w_0().h_full().child(body)),
            )
    }
}

impl Page {
    pub(super) fn sidebar_title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Layout => "Interface",
            Self::Keyboard => "Shortcuts",
            Self::Spacing => "Terminal",
            Self::Ghostty => "Ghostty",
            Self::Updates => "Updates",
            Self::Connections => "Machines",
            #[cfg(target_os = "macos")]
            Self::Permissions => "Permissions",
            Self::Developer => "Developer",
        }
    }

    fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::Appearance => IconName::Palette,
            Self::Layout => IconName::PanelLeft,
            Self::Keyboard => IconName::Keyboard,
            Self::Spacing => IconName::Frame,
            Self::Ghostty => IconName::Terminal,
            Self::Updates => IconName::Download,
            Self::Connections => IconName::Network,
            #[cfg(target_os = "macos")]
            Self::Permissions => IconName::Lock,
            Self::Developer => IconName::CodeXml,
        }
    }
}
