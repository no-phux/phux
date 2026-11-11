//! What's new: the bell (the title bar's, or the phone top bar's) and the
//! sheet it opens.

use rune::{El, col, el, text};

use super::overlay::key_hint;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, Sheet, icon_press, icons};
use crate::desk::Desk;
use crate::desk::news::NOTES;

/// What a screen reader says for the bell.
pub fn bell_name(desk: &Desk) -> &'static str {
    if desk.news.unread {
        "What's new, unread"
    } else {
        "What's new"
    }
}

/// The accent dot on a bell while a note is unread, `size` across at `at`.
pub fn unread_dot(desk: &Desk, p: &Ink, at: (f32, f32), size: f32) -> Option<El> {
    desk.news.unread.then(|| {
        el().absolute(at.0, at.1)
            .size(size, size)
            .radius(size / 2.0)
            .fill(p.accent)
            .a11y_hidden()
    })
}

/// The title bar's bell.
pub fn bell(desk: &Desk, p: &Ink) -> El {
    icon_press(
        &icons::BELL,
        bell_name(desk),
        "news-button",
        p,
        Desk::toggle_news,
    )
    .expanded(desk.news.open)
    .children(unread_dot(desk, p, (15.0, 3.0), 6.0))
}

pub fn view(_desk: &Desk, p: &Ink) -> El {
    let body = col()
        .w_full()
        .px(18.0)
        .py(14.0)
        .gap(16.0)
        .children(NOTES.iter().map(|note| {
            col()
                .w_full()
                .gap(3.0)
                .child(text(note.date).font(CAPTION).color(p.dim))
                .child(text(note.title).font(LABEL_STRONG).color(p.text).wrap())
                .child(text(note.body).font(LABEL).color(p.dim).wrap())
        }));
    let sheet = Sheet {
        key: "news",
        title: "What's new",
        width: 440.0,
        top: 96.0,
        scrim: 0.3,
        close: Desk::close_news,
    };
    sheet.build(None, body, key_hint("esc", "close", p), p)
}

#[cfg(test)]
mod tests {
    use crate::desk::Desk;

    #[test]
    fn the_bell_opens_whats_new_and_close_shuts_it() {
        use rune::accesskit::Action;

        for (phone, size, bell) in [
            (false, (1100.0, 720.0), "news-button"),
            (true, (393.0, 852.0), "phone-news"),
        ] {
            let mut desk = Desk::settings_test_fixture().with_phone(phone);
            desk.settings_open = false;
            desk.news.unread = true;
            let mut rt =
                rune::Runtime::new(desk, crate::ui::view, size, rune::Env::new(false, 1.0));
            rt.frame(0.0);
            assert!(rt.accessibility_action(bell, Action::Click, None), "{bell}");
            rt.frame(0.0);
            assert!(rt.state.news.open && !rt.state.news.unread, "{bell}");
            assert!(rt.find("news").is_some(), "{bell}");
            assert!(rt.accessibility_action("news-close", Action::Click, None));
            rt.frame(0.0);
            assert!(!rt.state.news.open, "{bell}");
        }
    }

    #[test]
    fn the_phone_bars_esc_closes_whats_new() {
        use crate::desk::phone::Soft;

        let mut desk = Desk::settings_test_fixture().with_phone(true);
        desk.settings_open = false;
        desk.toggle_news();
        let mut rt = rune::Runtime::new(
            desk,
            crate::ui::view,
            (393.0, 852.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        rt.update(|d, tx| d.soft(Soft::Key(rune::Key::Escape), tx));
        assert!(!rt.state.news.open);
    }
}
