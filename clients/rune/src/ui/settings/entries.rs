//! The searchable inventory is the inventory we render: no fake preferences.

use rune::{El, col, el, row, text};

use super::super::{CAPTION, Ink, icons, pressable, updates};
use super::{COPY, controls};
use crate::desk::{Desk, SettingsCategory as Category};

#[derive(Clone, Copy)]
pub(super) enum Entry {
    Theme,
    TextSize,
    CopySelect,
    ConfirmClose,
    OptionMeta,
    Notifications,
    Hosts,
    Diagnostics,
    GetStarted,
    Updates,
}

impl Entry {
    const ALL: [Self; 10] = [
        Self::Theme,
        Self::TextSize,
        Self::CopySelect,
        Self::ConfirmClose,
        Self::OptionMeta,
        Self::Notifications,
        Self::Hosts,
        Self::Diagnostics,
        Self::GetStarted,
        Self::Updates,
    ];

    const fn category(self) -> Category {
        match self {
            Self::Theme => Category::Appearance,
            Self::TextSize | Self::CopySelect | Self::ConfirmClose | Self::OptionMeta => {
                Category::Terminal
            }
            Self::Notifications => Category::Notifications,
            Self::Hosts => Category::RemoteHosts,
            Self::Diagnostics => Category::Developer,
            Self::GetStarted | Self::Updates => Category::About,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::TextSize => "Text size",
            Self::CopySelect => "Copy on select",
            Self::ConfirmClose => "Confirm before closing",
            Self::OptionMeta => "Option as Meta",
            Self::Notifications => "Notifications",
            Self::Hosts => "Machines",
            Self::Diagnostics => "Copy diagnostics",
            Self::GetStarted => "Get started",
            Self::Updates => "Updates",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Theme => {
                "System, built-in and local themes. Preview live, Return to keep, Escape to restore."
            }
            Self::TextSize => "Terminal text from 8 to 32 points.",
            Self::CopySelect => "Finishing a selection puts it on the clipboard.",
            Self::ConfirmClose => {
                "Ask before closing a terminal or tab that's still running something."
            }
            Self::OptionMeta => "Option sends Esc-prefixed keys instead of typing å and ∑.",
            Self::Notifications => crate::alerts::notifications_detail(
                crate::alerts::available(),
                cfg!(target_os = "macos"),
            ),
            Self::Hosts => {
                "Connect to a remote host, add a machine or manage your existing machines."
            }
            Self::Diagnostics => "Copy local logs and app state. Nothing is sent anywhere.",
            Self::GetStarted => {
                "The look, the phux agent skill, permissions and your key bindings, step by step."
            }
            Self::Updates => {
                "Phux Rune version, Check now, Check automatically, Update phux with Phux Rune, phux release channel: Follow installed, Stable or Next, local installation, package manager, server and protocol compatibility."
            }
        }
    }

    fn value(self, desk: &Desk) -> String {
        let on = |value| if value { "On" } else { "Off" }.to_owned();
        match self {
            Self::Theme => desk.settings.theme.map_or("System", |t| t.name).to_owned(),
            Self::TextSize => format!("{:.0} pt", desk.font_size),
            Self::CopySelect => on(desk.settings.copy_on_select),
            Self::ConfirmClose => on(desk.settings.confirm_close),
            Self::OptionMeta => on(desk.settings.option_as_meta),
            Self::Notifications => on(desk.settings.notifications),
            Self::Hosts => desk.machines.label().to_owned(),
            Self::Diagnostics => "Local only".to_owned(),
            Self::GetStarted => String::new(),
            Self::Updates => format!(
                "{} automatic {} manage phux {} channel {}",
                crate::updates::VERSION,
                on(desk.updater.automatic),
                on(desk.updater.manage_phux),
                desk.updater
                    .phux_channel
                    .map_or("installed", |c| c.as_str())
            ),
        }
    }

    fn matches(self, desk: &Desk, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {} {}",
            self.title(),
            self.detail(),
            self.category().name(),
            self.value(desk),
            self.aliases()
        )
        .to_lowercase();
        query.split_whitespace().all(|word| haystack.contains(word))
    }

    const fn aliases(self) -> &'static str {
        match self {
            Self::Theme => "colors colours appearance palette",
            Self::TextSize => "font zoom typography",
            Self::CopySelect => "clipboard selection",
            Self::OptionMeta => "alt keyboard escape",
            Self::ConfirmClose => "confirmation closing safety",
            Self::Notifications => "alerts bell",
            Self::Hosts => "remote ssh connection",
            Self::Diagnostics => "logs debug troubleshooting",
            Self::GetStarted => "onboarding welcome setup skill permissions migrate import",
            Self::Updates => {
                "upgrade version automatic release channel stable next preview install"
            }
        }
    }

    pub(super) fn view(self, desk: &Desk, ink: &Ink) -> El {
        match self {
            Self::Theme => self.link(
                "settings-theme",
                desk,
                self.value(desk),
                Desk::settings_themes,
                ink,
            ),
            Self::TextSize => controls::stepper(desk, ink),
            Self::Hosts => self.link(
                "settings-machines",
                desk,
                self.value(desk),
                Desk::toggle_machines,
                ink,
            ),
            Self::Diagnostics => diagnostics(desk, ink),
            Self::GetStarted => self.link(
                "settings-get-started",
                desk,
                self.value(desk),
                Desk::open_onboarding,
                ink,
            ),
            Self::Updates => el()
                .w_full()
                .padding(16.0)
                .child(updates::settings(desk, ink)),
            _ => self.toggle(desk, ink),
        }
    }

    fn toggle(self, desk: &Desk, ink: &Ink) -> El {
        let (key, value, press): (_, _, fn(&mut Desk)) = match self {
            Self::CopySelect => (
                "set-copy-select",
                desk.settings.copy_on_select,
                Desk::toggle_copy_on_select,
            ),
            Self::ConfirmClose => (
                "set-confirm-close",
                desk.settings.confirm_close,
                Desk::toggle_confirm_close,
            ),
            Self::OptionMeta => (
                "set-option-meta",
                desk.settings.option_as_meta,
                Desk::toggle_option_as_meta,
            ),
            _ => (
                "set-notifications",
                desk.settings.notifications,
                Desk::toggle_notifications,
            ),
        };
        controls::toggle(desk, key, self.title(), self.detail(), value, press, ink)
    }

    fn link(
        self,
        key: &'static str,
        desk: &Desk,
        value: String,
        press: fn(&mut Desk),
        ink: &Ink,
    ) -> El {
        pressable(key, ink)
            .w_full()
            .min_h(60.0)
            .px(16.0)
            .py(10.0)
            .gap(12.0)
            .items_center()
            .focusable(true)
            .label(self.title())
            .description(self.detail())
            .a11y_value(value.clone())
            .on_press(press)
            .child(self.link_copy(desk, &value, ink))
            .children(desk.phone.is_none().then(|| {
                text(value)
                    .font(CAPTION)
                    .color(ink.dim)
                    .max_w(120.0)
                    .truncate()
                    .a11y_hidden()
            }))
            .child(
                rune::icon(&icons::CHEVRON_RIGHT, 14.0)
                    .color(ink.dim)
                    .a11y_hidden(),
            )
    }

    fn link_copy(self, desk: &Desk, value: &str, ink: &Ink) -> El {
        let title = text(self.title())
            .font(super::primary(desk, COPY))
            .color(ink.text)
            .wrap();
        let heading = if desk.phone.is_some() {
            row()
                .w_full()
                .gap(8.0)
                .flex_wrap()
                .items_center()
                .child(title.w(80.0).min_w(80.0).grow(1.0))
                .child(
                    text(value)
                        .font(super::secondary(desk, CAPTION))
                        .color(ink.dim)
                        .max_w(140.0)
                        .wrap_anywhere(),
                )
        } else {
            title
        };
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(3.0)
            .child(heading)
            .child(
                text(self.detail())
                    .font(super::secondary(desk, CAPTION.lh(15.0)))
                    .color(ink.dim)
                    .wrap(),
            )
            .a11y_hidden()
    }
}

fn diagnostics(desk: &Desk, ink: &Ink) -> El {
    pressable("copy-diagnostics", ink)
        .w_full()
        .min_h(60.0)
        .padding(16.0)
        .focusable(true)
        .label("Copy diagnostics")
        .description(Entry::Diagnostics.detail())
        .on_press_tx(|d: &mut Desk, tx| d.copy_diagnostics(tx))
        .child(
            row()
                .w_full()
                .gap(12.0)
                .items_center()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(3.0)
                        .child(
                            text(if desk.updater.copied {
                                "Copied"
                            } else {
                                "Copy diagnostics"
                            })
                            .font(super::primary(desk, COPY))
                            .color(ink.text),
                        )
                        .child(
                            text(Entry::Diagnostics.detail())
                                .font(super::secondary(desk, CAPTION))
                                .color(ink.dim)
                                .wrap(),
                        ),
                )
                .child(rune::icon(&icons::CHEVRON_RIGHT, 14.0).color(ink.dim))
                .a11y_hidden(),
        )
}

pub(super) fn category_value(category: Category, desk: &Desk) -> String {
    match category {
        Category::Appearance => desk.settings.theme.map_or("System", |t| t.name).to_owned(),
        Category::Terminal => format!("{:.0} pt", desk.font_size),
        Category::Notifications => Entry::Notifications.value(desk),
        Category::RemoteHosts => desk.machines.label().to_owned(),
        Category::Developer => "Diagnostics".to_owned(),
        Category::About => crate::updates::VERSION.to_owned(),
    }
}

pub(super) fn matching(desk: &Desk) -> Vec<(Category, Vec<Entry>)> {
    let query = desk.settings_ui.query.text.trim().to_lowercase();
    let chosen = desk.settings_ui.category.unwrap_or(Category::Appearance);
    Category::ALL
        .into_iter()
        .filter_map(|category| {
            if query.is_empty() && category != chosen {
                return None;
            }
            let entries: Vec<_> = Entry::ALL
                .into_iter()
                .filter(|entry| !matches!(entry, Entry::OptionMeta) || cfg!(target_os = "macos"))
                .filter(|entry| entry.category() == category)
                .filter(|entry| entry.matches(desk, &query))
                .collect();
            (!entries.is_empty()).then_some((category, entries))
        })
        .collect()
}
