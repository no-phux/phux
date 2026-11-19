//! What's new: the bell (the title bar's, or the phone top bar's) and the
//! sheet it opens.

use crate::ui::tokens::Voice;
use phux_design::Tone;
use rune::{El, col, el, text};

use super::overlay::{EXTENTS, key_hint};
use super::tokens::{Paint, Tokens};
use super::{Sheet, icon_press, icons};
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

/// The bell's own dot while a note is unread, `size` across at `at`: the
/// bell's tone, and no word, since the bell's name says it.
pub fn unread_dot(desk: &Desk, p: &Tokens, at: (f32, f32), size: f32) -> Option<El> {
    let t = p;
    desk.news.unread.then(|| {
        el().absolute(at.0, at.1)
            .w(size)
            .h(size)
            .radius(t.radius.full)
            .fill(t.status.tone(Tone::Bell).ink)
            .a11y_hidden()
    })
}

/// The title bar's bell.
pub fn bell(desk: &Desk, p: &Tokens) -> El {
    let t = p;
    let s = &t.space;
    // Over the button's top-right corner, cut out of the bar by a ring of
    // its ground.
    let size = s.s2;
    let at = (s.pointer_min - size - s.hairline, s.s0_5 + s.hairline);
    icon_press(
        &icons::BELL,
        bell_name(desk),
        "news-button",
        p,
        Desk::toggle_news,
    )
    .expanded(desk.news.open)
    .help("What's new")
    .children(
        unread_dot(desk, p, at, size).map(|dot| dot.border(s.s0_5, t.roles.layer.sidebar.paint())),
    )
}

pub fn view(desk: &Desk, p: &Tokens) -> El {
    let t = p;
    let s = &t.space;
    let r = &t.roles;
    let body = col()
        .w_full()
        .px(s.sheet_pad)
        .py(s.s3)
        .gap(s.s4)
        .children(NOTES.iter().map(|note| {
            col()
                .w_full()
                .gap(s.s1)
                .child(text(note.date).font(t.ty.caption).color(r.text.dim.paint()))
                .child(
                    text(note.title)
                        .font(t.ty.label.semibold())
                        .color(r.text.text.paint())
                        .wrap(),
                )
                .child(
                    text(note.body)
                        .font(t.ty.label)
                        .color(r.text.dim.paint())
                        .wrap(),
                )
        }));
    let sheet = Sheet {
        key: "news",
        title: "What's new",
        width: EXTENTS.news_w,
        top: s.s12 + s.s12,
        close: Desk::close_news,
    };
    // A phone has no Escape: its sheet closes from the header's button.
    let foot = desk.phone.is_none().then(|| key_hint("esc", "close", p));
    sheet.build(None, body, foot, p)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_bell_opens_whats_new_and_close_shuts_it() {
        use rune::accesskit::Action;

        for (phone, size, bell) in [
            (false, (1100.0, 720.0), "news-button"),
            (true, (393.0, 852.0), "phone-news"),
        ] {
            let mut desk = crate::test_support::settings_window().with_phone(phone);
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

        let mut desk = crate::test_support::settings_window().with_phone(true);
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
