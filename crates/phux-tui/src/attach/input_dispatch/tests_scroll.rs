//! The scroll actions ask the dispatcher to move the focused pane's history
//! by a page, half a page, or to either end.

use crate::layout::ArrangementDoc;

use super::args::bare_action;
use super::test_support::*;

#[test]
fn scroll_actions_ask_for_a_page_half_a_page_or_an_end() {
    let mut f = fx(ArrangementDoc::single(tid(1)));
    let mut rows = |name: &str| {
        f.run(&bare_action(name))
            .scroll_viewport
            .unwrap_or_else(|| panic!("{name} scrolls"))
    };
    let page = rows("scroll-page-down");
    assert!(page > 1);
    assert_eq!(rows("scroll-page-up"), -page);
    assert_eq!(rows("scroll-half-page-down"), page / 2);
    assert_eq!(rows("scroll-half-page-up"), -(page / 2));
    assert!(rows("scroll-to-top") < -1_000_000);
    assert!(rows("scroll-to-bottom") > 1_000_000);
}
