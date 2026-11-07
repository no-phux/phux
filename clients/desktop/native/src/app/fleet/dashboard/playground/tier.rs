//! Creatures grow with work done: the score is tasks finished plus working
//! minutes over ten, read from the host's agent-stats tally. Without a tally
//! (the plugin is opt-in) every agent is a plain reef fish, as before.

use crate::app::fleet::Agent;
use crate::app::pane_identity::Harness;
use crate::connection::stats::AgentStats;

/// One tier: its name, how much bigger and slower than a reef fish, and the
/// score it starts at.
pub(crate) struct Grade {
    pub name: &'static str,
    pub size: f32,
    pub speed: f32,
    pub min: f32,
}

pub(crate) const TIERS: [Grade; 5] = [
    Grade {
        name: "Fry",
        size: 0.62,
        speed: 1.15,
        min: 0.,
    },
    Grade {
        name: "Reef fish",
        size: 1.,
        speed: 1.,
        min: 1.,
    },
    Grade {
        name: "Elder",
        size: 1.28,
        speed: 0.92,
        min: 5.,
    },
    Grade {
        name: "Exotic",
        size: 1.5,
        speed: 0.85,
        min: 12.,
    },
    Grade {
        name: "Legendary",
        size: 1.8,
        speed: 0.75,
        min: 30.,
    },
];

/// The tier a plain reef fish is: an agent without a tally.
pub(crate) const REEF_FISH: usize = 1;

pub(crate) fn score(stats: &AgentStats, working: bool, now_ms: u64) -> f32 {
    stats.turns as f32 + stats.working_now(working, now_ms) as f32 / 600.
}

/// The highest tier whose score the agent has reached.
pub(crate) fn tier_of(score: f32) -> usize {
    TIERS.iter().rposition(|t| score >= t.min).unwrap_or(0)
}

/// The agent's tier at `now_ms`.
pub(crate) fn tier(agent: &Agent, now_ms: u64) -> usize {
    agent.stats.map_or(REEF_FISH, |stats| {
        tier_of(score(&stats, agent.info.working(), now_ms))
    })
}

/// A level-up: the tier rose while watching. A first sight is no level-up,
/// and a drop (the host restarted, its tallies with it) just regrades.
pub(crate) fn rose(was: Option<usize>, now: usize) -> bool {
    was.is_some_and(|was| now > was)
}

/// A harness's exotic form.
pub(crate) fn exotic(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "lionfish",
        Harness::Codex => "manta ray",
        Harness::OpenCode => "seahorse",
        Harness::Gemini => "cuttlefish",
        _ => "octopus",
    }
}

pub(crate) fn harness(agent: &Agent) -> Harness {
    Harness::from_slug(agent.info.kind.as_deref().unwrap_or_default())
}

fn duration(s: u64) -> String {
    if s < 3600 {
        format!("{} min", (s as f32 / 60.).round())
    } else {
        format!("{} h {} min", s / 3600, ((s % 3600) as f32 / 60.).round())
    }
}

/// The detail rows, "Form: Exotic lionfish" and "Work: 14 tasks finished ·
/// 1 h 35 min working"; none without a tally.
pub(crate) fn detail(agent: &Agent, now_ms: u64) -> Option<[String; 2]> {
    let stats = agent.stats?;
    let at = tier(agent, now_ms);
    let form = if at >= 3 {
        format!("{} {}", TIERS[at].name, exotic(harness(agent)))
    } else {
        TIERS[at].name.to_owned()
    };
    let turns = stats.turns;
    let working = stats.working_now(agent.info.working(), now_ms);
    Some([
        format!("Form: {form}"),
        format!(
            "Work: {turns} task{} finished · {} working",
            if turns == 1 { "" } else { "s" },
            duration(working)
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::fleet::Destination;
    use crate::connection::{AgentAttention, AgentInfo, AgentState};

    fn agent(kind: &str, stats: Option<(u32, u64)>) -> Agent {
        Agent {
            destination: Destination {
                host: None,
                authority: None,
                key: None,
                name: "s".into(),
            },
            terminal: phux_protocol::ResourceId::local(1),
            boot: None,
            info: AgentInfo {
                name: "a".into(),
                kind: Some(kind.into()),
                state: AgentState::Idle,
                attention: AgentAttention::None,
                changed_at_ms: 0,
            },
            title: String::new(),
            project: None,
            stats: stats.map(|(turns, working_s)| AgentStats {
                turns,
                working_s,
                at_ms: 0,
            }),
        }
    }

    #[test]
    fn tiers_start_at_their_scores() {
        for (score, want) in [
            (0., 0),
            (0.99, 0),
            (1., 1),
            (4.99, 1),
            (5., 2),
            (11.99, 2),
            (12., 3),
            (29.99, 3),
            (30., 4),
            (500., 4),
        ] {
            assert_eq!(tier_of(score), want, "{score}");
        }
        // Ten working minutes count as one task.
        assert_eq!(tier(&agent("claude", Some((4, 600))), 0), 2);
        assert_eq!(tier(&agent("claude", Some((4, 599))), 0), 1);
        assert_eq!(tier(&agent("claude", Some((0, 0))), 0), 0, "fry");
    }

    #[test]
    fn no_tally_is_a_reef_fish_and_no_detail() {
        let plain = agent("claude", None);
        assert_eq!(tier(&plain, 0), REEF_FISH);
        assert_eq!(detail(&plain, 0), None);
    }

    #[test]
    fn a_level_up_is_only_a_rise_while_watching() {
        assert!(rose(Some(1), 2));
        assert!(!rose(None, 4), "first sight");
        assert!(!rose(Some(3), 3));
        assert!(!rose(Some(4), 1), "a restart regrades quietly");
    }

    #[test]
    fn detail_names_the_form_and_the_work() {
        let lionfish = agent("claude", Some((14, 95 * 60)));
        assert_eq!(
            detail(&lionfish, 0).unwrap(),
            [
                "Form: Exotic lionfish".to_owned(),
                "Work: 14 tasks finished · 1 h 35 min working".to_owned()
            ]
        );
        let elder = detail(&agent("pi", Some((1, 40 * 60))), 0).unwrap();
        assert_eq!(elder[0], "Form: Elder");
        assert_eq!(elder[1], "Work: 1 task finished · 40 min working");
        assert_eq!(
            detail(&agent("aider", Some((40, 0))), 0).unwrap()[0],
            "Form: Legendary octopus"
        );
        assert_eq!(exotic(Harness::Codex), "manta ray");
        assert_eq!(exotic(Harness::OpenCode), "seahorse");
        assert_eq!(exotic(Harness::Gemini), "cuttlefish");
    }
}
