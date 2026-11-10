//! What's new: the title bar's bell and the sheet it opens.

use rune::{El, col, el, text};

use super::overlay::key_hint;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, Sheet, icon_press, icons};
use crate::desk::Desk;
use crate::desk::news::NOTES;

/// The bell, with an accent dot while a note is unread.
pub fn bell(desk: &Desk, p: &Ink) -> El {
    let name = if desk.news.unread {
        "What's new, unread"
    } else {
        "What's new"
    };
    icon_press(&icons::BELL, name, "news-button", p, Desk::toggle_news)
        .expanded(desk.news.open)
        .children(desk.news.unread.then(|| {
            el().absolute(15.0, 3.0)
                .size(6.0, 6.0)
                .radius(3.0)
                .fill(p.accent)
                .a11y_hidden()
        }))
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

        let mut desk = Desk::settings_test_fixture();
        desk.settings_open = false;
        desk.news.unread = true;
        let mut rt = rune::Runtime::new(
            desk,
            crate::ui::view,
            (1100.0, 720.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        assert!(rt.accessibility_action("news-button", Action::Click, None));
        rt.frame(0.0);
        assert!(rt.state.news.open && !rt.state.news.unread);
        assert!(rt.find("news").is_some());
        assert!(rt.accessibility_action("news-close", Action::Click, None));
        rt.frame(0.0);
        assert!(!rt.state.news.open);
    }
}
