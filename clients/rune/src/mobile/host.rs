//! What a native host does for the runtime it embeds: it owns the
//! pasteboard, the link opener, haptics and the screen outside the surface.

use serde_json::{Value, json};

use super::State;

/// What the last frame asked of the system. An embedded runtime reaches no
/// pasteboard, link opener or haptics; the host that owns them answers.
/// Every copy here is plain: this app writes no secret to the clipboard.
pub fn take_requests(rt: &mut rune::Runtime<State>) -> Value {
    use crate::desk::phone::touch::Feedback;
    let copied = rt.copy_request.take();
    let copied = copied.as_ref().map(|write| write.content().text());
    let urls = rt.take_url_requests();
    let state = &mut rt.state;
    let feedback = state.phone.as_mut().and_then(|phone| phone.feedback.take());
    let feedback = feedback.map(|feedback| match feedback {
        Feedback::Hold => "hold",
        Feedback::Copied => "copied",
    });
    let native = state.native.as_mut();
    let (connection, paste) = native.map_or((false, false), |native| {
        (
            std::mem::take(&mut native.connection_requested),
            std::mem::take(&mut native.paste_requested),
        )
    });
    json!({"connection": connection, "paste": paste, "copied_text": copied,
        "open_urls": urls, "feedback": feedback})
}

/// The host's pasteboard text, answering a `paste` request.
pub fn paste(rt: &mut rune::Runtime<State>, text: &str) {
    rt.update(|state, tx| {
        state.paste_from_host(text);
        tx.instant();
    });
}

/// The surface's edge colour as sRGB, so UIKit can paint the status-bar and
/// home-indicator strips it keeps outside the surface as one ground.
pub fn edge_srgb(rt: &rune::Runtime<State>, dark: bool) -> [f32; 3] {
    let color = crate::ui::phone_edge_color(&rt.state, dark);
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
        rune::Runtime::new(
            super::super::with_target(target, None),
            crate::ui::view,
            (393.0, 852.0),
            rune::Env::new(true, 2.0),
        )
    }

    #[test]
    fn edge_colour_is_the_settings_page_or_the_terminal_ground_as_srgb() {
        let mut rt = runtime();
        // No terminal yet: the theme's own ground, encoded for UIKit.
        let ground = edge_srgb(&rt, true);
        let linear = crate::ui::phone_edge_color(&rt.state, true);
        assert!(ground.iter().all(|c| (0.0..=1.0).contains(c)), "{ground:?}");
        assert!(
            ground[2] > linear.b,
            "sRGB encoding lifts a dark linear value"
        );
        rt.update(|state, _| state.settings_open = true);
        assert_ne!(edge_srgb(&rt, true), ground, "Settings paints its own page");
    }

    #[test]
    fn a_frames_requests_are_taken_once_and_copies_leave_as_plain_text() {
        let mut rt = runtime();
        let quiet = json!({"connection": false, "paste": false, "copied_text": null,
            "open_urls": [], "feedback": null});
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
            json!({"connection": true, "paste": true, "copied_text": "selected words",
                "open_urls": [], "feedback": "hold"})
        );
        assert_eq!(take_requests(&mut rt), quiet, "each request is served once");
    }
}
