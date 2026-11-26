//! What a native host does for the runtime it embeds: it owns the
//! pasteboard, the link opener, haptics and the screen outside the surface.

use serde_json::{Value, json};

use super::State;

/// Called before a changed native viewport reaches Rune. Restart the grid's
/// wait before poll can send an old proposal ahead of the new layout.
pub fn viewport_changed(rt: &mut rune::Runtime<State>) {
    rt.update(|state, _| state.phone_viewport_changed());
}

/// What the last frame asked of the system. An embedded runtime reaches no
/// pasteboard, link opener or haptics; the host that owns them answers.
/// Every copy here is plain: this app writes no secret to the clipboard.
pub fn take_requests(rt: &mut rune::Runtime<State>) -> Value {
    use crate::desk::phone::touch::Feedback;
    let menu = crate::desk::phone::navigation::native::export(rt);
    let menu_request = crate::desk::phone::navigation::native::current(&mut rt.state);
    let copied = rt.copy_request.take();
    let copied = copied.as_ref().map(|write| write.content().text());
    let urls = rt.take_url_requests();
    let state = &mut rt.state;
    let presence = state.take_presence();
    let feedback = state.phone.as_mut().and_then(|phone| phone.feedback.take());
    let feedback = feedback.map(|feedback| match feedback {
        Feedback::Hold => "hold",
        Feedback::Copied => "copied",
        Feedback::Detent => "detent",
        Feedback::Threshold => "threshold",
    });
    let attached = state.connection.status == phux_client_runtime::control::Status::Attached;
    let native = state.native.as_mut();
    let (connection, paste, attach, attaching) =
        native.map_or((false, false, false, None), |native| {
            let attaching = native.attaching.front().map(|asked| asked.id);
            (
                std::mem::take(&mut native.connection_requested),
                std::mem::take(&mut native.paste_requested),
                std::mem::take(&mut native.attach_requested),
                attaching,
            )
        });
    // Where the menu opens from: the paperclip, or a hardware key's nowhere.
    let attach = attach.then(|| {
        rt.presented("phone-attach")
            .map_or([0.0; 4], |(rect, _)| rect)
    });
    // `attaching` names the pane a menu's picks or a pasted image answer.
    let attaching = attaching.filter(|_| paste || attach.is_some());
    json!({"connection": connection, "paste": paste, "attach": attach,
        "attaching": attaching, "copied_text": copied, "menu": menu, "menu_request": menu_request,
        "open_urls": urls, "feedback": feedback, "presence": presence, "attached": attached})
}

/// A tap on the phone's card, widget or Dynamic Island: `phux://inbox`, or a
/// pane to show once the computer's sessions are known. The host holds it
/// until attached. False for any other link, which is the host's to handle.
pub fn open_link(rt: &mut rune::Runtime<State>, url: &str) -> bool {
    use crate::desk::presence::INBOX;
    use crate::desk::reveal::{Link, target};
    let inbox = url.trim_end_matches('/') == INBOX;
    if !inbox && !matches!(target(url), Some(Ok(Link::Pane(_)))) {
        return false;
    }
    rt.update(|state, tx| {
        if !inbox {
            state.close_inbox();
            state.open_system_link(url);
        } else if !state.inbox.open {
            state.close_phone_navigation();
            state.toggle_inbox();
        }
        tx.instant();
    });
    true
}

/// Where the phone's Live Activity gateway takes agent counts for it, and
/// its grant there; written to every computer it attaches to.
pub fn set_live(rt: &mut rune::Runtime<State>, device: &str, gateway: &str, grant: &str) {
    rt.update(|state, _| {
        if let Some(native) = state.native.as_mut() {
            let live =
                crate::desk::presence::Live::new(device.into(), gateway.into(), grant.into());
            native.live = Some(live);
        }
    });
}

/// The host's pasteboard text, answering a `paste` request. `None`: it
/// held more than the bridge carries, which is said rather than dropped.
pub fn paste(rt: &mut rune::Runtime<State>, text: Option<&str>) {
    rt.update(|state, tx| {
        match text {
            Some(text) => state.paste_from_host(text),
            None => state.paste_too_large(),
        }
        tx.instant();
    });
}

/// One file for the attachment request `id`, or `None` when the host
/// couldn't read it.
pub fn attach(rt: &mut rune::Runtime<State>, id: u64, extension: &str, data: Option<Vec<u8>>) {
    rt.update(|state, tx| {
        state.attach_from_host(id, extension, data);
        tx.instant();
    });
}

/// The host saw the network path change (Wi-Fi to cellular, a VPN coming
/// up): move the connection to it and probe, instead of waiting out the
/// quiet clock on a socket that may be dead.
pub fn path_changed(rt: &mut rune::Runtime<State>) {
    rt.update(|state, _| state.resume_link(None));
}

/// The system's accessibility settings Rune has no API for:
/// Reduce Transparency and the text size, as a `UIContentSizeCategory`
/// raw value. The phone's tokens read them; a change draws a frame.
pub fn set_accessibility(
    rt: &mut rune::Runtime<State>,
    reduce_transparency: bool,
    content_size: &str,
) {
    rt.update_if(|state, tx| {
        let changed = state.set_accessibility(reduce_transparency, content_size);
        if changed {
            tx.instant();
        }
        changed
    });
}

/// One step of text size from the host's pinch, kept like any other.
pub fn zoom(rt: &mut rune::Runtime<State>, larger: bool) {
    use crate::keys::Shortcut;
    let step = if larger {
        Shortcut::ZoomIn
    } else {
        Shortcut::ZoomOut
    };
    rt.update(|state, tx| state.shortcut(step, tx));
}

/// The surface's edge colour as sRGB, so UIKit can paint the status-bar and
/// home-indicator strips it keeps outside the surface as one ground.
pub fn edge_srgb(rt: &rune::Runtime<State>) -> [f32; 3] {
    srgb(crate::ui::phone_edge_color(&rt.state))
}

/// The floating controls UIKit draws as Liquid Glass this frame, where the
/// frame drew them (view points) and how opaque: `[]` off the native host
/// or where glass may not draw. A control something else covers (a sheet,
/// a page, a menu) is left out, so its glass never floats over a modal or
/// takes a tap meant for it.
pub fn controls(rt: &rune::Runtime<State>) -> Value {
    let Some(native) = rt.state.native.as_ref() else {
        return json!([]);
    };
    let controls = native.controls.borrow();
    let shown = controls.iter().filter_map(|control| {
        let ([x, y, w, h], opacity) = rt.presented(&control.key)?;
        let centre = rt.element_at(x + w / 2.0, y + h / 2.0)?;
        let uncovered = centre.key() == Some(control.key.as_str());
        (uncovered && opacity > 0.01 && w >= 1.0 && h >= 1.0).then(|| {
            json!({"id": control.key, "rect": [x, y, w, h], "opacity": opacity,
                "label": control.label, "title": control.title, "symbol": control.symbol,
                "subtitle": control.subtitle.as_ref().map(|(text, _)| text),
                "subtitle_ink": control.subtitle.as_ref().map(|(_, ink)| srgb(*ink)),
                "leading": control.leading,
                "enabled": control.enabled, "dark": control.dark, "ink": srgb(control.ink),
                "badge": control.badge.map(srgb), "painted": control.painted})
        })
    });
    // Reading order, so VoiceOver meets them as the eye does.
    let mut shown: Vec<Value> = shown.collect();
    // A row's controls share a centre line whatever their heights.
    shown.sort_by_key(|c| {
        let r = |i: usize| c["rect"][i].as_f64().unwrap_or_default();
        ((r(1) + r(3) / 2.0).round() as i64, r(0).round() as i64)
    });
    Value::Array(shown)
}

/// The host's tap on native control `key`: run its press, as an
/// accessibility press would, so a covered or inert control does nothing.
/// False when this frame showed no such control.
pub fn press(rt: &mut rune::Runtime<State>, key: &str) -> bool {
    let known = rt
        .state
        .native
        .as_ref()
        .is_some_and(|native| native.controls.borrow().iter().any(|c| c.key == key));
    known && rt.accessibility_action(key, rune::accesskit::Action::Click, None)
}

/// A linear colour as sRGB, for UIKit.
fn srgb(color: rune::Color) -> [f32; 3] {
    [color.r, color.g, color.b].map(|linear| {
        let linear = linear.clamp(0.0, 1.0);
        if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desk::phone::touch::Feedback;

    fn runtime() -> rune::Runtime<State> {
        let target = phux_client_runtime::Target::uds("/tmp/not-used");
        crate::test_support::runtime(
            super::super::with_target(target, None),
            crate::ui::view,
            (393.0, 852.0),
            rune::Env::new(true, 2.0),
        )
    }

    #[test]
    fn edge_colour_is_the_settings_page_or_the_terminal_ground_as_srgb() {
        crate::test_support::isolated_preferences(
            "mobile::host::tests::edge_colour_is_the_settings_page_or_the_terminal_ground_as_srgb",
            || {
                let mut rt = runtime();
                // No terminal yet: the theme's own ground, encoded for UIKit.
                let ground = edge_srgb(&rt);
                let linear = crate::ui::phone_edge_color(&rt.state);
                assert!(ground.iter().all(|c| (0.0..=1.0).contains(c)), "{ground:?}");
                let lifted = ground.iter().zip([linear.r, linear.g, linear.b]);
                assert!(
                    lifted.into_iter().all(|(srgb, linear)| *srgb >= linear),
                    "sRGB encoding never darkens a linear value"
                );
                rt.update(|state, _| state.settings_open = true);
                assert_ne!(edge_srgb(&rt), ground, "Settings paints its own page");
            },
        );
    }

    #[test]
    fn a_tapped_card_opens_the_inbox_or_its_pane_and_nothing_else() {
        let mut rt = runtime();
        assert!(open_link(&mut rt, "phux://inbox"));
        assert!(open_link(&mut rt, "phux://inbox/"), "open already");
        assert!(rt.state.inbox.open);
        assert!(open_link(&mut rt, "phux://pane/@3"));
        assert!(!rt.state.inbox.open);
        for other in [
            "phux://connect?quic=quic://mini:8788",
            "phux://folder/tmp",
            "phux://session/work",
            "phux://pane/3",
        ] {
            assert!(!open_link(&mut rt, other), "{other}");
        }
    }

    #[test]
    fn a_pinch_steps_the_text_size_both_ways() {
        // Its own preferences: a text size is saved as it changes.
        let mut rt = crate::test_support::runtime(
            crate::test_support::settings_window(),
            crate::ui::view,
            (393.0, 852.0),
            rune::Env::new(true, 2.0),
        );
        let size = rt.state.metrics().cell_h;
        zoom(&mut rt, true);
        assert!(rt.state.metrics().cell_h > size);
        zoom(&mut rt, false);
        zoom(&mut rt, false);
        assert!(rt.state.metrics().cell_h < size);
    }

    #[test]
    fn a_frames_requests_are_taken_once_and_copies_leave_as_plain_text() {
        crate::test_support::isolated_preferences(
            "mobile::host::tests::a_frames_requests_are_taken_once_and_copies_leave_as_plain_text",
            || {
                let mut rt = runtime();
                let quiet = json!({"connection": false, "paste": false, "attach": null,
            "attaching": null, "copied_text": null, "menu": null, "menu_request": null,
            "open_urls": [], "feedback": null, "presence": null, "attached": false});
                assert_eq!(take_requests(&mut rt), quiet);
                rt.update(|state, tx| {
                    state.toggle_machines();
                    state.phone.as_mut().unwrap().feedback = Some(Feedback::Hold);
                    tx.copy("selected words".to_owned());
                    let native = state.native.as_mut().unwrap();
                    native.paste_requested = true;
                });
                assert_eq!(
                    take_requests(&mut rt),
                    json!({"connection": true, "paste": true, "attach": null,
                "attaching": null, "copied_text": "selected words", "menu": null, "menu_request": null,
                "open_urls": [], "feedback": "hold", "presence": null,
                "attached": false})
                );
                assert_eq!(take_requests(&mut rt), quiet, "each request is served once");
            },
        );
    }
}
