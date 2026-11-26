//! What's new: the notes behind the title bar's bell, and whether this
//! window has seen the newest one.

use super::Desk;

use std::sync::LazyLock;

/// One note: when, a headline, and a sentence or two.
#[derive(serde::Deserialize)]
pub struct Note {
    pub date: String,
    pub title: String,
    pub body: String,
}

/// Newest first, from `news.json`. The release train puts each release's
/// changes on top (`scripts/release.py`); a hand-written note can go there
/// too. The bell lights until the person opens the panel after it ships.
pub static NOTES: LazyLock<Vec<Note>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../news.json")).expect("news.json is a list of notes")
});

const SEEN: &str = "news_seen";

/// What the bell compares against: the newest note's date and title.
fn latest() -> String {
    NOTES
        .first()
        .map(|n| format!("{} {}", n.date, n.title))
        .unwrap_or_default()
}

/// The panel, while open, and whether the bell is lit.
pub struct News {
    pub open: bool,
    pub unread: bool,
}

impl News {
    /// Lit when the newest note arrived after the panel was last opened.
    pub(super) fn load(prefs: &rune::prefs::Prefs) -> Self {
        Self {
            open: false,
            unread: prefs.get::<String>(SEEN) != Some(latest()),
        }
    }
}

impl Desk {
    pub fn toggle_news(&mut self) {
        self.news.open = !self.news.open;
        if self.news.open {
            self.news.unread = false;
            let _ = self.prefs.set(SEEN, &latest());
        }
    }

    pub fn close_news(&mut self) {
        self.news.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_the_panel_puts_the_bell_out_for_good() {
        let mut desk = crate::test_support::settings_window();
        desk.settings_open = false;
        assert!(News::load(&desk.prefs).unread);
        desk.news.unread = true;
        desk.toggle_news();
        assert!(desk.news.open && !desk.news.unread);
        assert!(!News::load(&desk.prefs).unread);
        desk.close_news();
        assert!(!desk.news.open);
    }

    #[test]
    fn notes_run_newest_first() {
        assert!(NOTES.windows(2).all(|w| w[0].date >= w[1].date));
    }
}
