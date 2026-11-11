//! What's new: the notes behind the title bar's bell, and whether this
//! window has seen the newest one.

use super::Desk;

/// One note: when, a headline, and a sentence or two.
pub struct Note {
    pub date: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

/// Newest first. A new note goes on top, and
/// the bell lights until the person opens the panel after it ships.
pub const NOTES: &[Note] = &[
    Note {
        date: "2026-11-10",
        title: "Pair your phone by scanning",
        body: "Pairing the phone client starts with the camera: scan the code \
               and you're in, no typing an address.",
    },
    Note {
        date: "2026-11-09",
        title: "Kitchen, pit lane, pitch and hive",
        body: "Four new playground views from the palette. Every agent is a \
               chef, a car, a player or a bee; one that needs you says so, \
               and a press shows its terminal. Escape or the esc chip closes \
               any of them.",
    },
    Note {
        date: "2026-11-09",
        title: "Screen mirrors on demand",
        body: "List a machine's screens and start a mirror when you want one, \
               and trade clipboards between a viewer and the mirrored host.",
    },
    Note {
        date: "2026-11-09",
        title: "Release channels in Settings",
        body: "About & Updates picks Stable or Next for the local phux, and \
               Linux gets the same settings as macOS.",
    },
    Note {
        date: "2026-11-08",
        title: "A signed Mac app and Linux arm64",
        body: "Rune ships as a signed DMG, and Linux builds now cover arm64 \
               as well as x86_64.",
    },
    Note {
        date: "2026-11-08",
        title: "Satellite sessions in the sidebar",
        body: "A hub lists its satellites' sessions and mirrors alongside its \
               own, so another machine's work is one click away.",
    },
    Note {
        date: "2026-11-07",
        title: "Reef, galaxy and creatures",
        body: "The fleet as a reef or a galaxy, with creatures that grow as \
               your agents get work done.",
    },
    Note {
        date: "2026-11-07",
        title: "Answer agents from the inbox",
        body: "ACP agent questions show up in the inbox, even ones asked \
               before Rune connected, and you answer them right there.",
    },
];

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
        let mut desk = Desk::settings_test_fixture();
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
