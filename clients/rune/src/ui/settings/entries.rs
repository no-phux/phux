//! The searchable inventory is the inventory we render: no fake preferences.

use crate::ui::tokens::Voice;
use crate::ui::tokens::layout::SETTINGS_LINK as GEOMETRY;
use phux_config_core::rune::TitleBar;
use rune::{El, col, el, row, text};

use super::super::{icons, pressable, updates};
use super::controls;
use crate::desk::{Desk, SettingsCategory as Category};
use crate::ui::tokens::{Paint, Tokens};

#[derive(Clone, Copy)]
pub(super) enum Entry {
    Theme,
    HoverToolbar,
    InterfaceSize,
    TextSize,
    Font,
    CopySelect,
    ConfirmClose,
    OptionMeta,
    WheelArrows,
    QuickTerminal,
    QuickChord,
    Notifications,
    Hosts,
    PairPhone,
    Diagnostics,
    GetStarted,
    Updates,
}

impl Entry {
    const ALL: [Self; 17] = [
        Self::Theme,
        Self::HoverToolbar,
        Self::InterfaceSize,
        Self::TextSize,
        Self::Font,
        Self::CopySelect,
        Self::ConfirmClose,
        Self::OptionMeta,
        Self::WheelArrows,
        Self::QuickTerminal,
        Self::QuickChord,
        Self::Notifications,
        Self::Hosts,
        Self::PairPhone,
        Self::Diagnostics,
        Self::GetStarted,
        Self::Updates,
    ];

    const fn category(self) -> Category {
        match self {
            Self::Theme | Self::HoverToolbar | Self::InterfaceSize => Category::Appearance,
            Self::TextSize
            | Self::Font
            | Self::CopySelect
            | Self::ConfirmClose
            | Self::OptionMeta
            | Self::WheelArrows
            | Self::QuickTerminal
            | Self::QuickChord => Category::Terminal,
            Self::Notifications => Category::Notifications,
            Self::Hosts | Self::PairPhone => Category::RemoteHosts,
            Self::Diagnostics => Category::Developer,
            Self::GetStarted | Self::Updates => Category::About,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::HoverToolbar => "Show toolbar on hover",
            Self::InterfaceSize => "Interface size",
            Self::TextSize => "Text size",
            Self::Font => "Font",
            Self::CopySelect => "Copy on select",
            Self::ConfirmClose => "Confirm before closing",
            Self::OptionMeta => "Option as Meta",
            Self::WheelArrows => "Scroll full-screen programs with arrows",
            Self::QuickTerminal => "Quick terminal",
            Self::QuickChord => "Quick terminal shortcut",
            Self::Notifications => "Notifications",
            Self::Hosts => "Machines",
            Self::PairPhone => "Pair a phone",
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
            Self::HoverToolbar => {
                "Tabs and buttons hide until the pointer reaches the top edge, so the terminal \
                 fills the window."
            }
            Self::InterfaceSize => {
                "The sidebar, tabs, palette and Settings from 80% to 200%. The terminal's text \
                 keeps its own size."
            }
            Self::TextSize => "Terminal text from 8 to 32 points.",
            Self::Font => "Terminal font. One that isn't installed shows the system's.",
            Self::CopySelect => "Finishing a selection puts it on the clipboard.",
            Self::ConfirmClose => {
                "Ask before closing a terminal or tab that's still running something. Until you \
                 choose, only a terminal an agent runs in asks."
            }
            Self::OptionMeta => "Option sends Esc-prefixed keys instead of typing å and ∑.",
            Self::WheelArrows => {
                "Scrolling over a full-screen program that doesn't use the mouse sends Up and \
                 Down, unless the program turned that off. Off, scrolling there does nothing, so \
                 it can't recall old prompts in an agent's input."
            }
            Self::QuickTerminal => {
                "A shortcut drops a terminal over any app, holding the quick session on this \
                 computer. It works in every app, so it's off until you turn it on."
            }
            Self::QuickChord => {
                "Click, then press the keys. Escape cancels; Backspace turns it off."
            }
            Self::Notifications => crate::alerts::notifications_detail(
                crate::alerts::available(),
                cfg!(target_os = "macos"),
            ),
            Self::Hosts => {
                "Connect to a remote host, add a machine or manage your existing machines."
            }
            Self::PairPhone => super::super::pair::DETAIL,
            Self::Diagnostics => "Copy local logs and app state. Nothing is sent anywhere.",
            Self::GetStarted => crate::onboarding::PROMISE.get_started,
            Self::Updates => {
                "Phux Rune version, Check now, Check automatically, Update phux with Phux Rune, phux release channel: Follow installed, Stable or Next, local installation, package manager, server and protocol compatibility."
            }
        }
    }

    fn value(self, desk: &Desk) -> String {
        let on = |value| if value { "On" } else { "Off" }.to_owned();
        match self {
            Self::Theme => desk.settings.theme.map_or("System", |t| t.name).to_owned(),
            Self::InterfaceSize => format!("{}%", desk.interface_size),
            Self::TextSize => format!("{:.0} pt", desk.font_size),
            Self::Font => match desk.font_family.as_str() {
                "" => "System".to_owned(),
                family => family.to_owned(),
            },
            Self::HoverToolbar => on(desk.settings.title_bar == TitleBar::Hover),
            Self::CopySelect => on(desk.settings.copy_on_select),
            Self::ConfirmClose => on(desk.settings.confirm_close),
            Self::OptionMeta => on(desk.settings.option_as_meta),
            Self::WheelArrows => on(desk.settings.wheel_arrows),
            Self::QuickTerminal => on(desk.quick_chord.on),
            Self::QuickChord => desk.quick_chord.recorder.keycaps().concat(),
            Self::Notifications => on(desk.settings.notifications),
            Self::Hosts => desk.machines.label().to_owned(),
            Self::PairPhone => match &desk.settings_ui.pairing.devices {
                Some(Ok(devices)) => format!("{} paired", devices.len()),
                _ => String::new(),
            },
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
            Self::HoverToolbar => "titlebar tab bar hide reveal edge to edge full window chrome",
            Self::InterfaceSize => "ui scale zoom bigger larger sidebar accessibility",
            Self::TextSize => "font zoom typography",
            Self::Font => "typeface family monospace",
            Self::CopySelect => "clipboard selection",
            Self::OptionMeta => "alt keyboard escape",
            Self::WheelArrows => "mouse wheel trackpad alternate scroll arrow keys history tui",
            Self::QuickTerminal | Self::QuickChord => {
                "global hotkey shortcut drop down dropdown panel quake visor"
            }
            Self::ConfirmClose => "confirmation closing safety",
            Self::Notifications => "alerts bell",
            Self::Hosts => "remote ssh connection",
            Self::PairPhone => "mobile iphone android qr code scan link devices revoke tailnet",
            Self::Diagnostics => "logs debug troubleshooting",
            Self::GetStarted if crate::onboarding::PERMISSIONS => {
                "onboarding welcome setup skill permissions migrate import"
            }
            Self::GetStarted => "onboarding welcome setup skill migrate import",
            Self::Updates => {
                "upgrade version automatic release channel stable next preview install"
            }
        }
    }

    pub(super) fn view(self, desk: &Desk, t: &Tokens) -> El {
        match self {
            Self::Theme => self.link(
                "settings-theme",
                desk,
                self.value(desk),
                Desk::settings_themes,
                t,
            ),
            Self::InterfaceSize => controls::interface_size(desk, t),
            Self::TextSize => controls::text_size(desk, t),
            Self::Font => self.link("settings-font", desk, self.value(desk), Desk::next_font, t),
            Self::Hosts => self.link(
                "settings-machines",
                desk,
                self.value(desk),
                Desk::toggle_machines,
                t,
            ),
            Self::PairPhone => super::super::pair::panel(desk, t, true),
            Self::Diagnostics => diagnostics(desk, t),
            Self::GetStarted => self.link(
                "settings-get-started",
                desk,
                self.value(desk),
                Desk::open_onboarding,
                t,
            ),
            Self::Updates => el()
                .w_full()
                .padding(t.space.s4)
                .child(updates::settings(desk, t)),
            Self::QuickChord => {
                let managed = desk.file.quick_terminal.is_some();
                let detail = if managed {
                    "Set in config.toml"
                } else {
                    self.detail()
                };
                controls::shortcut(desk, self.title(), detail, t)
            }
            _ => self.toggle(desk, t),
        }
    }

    fn toggle(self, desk: &Desk, t: &Tokens) -> El {
        let file = &desk.file;
        let (key, value, press, managed): (_, _, fn(&mut Desk), _) = match self {
            Self::HoverToolbar => (
                "set-hover-toolbar",
                desk.settings.title_bar == TitleBar::Hover,
                Desk::toggle_hover_toolbar,
                file.title_bar.is_some(),
            ),
            Self::CopySelect => (
                "set-copy-select",
                desk.settings.copy_on_select,
                Desk::toggle_copy_on_select,
                file.copy_on_select.is_some(),
            ),
            Self::ConfirmClose => (
                "set-confirm-close",
                desk.settings.confirm_close,
                Desk::toggle_confirm_close,
                file.confirm_close.is_some(),
            ),
            Self::OptionMeta => (
                "set-option-meta",
                desk.settings.option_as_meta,
                Desk::toggle_option_as_meta,
                file.option_as_meta.is_some(),
            ),
            Self::WheelArrows => (
                "set-wheel-arrows",
                desk.settings.wheel_arrows,
                Desk::toggle_wheel_arrows,
                file.wheel_arrows.is_some(),
            ),
            Self::QuickTerminal => (
                "set-quick-terminal",
                desk.quick_chord.on,
                Desk::toggle_quick_terminal,
                file.quick_terminal.is_some(),
            ),
            _ => (
                "set-notifications",
                desk.settings.notifications,
                Desk::toggle_notifications,
                file.notifications.is_some(),
            ),
        };
        // A switch the file owns says so where its description goes.
        let detail = if managed {
            "Set in config.toml"
        } else {
            self.detail()
        };
        let toggle = controls::Toggle {
            key,
            title: self.title(),
            detail,
            on: value,
            press,
            enabled: true,
        };
        controls::toggle(desk, toggle, t)
    }

    fn link(
        self,
        key: &'static str,
        desk: &Desk,
        value: String,
        press: fn(&mut Desk),
        t: &Tokens,
    ) -> El {
        if desk.phone.is_some() && matches!(self, Self::Theme) {
            return super::theme_row::view(desk, &value, self.detail(), t);
        }
        let s = &t.space;
        pressable(key, t)
            .w_full()
            .min_h(s.touch_min)
            .px(s.s4)
            .py(s.s3)
            .gap(s.s3)
            .items_center()
            .focusable(true)
            .label(self.title())
            .description(self.detail())
            .a11y_value(value.clone())
            .on_press(press)
            .child(self.link_copy(desk, &value, t))
            .children(desk.phone.is_none().then(|| {
                text(value)
                    .font(t.ty.caption)
                    .color(t.roles.text.dim.paint())
                    .max_w(GEOMETRY.value_max)
                    .truncate()
                    .a11y_hidden()
            }))
            // Font cycles in place, so it has nowhere to point.
            .children((!matches!(self, Self::Font)).then(|| chevron(t)))
    }

    fn link_copy(self, desk: &Desk, value: &str, t: &Tokens) -> El {
        let title = text(self.title())
            .font(super::primary(desk, t).medium())
            .color(t.roles.text.text.paint())
            .wrap();
        let heading = if desk.phone.is_some() {
            row()
                .w_full()
                .gap(t.space.s2)
                .flex_wrap()
                .items_center()
                .child(
                    title
                        .w(GEOMETRY.phone_title_min)
                        .min_w(GEOMETRY.phone_title_min)
                        .grow(1.0),
                )
                .child(
                    text(value)
                        .font(super::secondary(desk, t))
                        .color(t.roles.text.dim.paint())
                        .max_w(GEOMETRY.phone_value_max)
                        .wrap_anywhere(),
                )
        } else {
            title
        };
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(t.space.s0_5)
            .child(heading)
            .child(
                text(self.detail())
                    .font(super::secondary(desk, t))
                    .color(t.roles.text.dim.paint())
                    .wrap(),
            )
            .a11y_hidden()
    }
}

fn chevron(t: &Tokens) -> El {
    rune::icon(&icons::CHEVRON_RIGHT, t.icon.size.md)
        .color(t.roles.text.dim.paint())
        .a11y_hidden()
}

fn diagnostics(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    pressable("copy-diagnostics", t)
        .w_full()
        .min_h(s.touch_min)
        .padding(s.s4)
        .focusable(true)
        .label("Copy diagnostics")
        .description(Entry::Diagnostics.detail())
        .on_press_tx(|d: &mut Desk, tx| d.copy_diagnostics(tx))
        .child(
            row()
                .w_full()
                .gap(s.s3)
                .items_center()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(s.s0_5)
                        .child(
                            text(if desk.updater.copied {
                                "Copied"
                            } else {
                                "Copy diagnostics"
                            })
                            .font(super::primary(desk, t).medium())
                            .color(t.roles.text.text.paint()),
                        )
                        .child(
                            text(Entry::Diagnostics.detail())
                                .font(super::secondary(desk, t))
                                .color(t.roles.text.dim.paint())
                                .wrap(),
                        ),
                )
                .child(chevron(t))
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
        Category::About => (desk.native.as_ref())
            .and_then(|native| native.app.clone())
            .unwrap_or_else(|| crate::updates::VERSION.to_owned()),
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
                // Only a window with a quick terminal sets one up.
                .filter(|entry| {
                    !matches!(entry, Entry::QuickTerminal | Entry::QuickChord)
                        || desk.quick_chord.available()
                })
                // A phone doesn't pair a phone, and has no toolbar to hide.
                .filter(|entry| {
                    !matches!(entry, Entry::PairPhone | Entry::HoverToolbar) || desk.phone.is_none()
                })
                .filter(|entry| entry.category() == category)
                .filter(|entry| entry.matches(desk, &query))
                .collect();
            (!entries.is_empty()).then_some((category, entries))
        })
        .collect()
}
