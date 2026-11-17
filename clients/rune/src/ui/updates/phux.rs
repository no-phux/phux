//! One local CLI, managed through its own updater, not a second installation.

use phux_client_runtime::release::Channel;
use rune::{El, col, row, text};

use super::{Desk, Toggle, Tokens, secondary, toggle};
use crate::ui::Paint;
use crate::ui::pressable;

/// The release channel: one choice of three, drawn as an OWN segmented
/// control. Kit `SegmentedControl` is fixed at 28 points, under the 44
/// point target every choice here keeps (DEVELOP.md, "Kit deviations");
/// each choice is a radio, so the group reads as one.
fn channel_picker(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let updater = &desk.updater;
    let enabled = updater.can_choose_phux_channel();
    let choices = [
        ("phux-channel-installed", "Follow installed", None),
        ("phux-channel-stable", "Stable", Some(Channel::Stable)),
        ("phux-channel-next", "Next", Some(Channel::Next)),
    ];
    row()
        .w_full()
        .gap(s.s1)
        .flex_wrap()
        .role("radiogroup")
        .label("phux release channel")
        .children(choices.into_iter().map(|(key, label, channel)| {
            let selected = updater.phux_channel == channel;
            let ink = if !enabled {
                r.text.faint
            } else if selected {
                r.text.text
            } else {
                r.text.dim
            };
            let choice = pressable(key, t)
                .focusable(true)
                .disabled(!enabled)
                .role("radio")
                .label(label)
                .checked(selected)
                .min_h(s.touch_min)
                .px(s.s3)
                .py(s.s1)
                .radius(t.radius.sm)
                .center()
                .border(
                    s.hairline,
                    if enabled {
                        r.border.control.paint()
                    } else {
                        r.border.muted.paint()
                    },
                )
                .on_press(move |d: &mut Desk| d.set_phux_channel(channel))
                .child(
                    text(label)
                        .font(secondary(desk, t))
                        .color(ink.paint())
                        .a11y_hidden(),
                );
            if selected && enabled {
                crate::ui::lifted(choice, t)
            } else {
                choice
            }
        }))
}

pub(super) fn settings(desk: &Desk, t: &Tokens, inset: f32) -> El {
    let s = &t.space;
    let dim = t.roles.text.dim.paint();
    let note = |words: String| text(words).font(secondary(desk, t)).color(dim).wrap();
    let updater = &desk.updater;
    let Some(path) = updater.phux_path() else {
        return note("This window cannot manage a local phux installation. Explicit CLI, socket, or profile selections are never modified.".to_owned());
    };
    let managed = Toggle {
        key: "set-update-phux",
        title: "Update phux with Phux Rune",
        detail: "Uses this CLI's updater and hands the local server off; sessions keep running.",
        on: updater.manage_phux,
        press: Desk::toggle_update_phux,
        enabled: updater.can_toggle_phux_management(),
    };
    let mut body = col()
        .w_full()
        .gap(s.s2)
        .child(
            text(format!("Local phux CLI: {}", path.display()))
                .font(secondary(desk, t))
                .color(dim)
                .wrap_anywhere(),
        )
        .child(toggle(desk, managed, t).px(inset));
    if let Some(cli) = updater.cli.as_ref().filter(|cli| !cli.mutable) {
        let command = cli
            .native_command
            .as_deref()
            .unwrap_or("your package manager");
        return body.child(note(format!("This installation is externally managed. Update it with: {command}. Its release channel is managed outside Rune.")));
    }
    body = body
        .child(
            text("phux release channel")
                .font(secondary(desk, t))
                .color(t.roles.text.text.paint()),
        )
        .child(channel_picker(desk, t))
        .child(note("Stable: numbered releases. Next: the latest green main build. Follow installed keeps the CLI's existing choice. Changing this checks for updates; it does not install anything. Phux Rune itself uses a separate app feed.".to_owned()));
    if updater.staged.is_some() || updater.pending_handoff {
        body = body.child(note(
            "Finish the pending update or server handoff before changing channels.".to_owned(),
        ));
    }
    if let Some(channel) = updater.cli.as_ref().and_then(|cli| cli.channel) {
        body = body.child(note(format!(
            "Last checked phux channel: {}",
            channel.as_str()
        )));
    }
    body
}
