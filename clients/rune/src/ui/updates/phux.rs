//! One local CLI, managed through its own updater, not a second installation.

use phux_client_runtime::release::Channel;
use rune::{El, col, row, text};

use super::{CAPTION, Desk, Ink, secondary, toggle};
use crate::ui::pressable;

fn channel_picker(desk: &Desk, ink: &Ink) -> El {
    let updater = &desk.updater;
    let choices = [
        ("phux-channel-installed", "Follow installed", None),
        ("phux-channel-stable", "Stable", Some(Channel::Stable)),
        ("phux-channel-next", "Next", Some(Channel::Next)),
    ];
    row()
        .w_full()
        .gap(8.0)
        .flex_wrap()
        .role("group")
        .label("phux release channel")
        .children(choices.into_iter().map(|(key, label, channel)| {
            let selected = updater.phux_channel == channel;
            pressable(key, ink)
                .focusable(true)
                .disabled(!updater.can_choose_phux_channel())
                .label(label)
                .checked(selected)
                .min_h(44.0)
                .px(12.0)
                .py(6.0)
                .radius(6.0)
                .border(1.0, if selected { ink.accent } else { ink.separator })
                .on_press(move |d: &mut Desk| d.set_phux_channel(channel))
                .child(text(label).font(secondary(desk, CAPTION)).color(ink.text))
        }))
}

pub(super) fn settings(desk: &Desk, ink: &Ink, inset: f32) -> El {
    let updater = &desk.updater;
    let Some(path) = updater.phux_path() else {
        return text("This window cannot manage a local phux installation. Explicit CLI, socket, or profile selections are never modified.")
            .font(secondary(desk, CAPTION)).color(ink.dim).wrap();
    };
    let mut body = col()
        .w_full()
        .gap(8.0)
        .child(
            text(format!("Local phux CLI: {}", path.display()))
                .font(secondary(desk, CAPTION))
                .color(ink.dim)
                .wrap_anywhere(),
        )
        .child(
            toggle(
                desk,
                "set-update-phux",
                "Update phux with Phux Rune",
                "Uses this CLI's updater and hands the local server off; sessions keep running.",
                updater.manage_phux,
                Desk::toggle_update_phux,
                ink,
            )
            .px(inset)
            .disabled(!updater.can_toggle_phux_management()),
        );
    if let Some(cli) = updater.cli.as_ref().filter(|cli| !cli.mutable) {
        let command = cli
            .native_command
            .as_deref()
            .unwrap_or("your package manager");
        return body.child(text(format!("This installation is externally managed. Update it with: {command}. Its release channel is managed outside Rune."))
            .font(secondary(desk, CAPTION)).color(ink.dim).wrap());
    }
    body = body
        .child(text("phux release channel").font(secondary(desk, CAPTION)).color(ink.text))
        .child(channel_picker(desk, ink))
        .child(text("Stable: numbered releases. Next: the latest green main build. Follow installed keeps the CLI's existing choice. Changing this checks for updates; it does not install anything. Phux Rune itself uses a separate app feed.")
            .font(secondary(desk, CAPTION)).color(ink.dim).wrap());
    if updater.staged.is_some() || updater.pending_handoff {
        body = body.child(
            text("Finish the pending update or server handoff before changing channels.")
                .font(secondary(desk, CAPTION))
                .color(ink.dim)
                .wrap(),
        );
    }
    if let Some(channel) = updater.cli.as_ref().and_then(|cli| cli.channel) {
        body = body.child(
            text(format!("Last checked phux channel: {}", channel.as_str()))
                .font(secondary(desk, CAPTION))
                .color(ink.dim)
                .wrap(),
        );
    }
    body
}
