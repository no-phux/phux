//! Ports of phux-mobile's `AttentionGradeTests`, `AttentionMonotonicityTests`
//! and `TriageSortTests`, plus the shared fixture both platforms read.

use super::{
    AttentionGrade, DeclaredAgent, PaneStatus, REFERENCE_DATE_UNIX_MS, TriagePane, activity_minute,
    grade, triage_order,
};
use crate::projection::agent::{AgentAttention, AgentState, derived_attention};

const EVERY_STATUS: [PaneStatus; 6] = [
    PaneStatus::Asked,
    PaneStatus::Blocked,
    PaneStatus::Working,
    PaneStatus::Idle,
    PaneStatus::Done { exit: 0 },
    PaneStatus::Done { exit: 1 },
];
const EVERY_STATE: [AgentState; 5] = [
    AgentState::Idle,
    AgentState::Working,
    AgentState::Blocked,
    AgentState::Done,
    AgentState::Unknown,
];
const EVERY_LEVEL: [AgentAttention; 4] = [
    AgentAttention::None,
    AgentAttention::Low,
    AgentAttention::Normal,
    AgentAttention::High,
];

#[expect(clippy::unnecessary_wraps, reason = "builds the Option a pane carries")]
const fn agent(state: AgentState, attention: AgentAttention) -> Option<DeclaredAgent> {
    Some(DeclaredAgent { state, attention })
}

/// Every record a pane can carry, `None` (no record) first.
fn every_record() -> Vec<Option<DeclaredAgent>> {
    let mut records = vec![None];
    for state in EVERY_STATE {
        for level in EVERY_LEVEL {
            records.push(agent(state, level));
        }
    }
    records
}

#[test]
fn each_wire_level_maps_to_its_own_grade() {
    let grades: Vec<_> = EVERY_LEVEL.into_iter().map(AttentionGrade::from).collect();
    assert_eq!(
        grades,
        [
            AttentionGrade::None,
            AttentionGrade::Low,
            AttentionGrade::Normal,
            AttentionGrade::High
        ]
    );
}

#[test]
fn grades_are_ordered() {
    assert!(AttentionGrade::None < AttentionGrade::Low);
    assert!(AttentionGrade::Low < AttentionGrade::Normal);
    assert!(AttentionGrade::Normal < AttentionGrade::High);
}

#[test]
fn no_record_grades_normal() {
    for status in EVERY_STATUS {
        assert_eq!(grade(status, None), AttentionGrade::Normal, "{status:?}");
    }
}

#[test]
fn no_waiting_pane_grades_below_normal() {
    for status in EVERY_STATUS
        .into_iter()
        .filter(|status| status.is_waiting())
    {
        for record in every_record() {
            assert!(
                grade(status, record) >= AttentionGrade::Normal,
                "{status:?} {record:?}"
            );
        }
    }
}

#[test]
fn a_stale_record_on_a_waiting_pane_is_floored_at_normal() {
    for state in EVERY_STATE {
        for status in [PaneStatus::Asked, PaneStatus::Blocked] {
            assert!(
                grade(status, agent(state, derived_attention(state))) >= AttentionGrade::Normal,
                "{status:?} with a {state:?} record"
            );
        }
    }
}

#[test]
fn an_unreadable_lifecycle_word_cannot_mute_a_pane() {
    let unknown = agent(AgentState::Unknown, AgentAttention::Low);
    assert_eq!(grade(PaneStatus::Asked, unknown), AttentionGrade::Normal);
}

#[test]
fn an_explicit_down_claim_cannot_quiet_a_waiting_pane() {
    assert_eq!(
        grade(
            PaneStatus::Asked,
            agent(AgentState::Blocked, AgentAttention::None)
        ),
        AttentionGrade::Normal
    );
    assert_eq!(
        grade(
            PaneStatus::Blocked,
            agent(AgentState::Working, AgentAttention::Low)
        ),
        AttentionGrade::Normal
    );
    // A down-claim still counts where no one is waiting.
    assert_eq!(
        grade(
            PaneStatus::Idle,
            agent(AgentState::Idle, AgentAttention::None)
        ),
        AttentionGrade::None
    );
    assert_eq!(
        grade(
            PaneStatus::Working,
            agent(AgentState::Done, AgentAttention::Low)
        ),
        AttentionGrade::Low
    );
}

#[test]
fn derived_high_on_a_blocked_record_is_ordinary() {
    for status in [PaneStatus::Asked, PaneStatus::Blocked] {
        assert_eq!(
            grade(status, agent(AgentState::Blocked, AgentAttention::High)),
            AttentionGrade::Normal
        );
    }
}

#[test]
fn a_record_that_flags_itself_grades_high() {
    for (status, state) in [
        (PaneStatus::Asked, AgentState::Working),
        (PaneStatus::Asked, AgentState::Idle),
        (PaneStatus::Idle, AgentState::Done),
    ] {
        assert_eq!(
            grade(status, agent(state, AgentAttention::High)),
            AttentionGrade::High,
            "{status:?} {state:?}"
        );
    }
}

#[test]
fn flags_urgency_is_exactly_declared_above_derived() {
    for state in EVERY_STATE {
        let derived = AttentionGrade::from(derived_attention(state));
        for level in EVERY_LEVEL {
            let record = DeclaredAgent {
                state,
                attention: level,
            };
            assert_eq!(
                record.flags_urgency(),
                AttentionGrade::from(level) > derived,
                "{state:?} {level:?}"
            );
        }
    }
}

#[test]
fn status_predicates() {
    let waiting: Vec<_> = EVERY_STATUS.map(PaneStatus::is_waiting).into();
    assert_eq!(waiting, [true, true, false, false, false, false]);
    let failure: Vec<_> = EVERY_STATUS.map(PaneStatus::is_failure).into();
    assert_eq!(failure, [false, false, false, false, false, true]);
}

/// The Swift tests' `Date(timeIntervalSinceReferenceDate:)`, in Unix ms.
const fn at(reference_seconds: i64) -> i64 {
    REFERENCE_DATE_UNIX_MS + reference_seconds * 1000
}

#[test]
fn activity_minutes_bucket_and_truncate() {
    assert_eq!(activity_minute(None), i64::MIN);
    assert_eq!(
        activity_minute(Some(at(6001))),
        activity_minute(Some(at(6059)))
    );
    assert_eq!(activity_minute(Some(at(6120))), 102);
    // Swift's `Int64(_:)` truncates toward zero, before 2001 too.
    assert_eq!(activity_minute(Some(at(-59))), 0);
    assert_eq!(activity_minute(Some(at(-61))), -1);
    assert_eq!(activity_minute(Some(i64::MIN)), i64::MIN / 60_000);
}

#[test]
fn a_minute_start_reads_back_as_its_minute() {
    for minute in [0, 1, 102, 29_000_000] {
        assert_eq!(
            activity_minute(Some(REFERENCE_DATE_UNIX_MS + minute * 60_000)),
            minute
        );
    }
}

/// `TriageSortTests`'s fixture: `attention` set means a record, with `state`.
const fn pane(
    status: PaneStatus,
    activity: Option<i64>,
    record: Option<DeclaredAgent>,
) -> TriagePane {
    TriagePane {
        status,
        agent: record,
        last_activity_unix_ms: activity,
    }
}

const URGENT: Option<DeclaredAgent> = agent(AgentState::Working, AgentAttention::High);
const ROUTINE: Option<DeclaredAgent> = agent(AgentState::Blocked, AgentAttention::Normal);

#[test]
fn ranks_waiting_then_failure_then_rest() {
    let order = triage_order(&[
        pane(PaneStatus::Idle, None, None),
        pane(PaneStatus::Asked, None, None),
        pane(PaneStatus::Done { exit: 1 }, None, None),
        pane(PaneStatus::Idle, None, None),
    ]);
    assert_eq!(order, [1, 2, 0, 3]);
}

#[test]
fn ties_break_by_recency_across_minute_buckets() {
    let order = triage_order(&[
        pane(PaneStatus::Idle, Some(at(6000)), None),
        pane(PaneStatus::Idle, Some(at(6120)), None),
    ]);
    assert_eq!(order, [1, 0]);
}

#[test]
fn activity_within_one_minute_keeps_input_order() {
    let order = triage_order(&[
        pane(PaneStatus::Idle, Some(at(6001)), None),
        pane(PaneStatus::Idle, Some(at(6059)), None),
    ]);
    assert_eq!(order, [0, 1]);
}

#[test]
fn equal_rank_keeps_first_seen_order() {
    let idle = pane(PaneStatus::Idle, None, None);
    assert_eq!(triage_order(&[idle, idle, idle]), [0, 1, 2]);
    let routine = pane(PaneStatus::Asked, None, ROUTINE);
    assert_eq!(triage_order(&[routine, routine, routine]), [0, 1, 2]);
}

#[test]
fn the_order_is_a_permutation() {
    let panes = [
        pane(PaneStatus::Asked, None, None),
        pane(PaneStatus::Idle, None, None),
        pane(PaneStatus::Done { exit: 0 }, None, None),
        pane(PaneStatus::Working, None, None),
        pane(PaneStatus::Asked, None, URGENT),
        pane(
            PaneStatus::Asked,
            None,
            agent(AgentState::Blocked, AgentAttention::Low),
        ),
    ];
    let mut order = triage_order(&panes);
    order.sort_unstable();
    assert_eq!(order, [0, 1, 2, 3, 4, 5]);
    assert!(triage_order(&[]).is_empty());
}

#[test]
fn an_urgent_waiter_sorts_first_from_either_input_order() {
    let urgent = pane(PaneStatus::Asked, None, URGENT);
    let routine = pane(PaneStatus::Asked, None, ROUTINE);
    assert_eq!(triage_order(&[routine, urgent]), [1, 0]);
    assert_eq!(triage_order(&[urgent, routine]), [0, 1]);
}

#[test]
fn a_record_bearing_blocked_pane_does_not_outrank_a_fresh_hook_only_ask() {
    let order = triage_order(&[
        pane(
            PaneStatus::Blocked,
            Some(at(6000)),
            agent(AgentState::Blocked, AgentAttention::High),
        ),
        pane(PaneStatus::Asked, Some(at(6120)), None),
    ]);
    assert_eq!(order, [1, 0]);
}

#[test]
fn an_ungraded_waiter_ties_with_an_explicit_normal() {
    let order = triage_order(&[
        pane(PaneStatus::Asked, None, None),
        pane(PaneStatus::Asked, None, ROUTINE),
    ]);
    assert_eq!(order, [0, 1]);
}

#[test]
fn the_sub_rank_never_leaks_across_bands() {
    let order = triage_order(&[
        pane(PaneStatus::Idle, None, None),
        pane(PaneStatus::Done { exit: 1 }, None, None),
        pane(
            PaneStatus::Asked,
            None,
            agent(AgentState::Idle, AgentAttention::None),
        ),
    ]);
    assert_eq!(order, [2, 1, 0]);
}

#[test]
fn a_grade_on_a_non_waiting_pane_changes_nothing() {
    let order = triage_order(&[
        pane(PaneStatus::Done { exit: 1 }, None, None),
        pane(PaneStatus::Idle, None, URGENT),
    ]);
    assert_eq!(order, [0, 1]);
}

#[test]
fn the_grade_outranks_recency_within_the_waiting_band() {
    let order = triage_order(&[
        pane(PaneStatus::Asked, Some(at(6120)), ROUTINE),
        pane(PaneStatus::Asked, Some(at(6000)), URGENT),
    ]);
    assert_eq!(order, [1, 0]);
}

// The shared fixture, `fixtures/attention.json`.

const FIXTURE: &str = include_str!("../../../fixtures/attention.json");

fn fixture() -> serde_json::Value {
    serde_json::from_str(FIXTURE).expect("the fixture is JSON")
}

fn status_of(case: &serde_json::Value) -> PaneStatus {
    match case["status"].as_str() {
        Some("working") => PaneStatus::Working,
        Some("asked") => PaneStatus::Asked,
        Some("blocked") => PaneStatus::Blocked,
        Some("idle") => PaneStatus::Idle,
        Some("done") => PaneStatus::Done {
            exit: i32::try_from(case["exit"].as_i64().expect("done has an exit"))
                .expect("exit fits i32"),
        },
        other => panic!("unknown status {other:?}"),
    }
}

fn agent_of(case: &serde_json::Value) -> Option<DeclaredAgent> {
    let record = case.get("agent").filter(|record| !record.is_null())?;
    let state = match record["state"].as_str() {
        Some("idle") => AgentState::Idle,
        Some("working") => AgentState::Working,
        Some("blocked") => AgentState::Blocked,
        Some("done") => AgentState::Done,
        Some("unknown") => AgentState::Unknown,
        other => panic!("unknown state {other:?}"),
    };
    let attention = match record["attention"].as_str() {
        Some("none") => AgentAttention::None,
        Some("low") => AgentAttention::Low,
        Some("normal") => AgentAttention::Normal,
        Some("high") => AgentAttention::High,
        other => panic!("unknown attention {other:?}"),
    };
    Some(DeclaredAgent { state, attention })
}

fn grade_word(grade: AttentionGrade) -> &'static str {
    match grade {
        AttentionGrade::None => "none",
        AttentionGrade::Low => "low",
        AttentionGrade::Normal => "normal",
        AttentionGrade::High => "high",
    }
}

#[test]
fn the_fixture_grade_matrix_is_exhaustive_and_holds() {
    let fixture = fixture();
    let cases = fixture["grade"].as_array().expect("grade cases");
    assert_eq!(cases.len(), EVERY_STATUS.len() * every_record().len());
    let mut seen = std::collections::HashSet::new();
    for case in cases {
        let (status, record) = (status_of(case), agent_of(case));
        assert!(seen.insert(format!("{status:?}{record:?}")), "{case}");
        assert_eq!(
            Some(grade_word(grade(status, record))),
            case["grade"].as_str(),
            "{case}"
        );
    }
}

#[test]
fn the_fixture_triage_cases_hold() {
    let fixture = fixture();
    let cases = fixture["triage"].as_array().expect("triage cases");
    assert!(!cases.is_empty());
    for case in cases {
        let rows = case["panes"].as_array().expect("panes");
        let panes: Vec<_> = rows
            .iter()
            .map(|row| TriagePane {
                status: status_of(row),
                agent: agent_of(row),
                last_activity_unix_ms: row["last_activity_unix_ms"].as_i64(),
            })
            .collect();
        let ids: Vec<_> = triage_order(&panes)
            .into_iter()
            .map(|index| rows[index]["id"].clone())
            .collect();
        assert_eq!(
            serde_json::Value::from(ids),
            case["order"],
            "{}",
            case["name"]
        );
    }
}
