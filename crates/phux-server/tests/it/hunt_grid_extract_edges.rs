//! Edge-of-grid and trim-boundary probes for search + extract coordinate
//! mapping.
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::print_stderr, reason = "probe diagnostics on failure")]

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_server::extract::{extract_match, extract_match_in_scope};
use phux_server::search::{Region, Scope, SearchOptions, search_oneshot};

fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 200,
    })
    .unwrap()
}

const fn vp() -> SearchOptions {
    SearchOptions {
        case_insensitive: false,
        include_viewport: true,
    }
}

// A match that ends exactly at the last grid column (no trailing space).
#[test]
fn probe_match_at_row_end() {
    let mut t = fresh(5, 2);
    t.vt_write(b"abcde"); // fills the whole 5-col row, cursor wraps
    let hits = search_oneshot(&t, "cde", Scope::AllHistory, vp()).unwrap();
    eprintln!("hits = {hits:?}");
    assert_eq!(hits.len(), 1);
    let text = extract_match(&t, hits[0]).unwrap();
    assert_eq!(text, "cde");
}

// A wide glyph occupying the final two columns; match includes it.
#[test]
fn probe_wide_glyph_at_row_end() {
    let mut t = fresh(6, 2);
    // "abcd" + wide "你" exactly fills 6 cols (a,b,c,d,你-wide,tail).
    t.vt_write("abcd你".as_bytes());
    let hits = search_oneshot(&t, "d你", Scope::AllHistory, vp()).unwrap();
    eprintln!("hits = {hits:?}");
    assert_eq!(hits.len(), 1);
    let text = extract_match(&t, hits[0]).unwrap();
    assert_eq!(text, "d你");
}

// 1-column-wide terminal: each row holds at most one cell. Wide glyph cannot
// fit; libghostty handles it (replacement or skip). Just must not panic.
#[test]
fn probe_one_column_terminal() {
    let mut t = fresh(1, 4);
    t.vt_write("a你b".as_bytes());
    let hits = search_oneshot(&t, "a", Scope::AllHistory, vp()).unwrap();
    eprintln!("1-col hits = {hits:?}");
    for m in &hits {
        // Must not panic; text may be "a".
        let _ = extract_match(&t, *m);
    }
}

// Search hit on the OLDEST history row (abs y=0) extracts correctly — the
// boundary of the history coordinate space.
#[test]
fn probe_oldest_history_row_extract() {
    let mut t = fresh(20, 2);
    t.vt_write(b"OLDEST line\r\nmid\r\nnewer\r\nvp1\r\nvp2");
    let hits = search_oneshot(&t, "OLDEST", Scope::AllHistory, vp()).unwrap();
    eprintln!("hits = {hits:?}");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].region, Region::Scrollback);
    assert_eq!(hits[0].row, 0, "OLDEST is the oldest history row");
    let text = extract_match_in_scope(&t, hits[0], Scope::AllHistory).unwrap();
    assert_eq!(text, "OLDEST");
}

// Newest history row (abs y = total-1, just above viewport) extracts right.
#[test]
fn probe_newest_history_row_extract() {
    let mut t = fresh(20, 2);
    t.vt_write(b"h0\r\nh1\r\nNEWEST-hist\r\nvp1\r\nvp2");
    let total = t.scrollback_rows().unwrap();
    eprintln!("total history = {total}");
    let hits = search_oneshot(&t, "NEWEST-hist", Scope::AllHistory, vp()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].region, Region::Scrollback);
    assert_eq!(hits[0].row, total - 1, "NEWEST-hist is just above viewport");
    let text = extract_match_in_scope(&t, hits[0], Scope::AllHistory).unwrap();
    assert_eq!(text, "NEWEST-hist");
}

// Case-insensitive history match with a multibyte glyph: the lowercase fold
// must not shift the char->grid mapping (ASCII fold preserves byte/char
// boundaries).
#[test]
fn probe_case_insensitive_history_with_multibyte() {
    let mut t = fresh(20, 2);
    t.vt_write("éERRORhere\r\nf1\r\nf2\r\nv1".as_bytes());
    let opts = SearchOptions {
        case_insensitive: true,
        include_viewport: false,
    };
    let hits = search_oneshot(&t, "error", Scope::AllHistory, opts).unwrap();
    eprintln!("ci hits = {hits:?}");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].region, Region::Scrollback);
    let text = extract_match_in_scope(&t, hits[0], Scope::AllHistory).unwrap();
    assert_eq!(text, "ERROR", "extracts the original-case run");
}
