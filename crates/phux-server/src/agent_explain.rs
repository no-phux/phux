//! Offline evaluation of the compiled agent-detection manifests (ADR-0046).
//!
//! The detector itself is a live, PTY-bound thing: it identifies an agent from
//! the foreground process group and re-reads the grid on a timer. None of that
//! is available to someone *writing* a manifest, and ADR-0046's Tradeoffs
//! section records what that costs — the first Claude manifest was authored
//! against an imagined TUI, every screen rule matched nothing in the shipped
//! CLI, the fail-safe (`idle`) swallowed the failure silently, and the unit
//! tests passed because they fed the matcher the same invented screens the
//! rules had been derived from. Three rules were deleted.
//!
//! This module is the answer: evaluate the real compiled rules against a
//! **captured** screen, with no server, no PTY, and no process to identify,
//! and report the text every region resolved to. A rule scoped to a region
//! that comes back empty is the failure mode, and an empty region is only
//! visible if something prints it.
//!
//! # Why this is a facade and not `pub` on the detector
//!
//! `agent_detect` is `pub(crate)` and stays that way. Its types are the
//! detector's working state — `Rc<RuleSet>`, `Predicate`, `Screen<'a>`,
//! `DetectedState` — and publishing them would freeze the internals of a
//! module whose whole design premise is that agent TUIs churn. What a caller
//! outside this crate needs is narrower and stable: *give me a screen and a
//! kind, tell me what the detector would conclude and why*. That is the
//! surface below. The types here own their data (owned `String`s, no
//! lifetimes, no `Rc`), derive `Serialize` so `--json` is a projection rather
//! than a second hand-written shape, and speak the wire's state words rather
//! than the detector's internal enum.
//!
//! # What it deliberately does not do
//!
//! Identify. Identification reads the PTY's foreground process group
//! (`agent_detect::identify`, private to this crate), which a file cannot
//! supply, so the caller names the kind. [`kinds`] enumerates what is
//! available.

use serde::Serialize;

use crate::agent_detect::regions::{Region, Screen};
use crate::agent_detect::rules::{self, PredicateTrace, RuleTrace};

/// A screen to evaluate: what the detector would have read from the pane.
///
/// `title` is separate because a capture does not carry it — the
/// `phux snapshot --json` contract (`phux_core::screen::ScreenState`) is the
/// grid alone. Leaving it empty is legitimate and common; it simply means
/// every `title`-scoped rule sees an empty region, which the explanation says
/// out loud rather than quietly folding into "no match".
#[derive(Debug, Clone, Default)]
pub struct Capture {
    /// The pane's OSC 0/2 title at capture time, or empty if unknown.
    pub title: String,
    /// Live viewport rows, top to bottom, right-trimmed.
    pub lines: Vec<String>,
}

/// One predicate node and what it saw.
#[derive(Debug, Clone, Serialize)]
pub struct PredicateEvidence {
    /// The manifest keyword: `contains`, `regex`, `line-regex`, `all`,
    /// `any`, `not`.
    pub op: String,
    /// The pattern, as compiled. Absent on a combinator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Whether this node matched.
    pub matched: bool,
    /// Children of a combinator. Every child is evaluated, including the
    /// ones a short-circuiting matcher would have skipped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PredicateEvidence>,
}

/// One rule's outcome on the captured screen.
#[derive(Debug, Clone, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "reports the manifest's independent per-rule flags verbatim"
)]
pub struct EvaluatedRule {
    /// The rule's manifest id.
    pub id: String,
    /// Its priority. Higher wins within the same region class.
    pub priority: i32,
    /// The region it read, in the spelling a manifest uses.
    pub region: String,
    /// The state it asserts, or absent for a pure-flag rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Whether its predicate matched.
    pub matched: bool,
    /// `visible-blocker`.
    pub visible_blocker: bool,
    /// `visible-idle`.
    pub visible_idle: bool,
    /// `visible-working`.
    pub visible_working: bool,
    /// `skip-state-update`.
    pub skip_state_update: bool,
    /// The predicate tree with per-node results.
    pub evidence: PredicateEvidence,
}

/// The text one region resolved to on the captured screen.
#[derive(Debug, Clone, Serialize)]
pub struct RegionPreview {
    /// The region's manifest spelling.
    pub region: String,
    /// `true` when the region resolved to nothing at all. A rule scoped to
    /// an empty region cannot match, whatever it says.
    pub empty: bool,
    /// The resolved lines, verbatim.
    pub lines: Vec<String>,
}

/// What the detector would conclude about a captured screen, and why.
#[derive(Debug, Clone, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "reports the union of the matching rules' independent manifest flags; \
              collapsing them would hide combinations a real screen produces"
)]
pub struct Explanation {
    /// The kind slug whose manifest was evaluated.
    pub kind: String,
    /// The human-facing name that manifest writes into `phux.agent/v1`.
    pub name: String,
    /// The state a rule asserted, absent when none did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// What the detector would actually publish for this screen: the
    /// asserted state, else the `idle` fail-safe, else `frozen` when a
    /// `skip-state-update` rule matched and the previous state is held.
    ///
    /// This is the field to read. `state` is the rule's assertion;
    /// `detector_state` is the outcome, and the two differ in exactly the
    /// cases worth knowing about.
    pub detector_state: String,
    /// The winning rule's id, absent when nothing state-bearing matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<String>,
    /// Why `detector_state` is not a rule's assertion, absent when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// A `skip-state-update` rule matched: this screen carries no
    /// information about agent state and the detector freezes.
    pub freeze: bool,
    /// A matching rule positively asserts a blocker is on screen.
    pub visible_blocker: bool,
    /// A matching rule positively asserts idleness.
    pub visible_idle: bool,
    /// A matching rule positively asserts work.
    pub visible_working: bool,
    /// Every region, resolved against this screen. Includes regions no rule
    /// names: picking the right one is half of authoring a rule.
    pub regions: Vec<RegionPreview>,
    /// Every rule, in declaration order, matched or not.
    pub evaluated_rules: Vec<EvaluatedRule>,
}

/// Every agent kind with a loaded manifest, sorted.
///
/// Reflects the same load path the server uses, overrides included
/// (`$PHUX_AGENT_RULES_DIR`, else `$XDG_CONFIG_HOME/phux/agent-rules`), so an
/// operator debugging their own manifest is debugging the one that would run.
/// Empty when `PHUX_AGENT_DETECT=0` disabled the whole feature.
#[must_use]
pub fn kinds() -> Vec<String> {
    rules::global().kinds()
}

/// Resolve `name` to a kind slug: an exact kind, else a binary alias.
///
/// Aliases are accepted because the thing an operator has in hand is usually
/// the command they typed (`claude-code`, `opencode2`), and making them
/// reverse-map it to a slug is friction with no upside.
#[must_use]
pub fn resolve_kind(name: &str) -> Option<String> {
    let set = rules::global();
    if set.manifest(name).is_some() {
        return Some(name.to_owned());
    }
    set.kind_for_binary(name).map(str::to_owned)
}

/// Evaluate `kind`'s manifest against `capture`.
///
/// `None` when no manifest is loaded for `kind`; [`kinds`] is the roster to
/// report alongside that miss.
#[must_use]
pub fn explain(kind: &str, capture: &Capture) -> Option<Explanation> {
    let set = rules::global();
    let manifest = set.manifest(kind)?;
    let screen = Screen {
        title: &capture.title,
        lines: &capture.lines,
    };
    let explained = manifest.explain(&screen);
    let evaluation = &explained.evaluation;

    let state = evaluation.state.map(|s| s.as_str().to_owned());
    // Order matters and mirrors `AgentDetector::tick`: the freeze branch
    // returns BEFORE the fail-safe is applied, so a screen that both freezes
    // and matches nothing is frozen, not idle.
    let (detector_state, fallback_reason) = if evaluation.freeze {
        (
            "frozen".to_owned(),
            Some(
                "a skip-state-update rule matched: this screen carries no agent-state \
                 information, so the detector holds its previous state"
                    .to_owned(),
            ),
        )
    } else if let Some(state) = state.clone() {
        (state, None)
    } else {
        (
            "idle".to_owned(),
            Some(
                "no state-bearing rule matched; the detector fails safe to idle, never blocked"
                    .to_owned(),
            ),
        )
    };

    Some(Explanation {
        kind: kind.to_owned(),
        name: manifest.name.clone(),
        state,
        detector_state,
        matched_rule: evaluation.matched.clone(),
        fallback_reason,
        freeze: evaluation.freeze,
        visible_blocker: evaluation.visible_blocker,
        visible_idle: evaluation.visible_idle,
        visible_working: evaluation.visible_working,
        regions: explained
            .regions
            .into_iter()
            .map(|(region, lines)| RegionPreview {
                region: region.as_str(),
                // A single empty string is what `title` yields for a pane
                // with no title: the region exists but holds nothing a
                // predicate can see, which is the same failure as no rows.
                empty: lines.iter().all(|line| line.trim().is_empty()),
                lines,
            })
            .collect(),
        evaluated_rules: explained.rules.into_iter().map(evaluated_rule).collect(),
    })
}

/// Project one internal [`RuleTrace`] onto the public shape.
fn evaluated_rule(trace: RuleTrace) -> EvaluatedRule {
    EvaluatedRule {
        id: trace.id,
        priority: trace.priority,
        region: Region::as_str(trace.region),
        state: trace.state.map(|s| s.as_str().to_owned()),
        matched: trace.matched,
        visible_blocker: trace.visible_blocker,
        visible_idle: trace.visible_idle,
        visible_working: trace.visible_working,
        skip_state_update: trace.skip_state_update,
        evidence: evidence(trace.predicate),
    }
}

/// Project one internal [`PredicateTrace`] onto the public shape.
fn evidence(trace: PredicateTrace) -> PredicateEvidence {
    PredicateEvidence {
        op: trace.op.to_owned(),
        pattern: trace.pattern,
        matched: trace.matched,
        children: trace.children.into_iter().map(evidence).collect(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{Capture, explain, kinds, resolve_kind};

    /// Flatten an evidence tree to its leaves: `(pattern, matched)`.
    fn leaves(node: &super::PredicateEvidence, out: &mut Vec<(String, bool)>) {
        if node.children.is_empty() {
            out.push((node.pattern.clone().unwrap_or_default(), node.matched));
        }
        for child in &node.children {
            leaves(child, out);
        }
    }

    fn capture(title: &str, body: &str) -> Capture {
        Capture {
            title: title.to_owned(),
            lines: body.lines().map(str::to_owned).collect(),
        }
    }

    /// The committed golden capture of a live Claude Code permission dialog —
    /// the same bytes `rules.rs` pins the detector against, not a screen
    /// invented for this test. A self-referential fixture is precisely the
    /// mistake ADR-0046 records.
    const CLAUDE_BLOCKED: &str =
        include_str!("agent_detect/fixtures/claude/blocked_permission.txt");
    const CLAUDE_IDLE: &str = include_str!("agent_detect/fixtures/claude/idle_prompt.txt");

    #[test]
    fn every_builtin_kind_is_listed_and_resolvable() {
        let listed = kinds();
        for kind in ["claude", "codex", "opencode", "pi", "omp"] {
            assert!(
                listed.iter().any(|k| k == kind),
                "{kind} missing: {listed:?}"
            );
            assert_eq!(resolve_kind(kind).as_deref(), Some(kind));
        }
        // A binary alias resolves to its kind ...
        assert_eq!(resolve_kind("claude-code").as_deref(), Some("claude"));
        assert_eq!(resolve_kind("opencode2").as_deref(), Some("opencode"));
        // ... and a name that is neither resolves to nothing.
        assert_eq!(resolve_kind("not-an-agent"), None);
    }

    #[test]
    fn an_unknown_kind_has_no_explanation() {
        assert!(explain("not-an-agent", &capture("", "hello")).is_none());
    }

    /// The captured blocked screen explains as blocked, names the rule that
    /// won, and carries the evidence that leaf-level predicates fired.
    #[test]
    fn the_captured_claude_dialog_explains_as_blocked_with_leaf_evidence() {
        let got =
            explain("claude", &capture("\u{2733} phux", CLAUDE_BLOCKED)).expect("claude manifest");
        assert_eq!(got.detector_state, "blocked");
        assert_eq!(got.state.as_deref(), Some("blocked"));
        assert_eq!(
            got.matched_rule.as_deref(),
            Some("prompt-permission-dialog")
        );
        assert!(got.fallback_reason.is_none());
        assert!(got.visible_blocker);

        let winner = got
            .evaluated_rules
            .iter()
            .find(|r| r.id == "prompt-permission-dialog")
            .expect("the winning rule is reported among the evaluated ones");
        assert!(winner.matched);
        assert!(winner.evidence.matched);
        // The evidence is a tree with real, inspectable leaves: at least one
        // leaf carries a pattern and says it fired.
        let mut found = Vec::new();
        leaves(&winner.evidence, &mut found);
        assert!(!found.is_empty(), "a rule's evidence must reach its leaves");
        assert!(
            found.iter().all(|(pattern, _)| !pattern.is_empty()),
            "every leaf must name the pattern it ran: {found:?}",
        );
        assert!(found.iter().any(|(_, matched)| *matched));
    }

    /// EVERY rule is reported, not only the matching ones. A manifest author
    /// debugging a rule that never fires needs to see it listed as a miss;
    /// reporting only matches would render the failure invisible, which is
    /// the failure this tool exists for.
    #[test]
    fn misses_are_reported_too() {
        let got = explain("claude", &capture("", CLAUDE_IDLE)).expect("claude manifest");
        assert!(
            got.evaluated_rules.iter().any(|r| !r.matched),
            "a screen this quiet must leave some rule unmatched",
        );
        assert!(
            got.evaluated_rules
                .iter()
                .any(|r| r.id == "prompt-permission-dialog"),
            "a non-matching rule is still evaluated and reported",
        );
    }

    /// The idle golden asserts no state at all: `idle` is reached by the
    /// fail-safe, and the explanation says so instead of pretending a rule
    /// decided it.
    #[test]
    fn the_captured_idle_screen_reports_the_fail_safe_rather_than_a_rule() {
        let got =
            explain("claude", &capture("\u{2733} phux", CLAUDE_IDLE)).expect("claude manifest");
        assert_eq!(got.state, None, "no rule claims the idle screen");
        assert_eq!(got.detector_state, "idle");
        assert!(got.matched_rule.is_none());
        let reason = got.fallback_reason.expect("a fail-safe needs a reason");
        assert!(reason.contains("fails safe"), "{reason}");
    }

    /// THE point of the tool. Every region is previewed, and an empty one is
    /// flagged — a rule scoped to an empty region cannot match, and that is
    /// the mistake ADR-0046 documents.
    #[test]
    fn every_region_is_previewed_and_an_empty_one_is_flagged() {
        let got = explain("claude", &capture("", CLAUDE_BLOCKED)).expect("claude manifest");
        let names: Vec<&str> = got.regions.iter().map(|r| r.region.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "title",
                "prompt-box",
                "after-last-rule",
                "bottom-lines",
                "viewport"
            ],
        );
        let title = got
            .regions
            .iter()
            .find(|r| r.region == "title")
            .expect("title region");
        assert!(
            title.empty,
            "an unsupplied title is an EMPTY region and must read as one",
        );
        let live = got
            .regions
            .iter()
            .find(|r| r.region == "after-last-rule")
            .expect("after-last-rule region");
        assert!(!live.empty);
        assert!(
            live.lines.iter().any(|l| l.contains("Do you want")),
            "the preview is the region's real text: {:?}",
            live.lines,
        );
    }

    /// The explainer must never disagree with the detector. Both goldens,
    /// through both entry points.
    #[test]
    fn the_explanation_agrees_with_the_production_evaluator() {
        use crate::agent_detect::regions::Screen;

        let set = crate::agent_detect::rules::global();
        let manifest = set.manifest("claude").expect("claude manifest");
        for (title, body) in [
            ("\u{2733} phux", CLAUDE_BLOCKED),
            ("\u{2802} phux", CLAUDE_BLOCKED),
            ("\u{2733} phux", CLAUDE_IDLE),
            ("", CLAUDE_IDLE),
        ] {
            let lines: Vec<String> = body.lines().map(str::to_owned).collect();
            let screen = Screen {
                title,
                lines: &lines,
            };
            let direct = manifest.evaluate(&screen);
            let explained = manifest.explain(&screen);
            assert_eq!(
                direct, explained.evaluation,
                "explain must reuse the production verdict, not re-derive it",
            );
        }
    }
}
