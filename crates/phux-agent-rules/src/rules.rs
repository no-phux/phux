//! Declarative, region-scoped detection rules (ADR-0046 §C).
//!
//! Rules are data: one TOML manifest per agent kind, built in and
//! overridable from a config directory, so an operator can repair a detection
//! without a release. Predicates (`contains` / `regex` / `line-regex` / `all`
//! / `any` / `not`) compile once at load. A manifest with an invalid regex,
//! unknown state, unparseable region, or anything over the load-time bounds
//! is logged and dropped whole: a half-applied manifest is worse than none,
//! because the `idle` fail-safe hides the seam.

use std::collections::HashMap;
use std::rc::Rc;

use regex::Regex;
use serde::Deserialize;
use tracing::{debug, warn};

use super::DetectedState;
use super::explain::{EvaluatedRule, PredicateEvidence};
use super::regions::{Region, Screen, extract};

/// Built-in manifests, each pinned by captured-screen tests below.
const BUILTIN_MANIFESTS: [(&str, &str); 12] = [
    ("fx", include_str!("../rules/fx.toml")),
    ("claude", include_str!("../rules/claude.toml")),
    ("codex", include_str!("../rules/codex.toml")),
    ("opencode", include_str!("../rules/opencode.toml")),
    ("pi", include_str!("../rules/pi.toml")),
    ("omp", include_str!("../rules/omp.toml")),
    ("grok", include_str!("../rules/grok.toml")),
    ("amp", include_str!("../rules/amp.toml")),
    ("cursor-agent", include_str!("../rules/cursor-agent.toml")),
    ("gemini", include_str!("../rules/gemini.toml")),
    ("goose", include_str!("../rules/goose.toml")),
    ("aider", include_str!("../rules/aider.toml")),
];

/// `PHUX_AGENT_DETECT=0` disables the detector (an empty rule set).
const ENV_DETECT: &str = "PHUX_AGENT_DETECT";

/// Directory of `*.toml` manifests overriding or extending the built-ins.
const ENV_RULES_DIR: &str = "PHUX_AGENT_RULES_DIR";

// Load-time bounds. Evaluation runs per agent pane every 100-500 ms on the
// shared current-thread runtime, and manifests load from a config directory,
// so these caps bound aggregate work and resident compiled-regex memory
// (`regex` is linear-time, so this is not a ReDoS guard). They are far above
// anything real and are enforced at compile time, dropping the manifest whole.

/// Most rules one manifest may declare.
const MAX_RULES_PER_MANIFEST: usize = 128;

/// Deepest a predicate tree may nest, root at 1. The TOML parser also refuses
/// deep nesting, but that is a dependency's internal limit, not the schema's.
const MAX_PREDICATE_DEPTH: usize = 8;

/// Most leaf matchers (`contains` / `regex` / `line-regex`) in one rule.
const MAX_MATCHERS_PER_RULE: usize = 32;

/// Most leaf matchers across a whole manifest (bounds resident regexes).
const MAX_MATCHERS_PER_MANIFEST: usize = 1024;

/// Longest pattern or needle a leaf matcher may carry, in characters.
const MAX_MATCHER_CHARS: usize = 512;

/// Largest override manifest that will be read, in bytes.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// Most `*.toml` files the override directory contributes (in sorted order).
const MAX_OVERRIDE_MANIFESTS: usize = 64;

// ---------------------------------------------------------------------------
// Deserialized manifest shape
// ---------------------------------------------------------------------------

/// A predicate over a region's text, as written in TOML.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PredicateSpec {
    /// Case-insensitive substring over the region joined with newlines.
    Contains(String),
    /// Regex over the region joined with newlines.
    Regex(String),
    /// Regex that must match at least one whole line of the region.
    LineRegex(String),
    /// Every child must match.
    All(Vec<Self>),
    /// At least one child must match.
    Any(Vec<Self>),
    /// The child must not match.
    Not(Box<Self>),
}

/// One rule, as written in TOML. `deny_unknown_fields` is load-bearing: a
/// typo'd flag must drop the manifest, not be silently ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent, orthogonal assertion a rule may make about the screen; \
              collapsing them into an enum would forbid the combinations the manifests need"
)]
pub(crate) struct RuleSpec {
    /// Stable identifier, for logs and for an operator's override file.
    pub(crate) id: String,
    /// The state this rule asserts. `None` for a pure-flag rule (e.g. a
    /// `skip-state-update` freeze rule), which asserts nothing.
    #[serde(default)]
    pub(crate) state: Option<String>,
    /// Higher wins among matching rules of the same region class.
    #[serde(default)]
    pub(crate) priority: i32,
    /// The screen sub-slice this rule matches against.
    pub(crate) region: Region,
    /// The predicate tree.
    #[serde(rename = "match")]
    pub(crate) predicate: PredicateSpec,
    /// The screen positively shows the agent is idle, bypassing the working
    /// -> idle hold (ADR-0046 point 6). Distinct from `state`, because idle is
    /// otherwise the fail-safe reached by nothing matching.
    #[serde(default)]
    pub(crate) visible_idle: bool,
    /// The screen (a pager, a picker) carries no agent-state information:
    /// freeze the last derivation.
    #[serde(default)]
    pub(crate) skip_state_update: bool,
}

/// One agent kind's manifest, as written in TOML.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ManifestSpec {
    /// Open-vocabulary kind slug, e.g. `"claude"`. Also the override key.
    pub(crate) kind: String,
    /// Human-facing name for the record's `name` field; defaults to `kind`.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// argv basenames (and program-path components) that identify this
    /// agent. The daemon's process probe matches these names.
    pub(crate) binaries: Vec<String>,
    /// The rules, in declaration order (the final tiebreak).
    #[serde(default)]
    pub(crate) rules: Vec<RuleSpec>,
}

// ---------------------------------------------------------------------------
// Compiled form
// ---------------------------------------------------------------------------

include!("rules/predicate.rs");

/// A whole-manifest evaluation with its working shown.
#[derive(Debug)]
pub(crate) struct Explanation {
    /// Exactly what [`CompiledManifest::evaluate`] returns, from the same pass.
    pub(crate) evaluation: Evaluation,
    /// Every rule, in declaration order, matched or not.
    pub(crate) rules: Vec<EvaluatedRule>,
    /// The text every previewed region resolved to, empty ones included.
    pub(crate) regions: Vec<(Region, Vec<String>)>,
}

/// A region's text, materialized once per tick and shared by its rules.
struct RegionText<'a> {
    lines: Vec<&'a str>,
    joined: String,
    lowered: String,
}

impl<'a> RegionText<'a> {
    fn new(region: Region, screen: &Screen<'a>) -> Self {
        let lines = extract(region, screen);
        let joined = lines.join("\n");
        let lowered = joined.to_lowercase();
        Self {
            lines,
            joined,
            lowered,
        }
    }
}

/// A compiled rule.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "compiled mirror of RuleSpec's independent flags; see that struct"
)]
pub(crate) struct Rule {
    /// Stable identifier, for `trace` logs.
    pub(crate) id: String,
    /// The state this rule asserts, if any.
    pub(crate) state: Option<DetectedState>,
    /// Higher wins.
    pub(crate) priority: i32,
    /// The screen sub-slice this rule reads.
    pub(crate) region: Region,
    /// The compiled predicate tree.
    pub(crate) predicate: Predicate,
    /// See [`RuleSpec::visible_idle`].
    pub(crate) visible_idle: bool,
    /// See [`RuleSpec::skip_state_update`].
    pub(crate) skip_state_update: bool,
}

/// A compiled manifest (keyed by kind in [`RuleSet`]).
#[derive(Debug)]
pub struct CompiledManifest {
    /// Human-facing name written into the `phux.agent/v1` record.
    pub name: String,
    /// Rules in declaration order.
    pub(crate) rules: Vec<Rule>,
}

/// What a full rule-set evaluation concluded about one screen.
#[derive(Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the union of the matching rules' independent flags; see RuleSpec"
)]
pub struct Evaluation {
    /// The winning state, or `None` (the caller fails safe to `idle`).
    pub state: Option<DetectedState>,
    /// A matching rule positively asserts idleness.
    pub visible_idle: bool,
    /// A matching rule says the screen carries no state: freeze.
    pub freeze: bool,
    /// The winning rule's id, for `trace` logs.
    pub matched: Option<String>,
}

impl CompiledManifest {
    /// Evaluate every rule against `screen`. Title rules outrank screen
    /// rules (the title is the agent's own statement), then `priority`
    /// descending, then declaration order.
    #[must_use]
    pub fn evaluate(&self, screen: &Screen<'_>) -> Evaluation {
        self.run(screen, None)
    }

    /// [`Self::evaluate`] with the working shown, from the same pass so an
    /// explanation can never disagree with the detector.
    pub(crate) fn explain(&self, screen: &Screen<'_>) -> Explanation {
        let mut rules = Vec::with_capacity(self.rules.len());
        let evaluation = self.run(screen, Some(&mut rules));
        // The default regions, then any window the manifest itself names.
        let mut previewed: Vec<Region> = Region::ALL.to_vec();
        for rule in &self.rules {
            if !previewed.contains(&rule.region) {
                previewed.push(rule.region);
            }
        }
        let regions = previewed
            .into_iter()
            .map(|region| {
                let text = RegionText::new(region, screen);
                let lines = text.lines.iter().map(|l| (*l).to_owned()).collect();
                (region, lines)
            })
            .collect();
        Explanation {
            evaluation,
            rules,
            regions,
        }
    }

    /// The one evaluation pass; `trace` collects every rule's evidence.
    fn run(&self, screen: &Screen<'_>, mut trace: Option<&mut Vec<EvaluatedRule>>) -> Evaluation {
        let mut texts: HashMap<Region, RegionText<'_>> = HashMap::new();
        let mut out = Evaluation::default();
        // (is_title, priority, declaration index) of the current winner.
        let mut best: Option<(bool, i32, usize)> = None;

        for (idx, rule) in self.rules.iter().enumerate() {
            let text = texts
                .entry(rule.region)
                .or_insert_with(|| RegionText::new(rule.region, screen));
            let matched = rule.predicate.eval(text);
            if let Some(sink) = trace.as_deref_mut() {
                sink.push(EvaluatedRule {
                    id: rule.id.clone(),
                    priority: rule.priority,
                    region: rule.region.as_str(),
                    state: rule.state.map(|s| s.as_str().to_owned()),
                    matched,
                    visible_idle: rule.visible_idle,
                    skip_state_update: rule.skip_state_update,
                    evidence: rule.predicate.trace(text),
                });
            }
            if !matched {
                continue;
            }
            out.visible_idle |= rule.visible_idle;
            out.freeze |= rule.skip_state_update;
            let Some(state) = rule.state else { continue };
            let key = (rule.region == Region::Title, rule.priority, idx);
            let wins = best.is_none_or(|(t, p, i)| {
                (key.0, key.1) > (t, p) || (key.0, key.1) == (t, p) && key.2 < i
            });
            if wins {
                best = Some(key);
                out.state = Some(state);
                out.matched = Some(rule.id.clone());
            }
        }
        out
    }
}

/// The process-wide compiled rule set: every known agent kind, plus the
/// argv-basename index used to identify one.
#[derive(Debug, Default)]
pub struct RuleSet {
    manifests: HashMap<String, CompiledManifest>,
    /// binary name (or program-path component) -> kind.
    by_binary: HashMap<String, String>,
}

impl RuleSet {
    /// `true` when nothing is loaded (the actor then builds no detector).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    /// The agent kind a program named `name` belongs to, if any. `name` is
    /// matched case-insensitively.
    pub fn kind_for_binary(&self, name: &str) -> Option<&str> {
        self.by_binary.get(&name.to_lowercase()).map(String::as_str)
    }

    /// The compiled manifest for `kind`.
    #[must_use]
    pub fn manifest(&self, kind: &str) -> Option<&CompiledManifest> {
        self.manifests.get(kind)
    }

    /// Every loaded kind slug, sorted.
    pub(crate) fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self.manifests.keys().cloned().collect();
        kinds.sort_unstable();
        kinds
    }

    /// Compile and install `spec`, replacing any manifest of the same kind.
    ///
    /// # Errors
    ///
    /// A human-readable reason the manifest is unusable. Nothing is
    /// inserted until the whole manifest compiles, so a rejection leaves no
    /// partial state.
    pub fn install(&mut self, spec: ManifestSpec) -> Result<(), String> {
        if spec.kind.is_empty() {
            return Err("manifest has an empty `kind`".to_owned());
        }
        if spec.rules.len() > MAX_RULES_PER_MANIFEST {
            return Err(format!(
                "{} rules, over the {MAX_RULES_PER_MANIFEST} limit",
                spec.rules.len()
            ));
        }
        let mut budget = Budget::default();
        let mut rules = Vec::with_capacity(spec.rules.len());
        for rule in &spec.rules {
            let state = match rule.state.as_deref() {
                None => None,
                Some(word) => Some(
                    parse_state(word)
                        .ok_or_else(|| format!("rule `{}`: unknown state `{word}`", rule.id))?,
                ),
            };
            budget.rule_matchers = 0;
            let predicate = Predicate::compile(&rule.predicate, 1, &mut budget)
                .map_err(|e| format!("rule `{}`: {e}", rule.id))?;
            rules.push(Rule {
                id: rule.id.clone(),
                state,
                priority: rule.priority,
                region: rule.region,
                predicate,
                visible_idle: rule.visible_idle,
                skip_state_update: rule.skip_state_update,
            });
        }
        // Drop any binary index entries pointing at a manifest we replace.
        self.by_binary.retain(|_, kind| *kind != spec.kind);
        for binary in &spec.binaries {
            self.by_binary
                .insert(binary.to_lowercase(), spec.kind.clone());
        }
        let name = spec.name.unwrap_or_else(|| spec.kind.clone());
        self.manifests
            .insert(spec.kind, CompiledManifest { name, rules });
        Ok(())
    }
}

/// Parse a `state` word from a manifest.
fn parse_state(word: &str) -> Option<DetectedState> {
    match word {
        "idle" => Some(DetectedState::Idle),
        "working" => Some(DetectedState::Working),
        "blocked" => Some(DetectedState::Blocked),
        "done" => Some(DetectedState::Done),
        _ => None,
    }
}

/// Parse and install one TOML manifest, logging and dropping it whole on
/// any error.
fn load_manifest(set: &mut RuleSet, source: &str, toml_text: &str) {
    match toml::from_str::<ManifestSpec>(toml_text) {
        Ok(spec) => {
            let kind = spec.kind.clone();
            if let Err(reason) = set.install(spec) {
                warn!(%source, %kind, %reason, "agent-detect: manifest dropped");
            } else {
                debug!(%source, %kind, "agent-detect: manifest loaded");
            }
        }
        Err(err) => {
            warn!(%source, error = %err, "agent-detect: manifest is not valid TOML; dropped");
        }
    }
}

/// Build the rule set from the built-ins plus any operator overrides.
fn build() -> RuleSet {
    let mut set = RuleSet::default();
    if std::env::var(ENV_DETECT).as_deref() == Ok("0") {
        debug!("agent-detect: disabled by PHUX_AGENT_DETECT=0");
        return set;
    }
    for (kind, manifest) in BUILTIN_MANIFESTS {
        load_manifest(&mut set, &format!("builtin:{kind}"), manifest);
    }

    let Some(dir) = overrides_dir() else {
        return set;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return set;
    };
    // Sorted, so overrides and the cap resolve the same way on every boot.
    let mut paths: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    if paths.len() > MAX_OVERRIDE_MANIFESTS {
        warn!(
            dir = %dir.display(),
            found = paths.len(),
            limit = MAX_OVERRIDE_MANIFESTS,
            "agent-detect: too many override manifests; loading the first {MAX_OVERRIDE_MANIFESTS} \
             in sorted order and ignoring the rest",
        );
        paths.truncate(MAX_OVERRIDE_MANIFESTS);
    }
    for path in paths {
        // Size-check before reading, so a huge file is never pulled in.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_MANIFEST_BYTES => {
                warn!(
                    path = %path.display(),
                    bytes = meta.len(),
                    limit = MAX_MANIFEST_BYTES,
                    "agent-detect: manifest is too large; dropped unread",
                );
                continue;
            }
            Ok(_) => {}
            Err(err) => {
                warn!(path = %path.display(), error = %err, "agent-detect: unreadable manifest");
                continue;
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => load_manifest(&mut set, &path.to_string_lossy(), &text),
            Err(err) => {
                warn!(path = %path.display(), error = %err, "agent-detect: unreadable manifest");
            }
        }
    }
    set
}

/// `$PHUX_AGENT_RULES_DIR`, else `$XDG_CONFIG_HOME/phux/agent-rules`, else
/// `$HOME/.config/phux/agent-rules`.
fn overrides_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var(ENV_RULES_DIR) {
        return (!dir.is_empty()).then(|| std::path::PathBuf::from(dir));
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("phux").join("agent-rules"))
}

thread_local! {
    /// Compiled once per (in practice, the one) runtime thread on first use.
    static RULES: std::cell::OnceCell<Rc<RuleSet>> = const { std::cell::OnceCell::new() };
}

/// The shared, compiled rule set.
#[must_use]
pub fn global() -> Rc<RuleSet> {
    RULES.with(|cell| Rc::clone(cell.get_or_init(|| Rc::new(build()))))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod tests {
    include!("rules/tests.rs");
}
