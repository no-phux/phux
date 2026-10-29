use super::*;

fn owned(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|s| (*s).to_owned()).collect()
}

/// An older payload without any additive key still deserializes, and
/// reports no wrap information.
#[test]
fn deserializes_v1_json_without_additive_keys() {
    let v1 = r#"{"schema_version":1,"pane":3,"cols":80,"rows":2,"cursor":null,"lines":["hello","world"]}"#;
    let screen: ScreenState = serde_json::from_str(v1).expect("v1 JSON must deserialize");
    assert_eq!(screen.lines, owned(&["hello", "world"]));
    assert_eq!(
        screen,
        ScreenState {
            schema_version: 1,
            pane: 3,
            cols: 80,
            rows: 2,
            lines: owned(&["hello", "world"]),
            ..ScreenState::default()
        }
    );
    assert!(!screen.has_soft_wrap_info());
}

/// Soft-wrapped runs join into logical lines (ADR-0077 §2): within the
/// viewport, across the scrollback seam, at the stream end, and with
/// malformed indices ignored.
#[test]
fn unwraps_soft_wrapped_runs() {
    type Case<'a> = (
        &'a [&'a str],
        &'a [&'a str],
        &'a [u32],
        &'a [u32],
        &'a [&'a str],
    );
    let cases: &[Case<'_>] = &[
        (
            &[],
            &["the quick", "brown fox", "next"],
            &[],
            &[0],
            &["the quickbrown fox", "next"],
        ),
        (
            &[],
            &["aaa", "bbb", "ccc", "ddd"],
            &[],
            &[0, 1],
            &["aaabbbccc", "ddd"],
        ),
        (
            &["old", "hist"],
            &["ory", "live"],
            &[1],
            &[],
            &["old", "history", "live"],
        ),
        (&[], &["only"], &[], &[0], &["only"]),
        (&[], &["a", "b"], &[], &[], &["a", "b"]),
        (&[], &["a", "b"], &[9], &[7, 0], &["ab"]),
    ];
    for (scrollback, lines, wrapped_sb, wrapped_lines, want) in cases {
        let screen = ScreenState {
            lines: owned(lines),
            scrollback: owned(scrollback),
            soft_wrap: Some(SoftWrap {
                lines: wrapped_lines.to_vec(),
                scrollback: wrapped_sb.to_vec(),
            }),
            ..ScreenState::default()
        };
        assert!(screen.has_soft_wrap_info());
        assert_eq!(screen.unwrapped_rows(), owned(want), "case {lines:?}");
    }

    // A run straddling the seam lands in the half where it ends.
    let seam = ScreenState {
        lines: owned(&["ory", "live"]),
        scrollback: owned(&["old", "hist"]),
        soft_wrap: Some(SoftWrap {
            lines: Vec::new(),
            scrollback: vec![1],
        }),
        ..ScreenState::default()
    };
    assert_eq!(
        seam.unwrapped_split(),
        (owned(&["old"]), owned(&["history", "live"]))
    );
    assert_eq!(seam.rendered_rows(), vec!["old", "hist", "ory", "live"]);

    // No wrap info: rows verbatim, never heuristically joined.
    let old = ScreenState {
        lines: owned(&["the quick", "brown fox"]),
        ..ScreenState::default()
    };
    assert_eq!(old.unwrapped_rows(), owned(&["the quick", "brown fox"]));
}

#[test]
fn row_window_keeps_the_tail_and_never_clips_silently() {
    let rows: Vec<String> = (0..10).map(|i| format!("row{i}")).collect();
    assert_eq!(
        row_window(rows.clone(), 3),
        (owned(&["row7", "row8", "row9"]), true)
    );
    assert_eq!(row_window(rows.clone(), 10), (rows.clone(), false));
    assert_eq!(row_window(rows.clone(), ROW_WINDOW_ALL), (rows, false));

    let over = usize::try_from(ROW_WINDOW_MAX).unwrap_or(usize::MAX) + 5;
    let rows: Vec<String> = (0..over).map(|i| format!("row{i}")).collect();
    let (window, truncated) = row_window(rows, ROW_WINDOW_ALL);
    assert_eq!(u32::try_from(window.len()).ok(), Some(ROW_WINDOW_MAX));
    assert!(truncated, "the ceiling is never silent");
}

/// Every additive key is absent from a default read (byte-identical to
/// the original contract) and round-trips when populated.
#[test]
fn additive_keys_are_omitted_when_unset_and_round_trip_when_set() {
    let json = serde_json::to_string(&ScreenState::default()).expect("serialize");
    for key in [
        "cells",
        "soft_wrap",
        "truncated",
        "truncated_reason",
        "title",
        "rendered",
        "rendered_error",
    ] {
        assert!(
            !json.contains(&format!("\"{key}\"")),
            "{key} leaked: {json}"
        );
    }

    let full = ScreenState {
        pane: 4,
        cols: 8,
        rows: 1,
        cursor: Some(CursorState {
            x: 1,
            y: 0,
            visible: true,
        }),
        lines: owned(&["$ ls"]),
        scrollback: owned(&["old"]),
        cells: Some(vec![
            CellInfo {
                col: 0,
                row: 0,
                semantic: Some(SemanticContent::Prompt),
                style: CellStyle {
                    bold: true,
                    fg: CellColor::Rgb { r: 1, g: 2, b: 3 },
                    ..CellStyle::default()
                },
            },
            CellInfo {
                col: 2,
                row: 0,
                semantic: Some(SemanticContent::Input),
                style: CellStyle {
                    fg: CellColor::Palette { index: 7 },
                    ..CellStyle::default()
                },
            },
        ]),
        soft_wrap: Some(SoftWrap {
            lines: vec![0],
            scrollback: vec![0],
        }),
        truncated: true,
        truncated_reason: Some(TRUNCATED_ROW_WINDOW.to_owned()),
        title: Some("claude — phux".to_owned()),
        rendered: Some(RenderedScreen {
            format: RENDERED_FORMAT_HTML.to_owned(),
            data: "<span>hi</span>".to_owned(),
        }),
        rendered_error: Some("libghostty: out of memory".to_owned()),
        ..ScreenState::default()
    };
    let json = serde_json::to_string(&full).expect("serialize");
    assert!(json.contains("\"truncated\":true"), "got: {json}");
    assert!(
        json.contains("\"truncated_reason\":\"row_window\""),
        "got: {json}"
    );
    assert!(json.contains("\"format\":\"html\""), "got: {json}");
    let back: ScreenState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, full);
}

#[test]
fn rendered_frame_is_dense_row_major_and_round_trips() {
    let mut f = RenderedFrame::blank(3, 2);
    assert_eq!(f.cells.len(), 6);
    assert_eq!(f.cell(1, 2).expect("in range").grapheme, " ");
    assert!(f.cell(2, 0).is_none() && f.cell(0, 3).is_none());
    assert!(f.cell_mut(2, 0).is_none());
    f.cell_mut(1, 2).expect("in range").grapheme = "X".to_owned();
    assert_eq!(f.cells[5].grapheme, "X");

    let json = serde_json::to_string(&f).expect("serialize");
    let back: RenderedFrame = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(f, back);
}
