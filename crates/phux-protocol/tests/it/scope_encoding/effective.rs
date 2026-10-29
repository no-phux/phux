//! The conjunctive effective set.

use super::set;
use phux_protocol::scope::{EffectiveScopeSet, Selector, TerminalScopeSet, Verb, Verbs};

#[test]
fn effective_set_keeps_conjunctive_clauses_and_never_flattens_group_to_terminal() {
    let requested = set(&["bind,observe@group:7"]);
    let ceiling = set(&["observe,input@terminal:9"]);
    let effective = EffectiveScopeSet::intersect(&requested, &ceiling).unwrap();
    assert_eq!(effective.clauses().len(), 1);
    let clause = &effective.clauses()[0];
    assert_eq!(clause.requested, Selector::Group(7), "the Group survives");
    assert_eq!(clause.ceiling, Selector::TerminalLocal(9));
    assert_eq!(clause.verbs, Verbs::of(&[Verb::Observe]));

    // Terminal 9 while it is in Group 7: both selectors contain it.
    let in_group =
        |selector: &Selector| matches!(selector, Selector::Group(7) | Selector::TerminalLocal(9));
    assert!(effective.admits(Verb::Observe, in_group));
    assert!(
        !effective.admits(Verb::Input, in_group),
        "INPUT is not in both"
    );
    assert!(
        !effective.admits(Verb::Bind, in_group),
        "BIND is not in both"
    );

    // The moment Terminal 9 leaves Group 7 the clause stops matching: the
    // Group ceiling was never flattened into a bare Terminal grant.
    let moved_out = |selector: &Selector| matches!(selector, Selector::TerminalLocal(9));
    assert!(!effective.admits(Verb::Observe, moved_out));

    // Selectors that can never share a subject produce no clause.
    let disjoint =
        EffectiveScopeSet::intersect(&set(&["*@terminal:1"]), &set(&["*@terminal:2"])).unwrap();
    assert!(disjoint.is_empty());
    let other_host =
        EffectiveScopeSet::intersect(&set(&["*@host:a"]), &set(&["*@terminal:b/1"])).unwrap();
    assert!(other_host.is_empty());

    // A set intersected with itself keeps its own grants as clauses.
    let own = set(&["*@global", "observe@terminal:5"]);
    let effective = EffectiveScopeSet::intersect(&own, &own).unwrap();
    assert!(effective.clauses().iter().any(|clause| {
        clause.requested == Selector::Global
            && clause.ceiling == Selector::Global
            && clause.verbs == phux_protocol::scope::all_verbs()
    }));
}

#[test]
fn unattenuated_set_is_one_clause_per_grant_and_grants_the_same() {
    let ceiling = set(&[
        "bind,observe@group:7",
        "observe,input@terminal:9",
        "*@host:a",
    ]);
    let diagonal = EffectiveScopeSet::unattenuated(&ceiling).unwrap();
    assert_eq!(diagonal.clauses().len(), 3);
    assert!(
        diagonal
            .clauses()
            .iter()
            .all(|clause| clause.requested == clause.ceiling)
    );
    // Canonical: it decodes as its own encoding.
    assert_eq!(
        EffectiveScopeSet::decode(&diagonal.encode()).unwrap(),
        diagonal
    );
    let full = EffectiveScopeSet::intersect(&ceiling, &ceiling).unwrap();
    let subjects: [&dyn Fn(&Selector) -> bool; 3] = [
        &|s| matches!(s, Selector::Group(7) | Selector::TerminalLocal(9)),
        &|s| matches!(s, Selector::TerminalLocal(9)),
        &|s| matches!(s, Selector::HostSatellite(host) if host.as_str() == "a"),
    ];
    for contains in subjects {
        for verb in Verb::ALL {
            assert_eq!(
                diagonal.admits(verb, contains),
                full.admits(verb, contains),
                "{verb:?}"
            );
        }
    }
    // A full ceiling fits, where its self-intersection may not.
    let mut wide: Vec<String> = (0..63).map(|n| format!("observe@host:h{n}")).collect();
    wide.push("*@global".to_owned());
    let wide = TerminalScopeSet::parse_all(&wide).unwrap();
    assert_eq!(
        EffectiveScopeSet::unattenuated(&wide)
            .unwrap()
            .clauses()
            .len(),
        64
    );
}
