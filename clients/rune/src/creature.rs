//! Creatures grow with work done: each host's `phux.agent-stats/v1` Global
//! key (the agent-stats plugin, `plugins/agent-stats/README.md#contract`)
//! says how many tasks each pane's agent has finished and how long it has
//! worked, and that picks its tier, from fry to legendary, in the reef and
//! the galaxy alike. The key is opt-in: absent, unreadable, another version,
//! or an agent it doesn't list is tier 1, the plain reef fish.

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;
use serde_json::Value;

/// The Global key every machine's server publishes its tallies under.
pub const KEY: &str = "phux.agent-stats/v1";

/// One agent's work, as of when its document arrived.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Work {
    /// Tasks finished.
    pub turns: u64,
    /// Seconds spent working, the open stretch included.
    pub working_s: f64,
}

pub struct Tier {
    pub name: &'static str,
    /// Times the base length (the reef) or radius (the galaxy).
    pub size: f32,
    /// Times the top speed.
    pub speed: f32,
    /// The least score that reaches it.
    pub min: f64,
}

pub const TIERS: [Tier; 5] = [
    Tier {
        name: "Fry",
        size: 0.62,
        speed: 1.15,
        min: 0.0,
    },
    Tier {
        name: "Reef fish",
        size: 1.0,
        speed: 1.0,
        min: 1.0,
    },
    Tier {
        name: "Elder",
        size: 1.28,
        speed: 0.92,
        min: 5.0,
    },
    Tier {
        name: "Exotic",
        size: 1.5,
        speed: 0.85,
        min: 12.0,
    },
    Tier {
        name: "Legendary",
        size: 1.8,
        speed: 0.75,
        min: 30.0,
    },
];

/// Tasks finished plus working minutes over ten.
pub fn score(w: &Work) -> f64 {
    w.turns as f64 + w.working_s / 600.0
}

/// The tier for `work`; none known is the plain reef fish.
pub fn tier(work: Option<&Work>) -> usize {
    work.map_or(1, |w| {
        TIERS.iter().rposition(|t| score(w) >= t.min).unwrap_or(0)
    })
}

/// Whether going from `before` to `after` is a level-up worth a burst: a
/// rise between two known tallies. The first tally to arrive is not one,
/// and a drop (the server restarted) only regrades.
pub fn levels_up(before: Option<&Work>, after: Option<&Work>) -> bool {
    before.is_some() && after.is_some() && tier(after) > tier(before)
}

/// The exotic creature a harness becomes at tier 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Lionfish,
    Manta,
    Seahorse,
    Cuttlefish,
    Octopus,
}

impl Form {
    pub fn name(self) -> &'static str {
        match self {
            Form::Lionfish => "lionfish",
            Form::Manta => "manta ray",
            Form::Seahorse => "seahorse",
            Form::Cuttlefish => "cuttlefish",
            Form::Octopus => "octopus",
        }
    }
}

/// What the card says it is: "Elder", "Exotic lionfish".
pub fn form_name(tier: usize, form: Form) -> String {
    let name = TIERS[tier.min(4)].name;
    if tier >= 3 {
        format!("{name} {}", form.name())
    } else {
        name.to_owned()
    }
}

/// "12 tasks finished · 1 h 20 min working".
pub fn work_line(w: &Work) -> String {
    let s = w.working_s.max(0.0);
    let dur = if s < 3600.0 {
        format!("{} min", (s / 60.0).round())
    } else {
        format!(
            "{} h {} min",
            (s / 3600.0).floor(),
            ((s % 3600.0) / 60.0).round()
        )
    };
    let plural = if w.turns == 1 { "" } else { "s" };
    format!("{} task{plural} finished · {dur} working", w.turns)
}

/// One machine's document, by terminal id, and when it arrived.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    agents: HashMap<u32, Work>,
    /// App clock seconds at arrival.
    at: f64,
}

impl Stats {
    /// The key's value as it arrived at `at`; anything but a version 1
    /// document reads as no stats.
    pub fn parse(value: Option<&[u8]>, at: f64) -> Stats {
        let doc = value.and_then(|v| serde_json::from_slice::<Value>(v).ok());
        let agents = doc
            .as_ref()
            .filter(|d| d.get("v").and_then(Value::as_u64) == Some(1))
            .and_then(|d| d.get("agents")?.as_object())
            .map(|agents| {
                agents
                    .iter()
                    .filter_map(|(id, a)| {
                        let n = |k: &str| a.get(k).and_then(Value::as_f64).filter(|v| *v >= 0.0);
                        let work = Work {
                            turns: n("turns")? as u64,
                            working_s: n("working_s")?,
                        };
                        Some((id.parse().ok()?, work))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Stats { agents, at }
    }

    /// The work of the agent in `terminal` at `now`: one still working has
    /// worked on since the document was written.
    pub fn work(&self, terminal: &ResourceId, working: bool, now: f64) -> Option<Work> {
        let mut w = *self.agents.get(&terminal.local_id()?)?;
        if working {
            w.working_s += (now - self.at).max(0.0);
        }
        Some(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(turns: u64, minutes: f64) -> Work {
        Work {
            turns,
            working_s: minutes * 60.0,
        }
    }

    #[test]
    fn score_crosses_each_tier_at_its_boundary() {
        for (turns, want) in [
            (0, 0),
            (1, 1),
            (4, 1),
            (5, 2),
            (11, 2),
            (12, 3),
            (29, 3),
            (30, 4),
            (500, 4),
        ] {
            assert_eq!(tier(Some(&w(turns, 0.0))), want, "{turns} turns");
        }
        // Ten working minutes are one task.
        assert_eq!(tier(Some(&w(0, 9.9))), 0);
        assert_eq!(tier(Some(&w(0, 10.0))), 1);
        assert_eq!(tier(Some(&w(2, 30.0))), 2);
        assert_eq!(tier(Some(&w(25, 50.0))), 4);
        assert_eq!(tier(None), 1, "unknown is the plain reef fish");
    }

    #[test]
    fn an_absent_or_foreign_document_is_no_stats() {
        let id = ResourceId::local(12);
        for bad in [
            None,
            Some(&b"not json"[..]),
            Some(br#"{"v":2,"agents":{"12":{"turns":40,"working_s":0}}}"#),
            Some(br#"{"agents":{"12":{"turns":40,"working_s":0}}}"#),
            Some(br#"{"v":1,"agents":[]}"#),
            Some(br#"{"v":1,"agents":{"12":{"turns":"many","working_s":0}}}"#),
        ] {
            let stats = Stats::parse(bad, 0.0);
            assert_eq!(tier(stats.work(&id, false, 0.0).as_ref()), 1, "{bad:?}");
        }
    }

    #[test]
    fn agents_match_by_terminal_id_and_working_ones_keep_counting() {
        let doc = br#"{"v":1,"extra":true,"agents":{"12":{"kind":"claude","name":"fox","turns":4,"working_s":731,"new":1},"7":{"turns":40,"working_s":0}}}"#;
        let stats = Stats::parse(Some(doc), 100.0);
        assert_eq!(
            stats.work(&ResourceId::local(12), false, 160.0),
            Some(w(4, 731.0 / 60.0))
        );
        assert_eq!(
            tier(stats.work(&ResourceId::local(7), false, 0.0).as_ref()),
            4
        );
        assert_eq!(
            stats.work(&ResourceId::local(3), false, 0.0),
            None,
            "not listed"
        );
        let live = stats.work(&ResourceId::local(12), true, 160.0).unwrap();
        assert_eq!(live.working_s, 791.0, "a minute on since it arrived");
        let there = ResourceId::satellite("box", 12);
        assert_eq!(
            stats.work(&there, false, 0.0),
            None,
            "only this server's own ids"
        );
    }

    #[test]
    fn only_a_rise_between_known_tallies_levels_up() {
        let (fry, elder) = (w(0, 0.0), w(6, 0.0));
        assert!(levels_up(Some(&fry), Some(&elder)));
        assert!(!levels_up(None, Some(&elder)), "the first tally to arrive");
        assert!(!levels_up(Some(&elder), Some(&fry)), "a server restart");
        assert!(!levels_up(Some(&elder), Some(&w(7, 0.0))), "same tier");
        assert!(!levels_up(Some(&elder), None));
    }

    #[test]
    fn the_card_names_the_form_and_the_work() {
        assert_eq!(form_name(3, Form::Lionfish), "Exotic lionfish");
        assert_eq!(form_name(4, Form::Manta), "Legendary manta ray");
        assert_eq!(form_name(2, Form::Octopus), "Elder");
        assert_eq!(work_line(&w(1, 4.0)), "1 task finished · 4 min working");
        assert_eq!(
            work_line(&w(12, 80.0)),
            "12 tasks finished · 1 h 20 min working"
        );
    }
}
