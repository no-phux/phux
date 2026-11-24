use super::*;

fn titles<'a>(query: &str, rows: &[&'a str]) -> Vec<&'a str> {
    let q = Query::parse(query);
    rank(
        &q,
        rows.iter().map(|title| {
            (
                Fields {
                    title,
                    ..Fields::default()
                },
                0,
            )
        }),
    )
    .into_iter()
    .map(|(i, _)| rows[i])
    .collect()
}

fn lit(query: &str, title: &str) -> String {
    let hit = score(
        &Query::parse(query),
        &Fields {
            title,
            ..Fields::default()
        },
    )
    .expect("matches");
    title
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if hit.title_positions.contains(&i) {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

#[test]
fn highlights_land_on_words_not_the_first_letters_met() {
    assert_eq!(
        lit("tab", "toggle a sidebar, tabs"),
        "toggle a sidebar, TABs"
    );
    // Two equally good word-start placements: the earlier one wins.
    assert_eq!(
        lit("ns", "next session or new session"),
        "Next Session or new session"
    );
}

#[test]
fn initials_find_the_title_whose_words_they_start() {
    assert_eq!(lit("ns", "new session"), "New Session");
    assert_eq!(
        titles("ns", &["Rename session", "Next session", "New session"]),
        ["New session", "Next session", "Rename session"],
        "both start their words; the shorter wins the tie"
    );
}

#[test]
fn every_word_must_match_in_any_order() {
    let rows = [
        "Tabs on the left",
        "Tabs on the right",
        "Focus pane on the left",
        "New tab",
    ];
    assert_eq!(titles("left tab", &rows), ["Tabs on the left"]);
    assert_eq!(titles("tab left", &rows), ["Tabs on the left"]);
    assert!(titles("tab nowhere", &rows).is_empty());
}

#[test]
fn exact_then_prefix_then_word_starts_then_fragments() {
    let rows = [
        "Detach other clients",
        "Split right",
        "Detach",
        "Close tab (detaches)",
    ];
    assert_eq!(
        titles("detach", &rows),
        ["Detach", "Detach other clients", "Close tab (detaches)"]
    );
}

#[test]
fn shorter_titles_win_ties() {
    assert_eq!(
        titles("zoom", &["Zoom pane to fill the tab", "Zoom pane"]),
        ["Zoom pane", "Zoom pane to fill the tab"]
    );
}

#[test]
fn aliases_answer_without_lighting_the_title() {
    let q = Query::parse("quit");
    let hit = score(
        &q,
        &Fields {
            title: "Detach",
            aliases: &["exit", "quit", "leave"],
            detail: None,
        },
    )
    .expect("an alias answers");
    assert!(hit.title_positions.is_empty());
    let rows = [
        (
            Fields {
                title: "Detach",
                aliases: &["quit"],
                detail: None,
            },
            0,
        ),
        (
            Fields {
                title: "Quiet mode",
                aliases: &[],
                detail: None,
            },
            0,
        ),
    ];
    let order: Vec<usize> = rank(&Query::parse("quit"), rows)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        order,
        [0, 1],
        "a whole alias outranks a fragment ('quit' inside 'quiet')"
    );
}

#[test]
fn a_title_match_outranks_an_alias_match() {
    let rows = [
        (
            Fields {
                title: "Kill pane",
                aliases: &["close"],
                detail: None,
            },
            0,
        ),
        (
            Fields {
                title: "Close tab",
                aliases: &[],
                detail: None,
            },
            0,
        ),
    ];
    let order: Vec<usize> = rank(&Query::parse("close"), rows)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    assert_eq!(order, [1, 0]);
}

#[test]
fn detail_matches_rank_below_title_matches() {
    let rows = [
        (
            Fields {
                title: "api",
                aliases: &[],
                detail: Some("~/work/deploy"),
            },
            0,
        ),
        (
            Fields {
                title: "deploy",
                aliases: &[],
                detail: None,
            },
            0,
        ),
    ];
    let order: Vec<usize> = rank(&Query::parse("deploy"), rows)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    assert_eq!(order, [1, 0]);
}

#[test]
fn smart_case_upper_case_is_exact() {
    assert!(
        score(
            &Query::parse("Tab"),
            &Fields {
                title: "new tab",
                ..Fields::default()
            }
        )
        .is_none()
    );
    assert!(
        score(
            &Query::parse("tab"),
            &Fields {
                title: "New Tab",
                ..Fields::default()
            }
        )
        .is_some()
    );
}

#[test]
fn a_boost_reorders_close_matches_but_not_a_far_better_one() {
    let rows = |boost| {
        [
            (
                Fields {
                    title: "Split right",
                    ..Fields::default()
                },
                0,
            ),
            (
                Fields {
                    title: "Split below",
                    ..Fields::default()
                },
                boost,
            ),
        ]
    };
    let order = |boost| -> Vec<usize> {
        rank(&Query::parse("split"), rows(boost))
            .into_iter()
            .map(|(i, _)| i)
            .collect()
    };
    assert_eq!(order(0), [0, 1]);
    assert_eq!(order(40), [1, 0], "a recent row leads among equals");
    let exact = [
        (
            Fields {
                title: "Split",
                ..Fields::default()
            },
            0,
        ),
        (
            Fields {
                title: "Split below",
                ..Fields::default()
            },
            40,
        ),
    ];
    let order: Vec<usize> = rank(&Query::parse("split"), exact)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    assert_eq!(order, [0, 1], "recency never beats the exact title");
}

#[test]
fn an_empty_query_keeps_catalog_order() {
    assert_eq!(titles("", &["b", "a", "c"]), ["b", "a", "c"]);
    assert_eq!(titles("   ", &["b", "a"]), ["b", "a"]);
}

#[test]
fn groups_follow_their_best_row() {
    let section = |row: usize| ["Commands", "Sessions", "Commands", "Panes"][row];
    let grouped = group(&[1, 0, 3, 2], section);
    assert_eq!(
        grouped,
        vec![
            ("Sessions", vec![1]),
            ("Commands", vec![0, 2]),
            ("Panes", vec![3])
        ]
    );
}

#[test]
fn unicode_and_long_titles_do_not_panic() {
    let long = "é".repeat(400);
    assert!(
        score(
            &Query::parse("éé"),
            &Fields {
                title: &long,
                ..Fields::default()
            }
        )
        .is_some()
    );
    assert!(
        score(
            &Query::parse("日本"),
            &Fields {
                title: "日本語 tab",
                ..Fields::default()
            }
        )
        .is_some()
    );
    assert!(score(&Query::parse("x"), &Fields::default()).is_none());
}
