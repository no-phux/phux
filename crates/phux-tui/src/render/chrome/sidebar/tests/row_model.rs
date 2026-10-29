//! The row model: footer, bands, and invariants.

use super::*;

#[test]
fn row_model_reserves_footer_and_truncates_blocks() {
    // Band off: this test is about the footer and the session blocks.
    let c = SidebarCounts {
        active_session: Some(0),
        rule: SidebarRule::Trailing,
        agents_share: 0,
        ..counts(0, 3, 1)
    };
    let rows = row_model(c, 9);
    assert_eq!(rows.len(), 9);
    assert_eq!(rows[0], SidebarRow::SpacesHeader);
    assert_eq!(rows[1], SidebarRow::RosterHost(0));
    assert_eq!(rows[2], SidebarRow::RosterEntry(0));
    assert_eq!(rows[5], SidebarRow::WindowName(2));
    assert_eq!(rows[8], SidebarRow::NewWindow);
    // Keep hidden tabs reachable when the pair and its tabs cannot fit.
    let rows = row_model(c, 5);
    assert_eq!(rows[3], SidebarRow::RosterOverflow);
    assert_eq!(rows[4], SidebarRow::NewWindow);
    // Below the minimum height there is no footer, and hidden tabs
    // stay reachable through the overflow row.
    let rows = row_model(c, 3);
    assert_eq!(
        rows,
        vec![
            SidebarRow::SpacesHeader,
            SidebarRow::RosterOverflow,
            SidebarRow::Blank,
        ]
    );
}

/// Agents keep a fixed share of the body: Sessions land on the same row
/// whatever the population, and a calm panel keeps its header and a quiet
/// placeholder (ADR-0112).
#[test]
fn row_model_fixes_the_agents_band_above_sessions() {
    // 2 agents + 1 tab in 10 rows: header, filter chips, two agent rows,
    // then blanks.
    assert_eq!(
        row_model(counts(2, 1, 0), 10),
        vec![
            SidebarRow::NeedsYouHeader,
            SidebarRow::AgentFilter,
            SidebarRow::NeedsYou(0),
            SidebarRow::NeedsYou(1),
            SidebarRow::Blank,
            SidebarRow::Blank,
            SidebarRow::Blank,
            SidebarRow::Blank,
            SidebarRow::Blank,
            SidebarRow::NewWindow,
        ]
    );
    // The Sessions header is at the same row for every population.
    let sessions_at = |n| {
        row_model(counts(n, 1, 2), 12)
            .iter()
            .position(|r| *r == SidebarRow::SpacesHeader)
            .expect("sessions laid")
    };
    for n in 0..=9 {
        assert_eq!(
            sessions_at(n),
            sessions_at(0),
            "population {n} moved Sessions"
        );
    }
    // ...and the panel is exactly the configured share of the body
    // (12 rows - 1 footer = 11).
    assert_eq!(sessions_at(3), agents_budget(DEFAULT_AGENTS_SHARE, 11));
    // A calm panel keeps its header and quiet placeholder; Sessions still
    // sits on the fixed line.
    let rows = row_model(counts(0, 1, 2), 12);
    assert_eq!(rows[0], SidebarRow::NeedsYouHeader);
    assert_eq!(rows[1], SidebarRow::AgentFilter);
    assert_eq!(rows[2], SidebarRow::NeedsYouEmpty);
    let sessions = sessions_at(0);
    assert_eq!(rows[sessions], SidebarRow::SpacesHeader);
    assert_eq!(rows[sessions + 1], SidebarRow::RosterHost(0));
    assert_eq!(rows[sessions + 2], SidebarRow::RosterEntry(0));
    assert_eq!(rows[sessions + 3], SidebarRow::RosterHost(1));
    assert_eq!(rows[sessions + 4], SidebarRow::RosterEntry(1));
    // Sessions sharing a machine share one header.
    let one_machine = SidebarCounts {
        host_starts: 1,
        ..counts(0, 1, 2)
    };
    let rows = row_model(one_machine, 12);
    assert_eq!(rows[2], SidebarRow::NeedsYouEmpty);
    let sessions = rows
        .iter()
        .position(|r| *r == SidebarRow::SpacesHeader)
        .expect("sessions");
    assert_eq!(rows[sessions + 1], SidebarRow::RosterHost(0));
    assert_eq!(rows[sessions + 2], SidebarRow::RosterEntry(0));
    assert_eq!(rows[sessions + 3], SidebarRow::RosterEntry(1));
    // A strip with nothing to show paints no panels at all.
    let rows = row_model(counts(0, 1, 0), 12);
    assert!(!rows.contains(&SidebarRow::SpacesHeader));
    assert!(!rows.contains(&SidebarRow::NeedsYouHeader));
}

/// One machine opens no segment; two or more open one each.
#[test]
fn a_lone_machine_paints_no_host_line() {
    assert_eq!(
        segment_starts([(false, "mini"), (false, "mini")].into_iter()),
        0
    );
    assert_eq!(
        segment_starts([(false, "mini"), (false, "mini"), (false, "xps")].into_iter()),
        0b101
    );
}

/// Agents overflow at the panel's capacity, not a population-based cap.
#[test]
fn agents_overflow_declares_what_it_dropped() {
    // A 15-row body at half reserves 8 rows: a header, the filter chips,
    // and six list rows (one of which becomes `+N more` when over
    // capacity).
    let rows = row_model(counts(9, 1, 0), 16);
    let listed = rows
        .iter()
        .filter(|r| matches!(r, SidebarRow::NeedsYou(_)))
        .count();
    assert_eq!(listed, 5, "{rows:?}");
    assert!(rows.contains(&SidebarRow::NeedsYouOverflow), "{rows:?}");
    // Exactly at capacity there is nothing to declare.
    let rows = row_model(counts(6, 1, 0), 16);
    assert!(!rows.contains(&SidebarRow::NeedsYouOverflow), "{rows:?}");
}

/// The panel is a fixed share of the body, so however many agents need a
/// human Sessions always keeps their header at the same row.
#[test]
fn agents_band_is_fixed_and_never_starves_sessions() {
    for h in MIN_FOOTER_HEIGHT..24 {
        let rows = row_model(counts(20, 2, 3), h);
        let body = usize::from(h) - 1;
        assert_eq!(rows[0], SidebarRow::NeedsYouHeader, "h={h}");
        let band = agents_budget(DEFAULT_AGENTS_SHARE, body);
        assert_eq!(rows[band], SidebarRow::SpacesHeader, "h={h}");
        assert!(band < body, "Sessions keeps a row: h={h}");
    }
}

/// The invariants that must hold for every shape, since the allocator is
/// what both the painter and the hit-tester read.
#[test]
fn row_model_invariants_hold_across_shapes() {
    for needs_you in [0usize, 1, 5, 9] {
        for windows in [0usize, 1, 4] {
            for roster in [0usize, 1, 7] {
                for h in 0u16..26 {
                    let c = counts(needs_you, windows, roster);
                    let rows = row_model(c, h);
                    assert_eq!(rows.len(), usize::from(h), "{c:?} h={h}");

                    let footer = rows.contains(&SidebarRow::NewWindow);
                    assert_eq!(
                        footer,
                        h >= MIN_FOOTER_HEIGHT,
                        "footer presence tracks the height floor: {c:?} h={h}"
                    );

                    assert_model_items(c, &rows);
                }
            }
        }
    }
}
