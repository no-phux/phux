//! `SnapshotSynthesizer` round-trips plus search/extract edge probes, all on
//! bare libghostty terminals (no PTY, no server).
//!
//! The incremental contract (ADR-0018): a clean post-ack terminal emits
//! nothing; an unacked diff stays re-emittable (loss tolerance); `Dirty::Full`
//! falls back to the full reset path; and the per-consumer reference diff,
//! applied to a mirror, reconstructs the source across churn and resizes.

use phux_server::extract::{extract_match, extract_match_in_scope};
use phux_server::grid::{ConsumerReference, SCROLLBACK_ALL, SnapshotSynthesizer, synthesize};
use phux_server::search::{Region, Scope, SearchOptions, search_oneshot};

use super::common::{fresh, render_grid};

const VIEWPORT: SearchOptions = SearchOptions {
    case_insensitive: false,
    include_viewport: true,
};

#[test]
fn incremental_clean_after_ack_is_empty() {
    let mut canonical = fresh(20, 5);
    canonical.vt_write(b"hello world");
    let mut synth = SnapshotSynthesizer::new().unwrap();
    let baseline = synth.synthesize(&canonical).unwrap();
    assert!(!baseline.bytes.is_empty());
    synth.mark_synced(&canonical).unwrap();

    let incremental = synth.synthesize_incremental(&canonical).unwrap();
    assert_eq!(
        (incremental.cols, incremental.rows),
        (baseline.cols, baseline.rows)
    );
    assert!(
        incremental.bytes.is_empty(),
        "post-ack Clean must emit nothing"
    );
}

/// Two unacked emissions of the same change must each bring a baseline
/// mirror to the canonical grid: the synthesizer never clears dirty state on
/// emission, only on ack, so a lost frame can be re-sent.
#[test]
fn unacked_incremental_diffs_stay_reemittable_and_converge() {
    let mut canonical = fresh(20, 5);
    canonical.vt_write(b"first");
    let mut synth = SnapshotSynthesizer::new().unwrap();
    let baseline = synth.synthesize(&canonical).unwrap();
    let mut mirrors = [
        fresh(baseline.cols, baseline.rows),
        fresh(baseline.cols, baseline.rows),
    ];
    for m in &mut mirrors {
        m.vt_write(&baseline.bytes);
    }
    synth.mark_synced(&canonical).unwrap();

    canonical.vt_write(b"\r\nsecond");
    for mirror in &mut mirrors {
        let diff = synth.synthesize_incremental(&canonical).unwrap();
        assert!(!diff.bytes.is_empty(), "an unacked diff must be re-emitted");
        mirror.vt_write(&diff.bytes);
        assert_eq!(render_grid(&canonical), render_grid(mirror));
    }
}

#[test]
fn incremental_full_dirty_matches_full_synthesis() {
    let mut canonical = fresh(20, 5);
    canonical.vt_write(b"primary\x1b[?1049halt content");
    let full = SnapshotSynthesizer::new()
        .unwrap()
        .synthesize(&canonical)
        .unwrap();
    let incremental = SnapshotSynthesizer::new()
        .unwrap()
        .synthesize_incremental(&canonical)
        .unwrap();
    assert!(full.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));
    assert_eq!(full.bytes, incremental.bytes);
    let mut mirror = fresh(full.cols, full.rows);
    mirror.vt_write(&full.bytes);
    assert_eq!(render_grid(&canonical), render_grid(&mirror));
}

/// Mirror primed from `synthesize`, fed each reference diff, must equal the
/// source after every step: churn, alt-screen enter/leave, grow resize, and
/// sparse edits across distant rows.
#[test]
fn reference_diff_roundtrips_through_churn_alt_screen_and_resize() {
    enum Step {
        Write(Vec<u8>),
        Resize(u16, u16),
    }
    let sparse: Vec<u8> = [5u16, 15, 25]
        .iter()
        .flat_map(|r| format!("\x1b[{};1H\x1b[2KEDIT-{r}", r + 1).into_bytes())
        .collect();
    let steps = [
        Step::Write(b"\r\nmore primary output".to_vec()),
        Step::Write(b"\x1b[?1049h\x1b[2J\x1b[Halt screen!!!\r\nalt line two".to_vec()),
        Step::Write(b"\x1b[?1049l\r\nback on primary".to_vec()),
        Step::Resize(40, 30),
        Step::Write(b"\r\nafter grow".to_vec()),
        Step::Write(sparse),
    ];

    let mut src = fresh(40, 10);
    src.vt_write(b"initial primary content\r\nsecond line");
    let snap = synthesize(&src).unwrap();
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);
    let mut synth = SnapshotSynthesizer::new().unwrap();
    let mut reference = ConsumerReference::new();
    synth.prime_reference(&src, &mut reference).unwrap();

    for (n, step) in steps.iter().enumerate() {
        match step {
            Step::Write(bytes) => src.vt_write(bytes),
            Step::Resize(cols, rows) => src.resize(*cols, *rows, 8, 16).unwrap(),
        }
        let diff = synth
            .synthesize_against_reference(&src, &mut reference)
            .unwrap();
        mirror.resize(diff.cols, diff.rows, 8, 16).unwrap();
        mirror.vt_write(&diff.bytes);
        assert_eq!(
            render_grid(&src),
            render_grid(&mirror),
            "diverged after step {n}"
        );
    }
    let unchanged = synth
        .synthesize_against_reference(&src, &mut reference)
        .unwrap();
    assert!(unchanged.bytes.is_empty(), "no change -> empty diff");

    src.resize(10, 3, 8, 16).unwrap();
    let shrunk = synth
        .synthesize_against_reference(&src, &mut reference)
        .unwrap();
    assert_eq!((shrunk.cols, shrunk.rows), (10, 3));
}

/// Scrollback projection equals what libghostty retains and clamps
/// oversized requests, never over-reading.
#[test]
fn scrollback_projection_is_bounded_by_retained_history() {
    let mut t = libghostty_vt::Terminal::new(10, 2).unwrap();
    t.set_scrollback_max_lines(Some(5)).unwrap();
    for i in 0..100u32 {
        t.vt_write(format!("L{i}\r\n").as_bytes());
    }
    let total = t.scrollback_rows().unwrap();
    let synth = SnapshotSynthesizer::new().unwrap();
    for want in [SCROLLBACK_ALL, u32::MAX] {
        let screen = synth
            .screen_state_with_scrollback(&t, 0, Some(want), false)
            .unwrap();
        assert_eq!(screen.scrollback.len(), total, "request {want}");
    }
}

/// Search hits map back to the exact source text at row ends, across wide
/// glyphs, ZWJ and combining clusters, and at both history boundaries.
#[test]
fn search_hits_extract_their_exact_text_at_edges() {
    struct Probe {
        cols: u16,
        vt: &'static str,
        needle: &'static str,
        case_insensitive: bool,
        expected: &'static str,
    }
    let probe = |cols, vt, needle, expected| Probe {
        cols,
        vt,
        needle,
        case_insensitive: false,
        expected,
    };
    let probes = [
        probe(5, "abcde", "cde", "cde"),
        probe(6, "abcd你", "d你", "d你"),
        probe(20, "👨‍👩‍👧END", "END", "END"),
        probe(20, "e\u{301}xy", "xy", "xy"),
        Probe {
            case_insensitive: true,
            ..probe(20, "éERRORhere\r\nf1\r\nf2\r\nv1", "error", "ERROR")
        },
    ];
    for p in probes {
        let mut t = fresh(p.cols, 2);
        t.vt_write(p.vt.as_bytes());
        let opts = SearchOptions {
            case_insensitive: p.case_insensitive,
            include_viewport: true,
        };
        let hits = search_oneshot(&t, p.needle, Scope::AllHistory, opts).unwrap();
        assert_eq!(hits.len(), 1, "{}: {hits:?}", p.needle);
        let text = extract_match_in_scope(&t, hits[0], Scope::AllHistory).unwrap();
        assert_eq!(text, p.expected);
    }

    // History coordinates: the oldest row is 0, the newest is total - 1.
    let mut t = fresh(20, 2);
    t.vt_write(b"OLDEST\r\nh1\r\nNEWEST\r\nvp1\r\nvp2");
    let total = t.scrollback_rows().unwrap();
    for (needle, row) in [("OLDEST", 0), ("NEWEST", total - 1)] {
        let hit = search_oneshot(&t, needle, Scope::AllHistory, VIEWPORT).unwrap()[0];
        assert_eq!((hit.region, hit.row), (Region::Scrollback, row), "{needle}");
        assert_eq!(
            extract_match_in_scope(&t, hit, Scope::AllHistory).unwrap(),
            needle
        );
    }

    // A 1-column grid cannot hold a wide glyph; extraction must not panic.
    let mut t = fresh(1, 4);
    t.vt_write("a你b".as_bytes());
    for hit in search_oneshot(&t, "a", Scope::AllHistory, VIEWPORT).unwrap() {
        let _ = extract_match(&t, hit);
    }
}
