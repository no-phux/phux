//! Offline evaluation of the compiled agent-detection manifests against a
//! captured screen (ADR-0046), with no server or PTY.
//!
//! Manifest authors need to see which region their rule read and which leaf
//! fired: rules once written against an imagined TUI matched nothing, and the
//! `idle` fail-safe hid it. These types own their data and derive `Serialize`
//! so `--json` is a projection. Identification (the PTY's foreground process)
//! is not available offline, so the caller names the kind.

use serde::Serialize;

use crate::regions::Screen;
use crate::rules;

/// A screen to evaluate. `title` is separate because a grid capture does not
/// carry it; empty is legitimate.
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
    /// Children of a combinator, all evaluated (no short-circuit).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Self>,
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
    /// `visible-idle`.
    pub visible_idle: bool,
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
    /// The region resolved to nothing, so no rule scoped to it can match.
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
    /// What the detector would publish: `frozen` when a `skip-state-update`
    /// rule matched, else the asserted state, else the `idle` fail-safe.
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
    /// A matching rule positively asserts idleness.
    pub visible_idle: bool,
    /// Every region, resolved against this screen. Includes regions no rule
    /// names: picking the right one is half of authoring a rule.
    pub regions: Vec<RegionPreview>,
    /// Every rule, in declaration order, matched or not.
    pub evaluated_rules: Vec<EvaluatedRule>,
}

/// Every agent kind with a loaded manifest, sorted, through the server's own
/// load path (overrides included).
#[must_use]
pub fn kinds() -> Vec<String> {
    rules::global().kinds()
}

/// Resolve `name` to a kind slug: an exact kind, else a binary alias.
#[must_use]
pub fn resolve_kind(name: &str) -> Option<String> {
    let set = rules::global();
    if set.manifest(name).is_some() {
        return Some(name.to_owned());
    }
    set.kind_for_binary(name).map(str::to_owned)
}

/// Evaluate `kind`'s manifest against `capture`; `None` when none is loaded.
#[must_use]
pub fn explain(kind: &str, capture: &Capture) -> Option<Explanation> {
    let set = rules::global();
    let manifest = set.manifest(kind)?;
    let screen = Screen {
        title: &capture.title,
        progress: "",
        lines: &capture.lines,
    };
    let explained = manifest.explain(&screen);
    let evaluation = &explained.evaluation;

    let state = evaluation.state.map(|s| s.as_str().to_owned());
    // Mirrors `AgentDetector::tick`: freeze wins over the fail-safe.
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
        visible_idle: evaluation.visible_idle,
        regions: explained
            .regions
            .into_iter()
            .map(|(region, lines)| RegionPreview {
                region: region.as_str(),
                empty: lines.iter().all(|line| line.trim().is_empty()),
                lines,
            })
            .collect(),
        evaluated_rules: explained.rules,
    })
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
    const CLAUDE_BLOCKED: &str = include_str!("fixtures/claude/blocked_permission.txt");
    const CLAUDE_IDLE: &str = include_str!("fixtures/claude/idle_prompt.txt");

    #[test]
    fn every_loaded_kind_is_listed_and_resolvable() {
        let listed = kinds();
        assert!(!listed.is_empty(), "built-in manifests must load");
        for kind in listed {
            assert_eq!(
                resolve_kind(&kind).as_deref(),
                Some(kind.as_str()),
                "{kind} must resolve to itself",
            );
            assert!(
                explain(&kind, &capture("", "")).is_some(),
                "{kind} must be explainable",
            );
        }
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
                "osc-progress",
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
        let progress = got
            .regions
            .iter()
            .find(|r| r.region == "osc-progress")
            .expect("osc-progress region");
        assert!(progress.empty, "offline captures carry no raw OSC progress");
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
}
