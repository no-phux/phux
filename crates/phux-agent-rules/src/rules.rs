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
const BUILTIN_MANIFESTS: [(&str, &str); 8] = [
    ("claude", include_str!("../rules/claude.toml")),
    ("codex", include_str!("../rules/codex.toml")),
    ("opencode", include_str!("../rules/opencode.toml")),
    ("pi", include_str!("../rules/pi.toml")),
    ("omp", include_str!("../rules/omp.toml")),
    ("grok", include_str!("../rules/grok.toml")),
    ("amp", include_str!("../rules/amp.toml")),
    ("cursor-agent", include_str!("../rules/cursor-agent.toml")),
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

/// A compiled predicate tree. Regexes are built once, at manifest load.
#[derive(Debug)]
pub(crate) enum Predicate {
    /// Needle, pre-lowercased at compile time.
    Contains(String),
    /// Matched against the region joined with newlines.
    Regex(Regex),
    /// Matched against each line of the region until one hits.
    LineRegex(Regex),
    /// Conjunction.
    All(Vec<Self>),
    /// Disjunction.
    Any(Vec<Self>),
    /// Negation.
    Not(Box<Self>),
}

/// The load-time matcher budget, per rule and per manifest.
#[derive(Debug, Default)]
struct Budget {
    /// Leaf matchers compiled for the rule currently being compiled.
    rule_matchers: usize,
    /// Leaf matchers compiled for the manifest so far.
    manifest_matchers: usize,
}

impl Budget {
    /// Charge one leaf matcher, or fail the manifest.
    fn charge_matcher(&mut self) -> Result<(), String> {
        self.rule_matchers += 1;
        self.manifest_matchers += 1;
        if self.rule_matchers > MAX_MATCHERS_PER_RULE {
            return Err(format!(
                "more than {MAX_MATCHERS_PER_RULE} matchers in one rule"
            ));
        }
        if self.manifest_matchers > MAX_MATCHERS_PER_MANIFEST {
            return Err(format!(
                "more than {MAX_MATCHERS_PER_MANIFEST} matchers in the manifest"
            ));
        }
        Ok(())
    }
}

/// Charge one leaf, rejecting an over-long pattern (counted in characters).
fn charge_leaf(op: &str, pattern: &str, budget: &mut Budget) -> Result<(), String> {
    let len = pattern.chars().count();
    if len > MAX_MATCHER_CHARS {
        return Err(format!(
            "{op} pattern is {len} characters, over the {MAX_MATCHER_CHARS} limit"
        ));
    }
    budget.charge_matcher()
}

impl Predicate {
    /// Compile a spec at `depth` (root 1), checked before any work.
    fn compile(spec: &PredicateSpec, depth: usize, budget: &mut Budget) -> Result<Self, String> {
        if depth > MAX_PREDICATE_DEPTH {
            return Err(format!(
                "predicate nests deeper than {MAX_PREDICATE_DEPTH} levels"
            ));
        }
        Ok(match spec {
            PredicateSpec::Contains(needle) => {
                charge_leaf("contains", needle, budget)?;
                Self::Contains(needle.to_lowercase())
            }
            PredicateSpec::Regex(pat) => {
                charge_leaf("regex", pat, budget)?;
                Self::Regex(Regex::new(pat).map_err(|e| format!("regex `{pat}`: {e}"))?)
            }
            PredicateSpec::LineRegex(pat) => {
                charge_leaf("line-regex", pat, budget)?;
                Self::LineRegex(Regex::new(pat).map_err(|e| format!("line-regex `{pat}`: {e}"))?)
            }
            PredicateSpec::All(children) => Self::All(
                children
                    .iter()
                    .map(|child| Self::compile(child, depth + 1, budget))
                    .collect::<Result<_, _>>()?,
            ),
            PredicateSpec::Any(children) => Self::Any(
                children
                    .iter()
                    .map(|child| Self::compile(child, depth + 1, budget))
                    .collect::<Result<_, _>>()?,
            ),
            PredicateSpec::Not(child) => {
                Self::Not(Box::new(Self::compile(child, depth + 1, budget)?))
            }
        })
    }

    /// The manifest keyword this node is written with.
    const fn op(&self) -> &'static str {
        match self {
            Self::Contains(_) => "contains",
            Self::Regex(_) => "regex",
            Self::LineRegex(_) => "line-regex",
            Self::All(_) => "all",
            Self::Any(_) => "any",
            Self::Not(_) => "not",
        }
    }

    /// A leaf's pattern as compiled (a `contains` needle is lowercased).
    fn pattern(&self) -> Option<String> {
        match self {
            Self::Contains(needle) => Some(needle.clone()),
            Self::Regex(re) | Self::LineRegex(re) => Some(re.as_str().to_owned()),
            Self::All(_) | Self::Any(_) | Self::Not(_) => None,
        }
    }

    /// Evaluate against a region's pre-computed text.
    fn eval(&self, text: &RegionText<'_>) -> bool {
        match self {
            Self::Contains(needle) => text.lowered.contains(needle.as_str()),
            Self::Regex(re) => re.is_match(&text.joined),
            Self::LineRegex(re) => text.lines.iter().any(|line| re.is_match(line)),
            Self::All(children) => children.iter().all(|c| c.eval(text)),
            Self::Any(children) => children.iter().any(|c| c.eval(text)),
            Self::Not(child) => !child.eval(text),
        }
    }

    /// Evaluate and record every node for the offline explainer, without
    /// short-circuiting, so an author sees which conjunct failed.
    fn trace(&self, text: &RegionText<'_>) -> PredicateEvidence {
        let children: Vec<PredicateEvidence> = match self {
            Self::Contains(_) | Self::Regex(_) | Self::LineRegex(_) => Vec::new(),
            Self::All(kids) | Self::Any(kids) => kids.iter().map(|c| c.trace(text)).collect(),
            Self::Not(child) => vec![child.trace(text)],
        };
        let matched = match self {
            Self::Contains(_) | Self::Regex(_) | Self::LineRegex(_) => self.eval(text),
            Self::All(_) => children.iter().all(|c| c.matched),
            Self::Any(_) => children.iter().any(|c| c.matched),
            Self::Not(_) => !children.first().is_some_and(|c| c.matched),
        };
        PredicateEvidence {
            op: self.op().to_owned(),
            pattern: self.pattern(),
            matched,
            children,
        }
    }
}

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
    use super::{Evaluation, ManifestSpec, RuleSet, global};
    use crate::DetectedState::{self, Blocked, Idle, Working};
    use crate::regions::Screen;

    fn compile(toml_text: &str) -> RuleSet {
        let spec: ManifestSpec = toml::from_str(toml_text).expect("manifest parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("manifest compiles");
        set
    }

    fn builtin(kind: &str) -> &'static str {
        super::BUILTIN_MANIFESTS
            .iter()
            .find_map(|(candidate, manifest)| (*candidate == kind).then_some(*manifest))
            .expect("built-in manifest")
    }

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    /// A committed golden viewport under `src/fixtures/`.
    fn golden(rel: &str) -> Vec<String> {
        let path = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR")
                .expect("the test runner sets CARGO_MANIFEST_DIR"),
        )
        .join("src/fixtures")
        .join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn eval(set: &RuleSet, kind: &str, title: &str, progress: &str, buf: &[String]) -> Evaluation {
        set.manifest(kind).expect("manifest").evaluate(&Screen {
            title,
            progress,
            lines: buf,
        })
    }

    fn install(toml_text: &str) -> Result<RuleSet, String> {
        let spec: ManifestSpec = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        let mut set = RuleSet::default();
        set.install(spec)?;
        Ok(set)
    }

    /// A one-rule manifest around `rule_body` (the lines after `[[rules]]`).
    fn one_rule(rule_body: &str) -> String {
        format!("kind = \"k\"\nbinaries = [\"k\"]\n[[rules]]\nid = \"r\"\n{rule_body}\n")
    }

    const SAMPLE: &str = r#"
kind = "sample"
name = "Sample"
binaries = ["sample", "sample-cli"]

[[rules]]
id = "title-working"
state = "working"
priority = 10
region = "title"
match = { line-regex = "^W " }

[[rules]]
id = "screen-blocked"
state = "blocked"
priority = 90
region = "bottom-lines"
match = { all = [ { contains = "do you want" }, { line-regex = "^\\s*\\d+\\." } ] }

[[rules]]
id = "screen-idle"
state = "idle"
priority = 40
region = "bottom-lines"
visible-idle = true
match = { contains = "ready" }

[[rules]]
id = "pager"
priority = 200
region = "bottom-lines"
skip-state-update = true
match = { contains = "-- pager --" }
"#;

    /// `(title, screen, state, matched rule, visible_idle, freeze)`.
    type SampleCase<'a> = (
        &'a str,
        &'a [&'a str],
        Option<DetectedState>,
        Option<&'a str>,
        bool,
        bool,
    );

    /// Title rules outrank screen rules regardless of priority, priority
    /// orders within a class, `all` needs every child, flags union across
    /// matching rules, and nothing matching yields no state (the caller's
    /// fail-safe decides).
    #[test]
    fn evaluation_orders_rules_and_unions_flags() {
        let set = compile(SAMPLE);
        assert_eq!(set.kind_for_binary("SAMPLE-CLI"), Some("sample"));
        assert_eq!(set.kind_for_binary("nope"), None);
        let dialog = ["do you want to proceed?", " 1. Yes"];
        let cases: &[SampleCase<'_>] = &[
            (
                "W busy",
                &dialog,
                Some(Working),
                Some("title-working"),
                false,
                false,
            ),
            (
                "idle",
                &["ready", dialog[0], dialog[1]],
                Some(Blocked),
                Some("screen-blocked"),
                true,
                false,
            ),
            ("", &[dialog[0]], None, None, false, false),
            ("", &["nothing interesting here"], None, None, false, false),
            (
                "",
                &[dialog[0], dialog[1], "-- pager --"],
                Some(Blocked),
                Some("screen-blocked"),
                false,
                true,
            ),
        ];
        for &(title, screen, state, matched, visible_idle, freeze) in cases {
            let got = eval(&set, "sample", title, "", &lines(screen));
            assert_eq!(got.state, state, "{title} {screen:?}");
            assert_eq!(got.matched.as_deref(), matched, "{title} {screen:?}");
            assert_eq!(
                (got.visible_idle, got.freeze),
                (visible_idle, freeze),
                "{screen:?}"
            );
        }
    }

    #[test]
    fn not_combinator_negates() {
        let set = compile(&one_rule(
            "state = \"idle\"\nregion = \"viewport\"\n\
             match = { all = [ { contains = \"prompt\" }, { not = { contains = \"pager\" } } ] }",
        ));
        assert_eq!(
            eval(&set, "k", "", "", &lines(&["prompt", "pager"])).state,
            None
        );
        assert_eq!(
            eval(&set, "k", "", "", &lines(&["prompt"])).state,
            Some(Idle)
        );
    }

    /// Kebab-case flags bind (without `rename_all` they would be silently
    /// ignored), and any unusable manifest is rejected whole: a bad regex,
    /// an unknown state, a removed or mis-cased flag, a malformed region.
    #[test]
    fn manifests_bind_their_flags_or_are_rejected_whole() {
        let set = compile(&one_rule(
            "state = \"idle\"\nregion = \"title\"\nvisible-idle = true\n\
             skip-state-update = true\nmatch = { contains = \"x\" }",
        ));
        let rule = &set.manifest("k").expect("manifest").rules[0];
        assert!(rule.visible_idle && rule.skip_state_update);

        for body in [
            "state = \"idle\"\nregion = \"title\"\nmatch = { regex = \"(unclosed\" }",
            "state = \"confused\"\nregion = \"title\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title\"\nvisible-blocker = true\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title\"\nvisible-working = true\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title\"\nvisible_idle = true\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"bottom-lines(0)\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title(2)\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"bottom_lines\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"nonsense\"\nmatch = { contains = \"x\" }",
        ] {
            assert!(install(&one_rule(body)).is_err(), "{body}");
        }
    }

    /// Claude's OSC 9;4 remove signal is the only shipped positive-idle
    /// source; idle is otherwise the fail-safe reached by nothing matching.
    #[test]
    fn only_claudes_captured_progress_sets_visible_idle() {
        let visible_idle: Vec<(&str, String)> = super::BUILTIN_MANIFESTS
            .iter()
            .flat_map(|(kind, text)| {
                let spec: ManifestSpec = toml::from_str(text).expect("builtin parses");
                spec.rules
                    .into_iter()
                    .filter(|rule| rule.visible_idle)
                    .map(move |rule| (*kind, rule.id))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(visible_idle, [("claude", "osc-progress-idle".to_owned())]);
    }

    /// `docs/spec/L3.md` §3.7 states the shipped per-state rule counts as the
    /// basis of its level-versus-edge ruling. If this fails, update that
    /// sentence and re-read its paragraph: a new `idle` or `done` rule may
    /// invalidate the reasoning, not just the arithmetic.
    #[test]
    fn the_spec_paragraph_reports_the_real_manifest_rule_counts() {
        const WORDS: [&str; 21] = [
            "zero",
            "one",
            "two",
            "three",
            "four",
            "five",
            "six",
            "seven",
            "eight",
            "nine",
            "ten",
            "eleven",
            "twelve",
            "thirteen",
            "fourteen",
            "fifteen",
            "sixteen",
            "seventeen",
            "eighteen",
            "nineteen",
            "twenty",
        ];
        let count = |state: &str| -> String {
            let n: usize = super::BUILTIN_MANIFESTS
                .iter()
                .map(|(_, text)| {
                    let spec: ManifestSpec = toml::from_str(text).expect("builtin parses");
                    spec.rules
                        .iter()
                        .filter(|rule| rule.state.as_deref() == Some(state))
                        .count()
                })
                .sum();
            WORDS
                .get(n)
                .map_or_else(|| n.to_string(), |w| (*w).to_owned())
        };
        let expected = format!(
            "declare {} `working` rules, {} `blocked` rules, exactly {} `idle` rule and {} \
             `done` rules between them.",
            count("working"),
            count("blocked"),
            count("idle"),
            count("done"),
        );
        let spec_path = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR")
                .expect("the test runner sets CARGO_MANIFEST_DIR"),
        )
        .join("../../docs/spec/L3.md");
        let spec = std::fs::read_to_string(&spec_path).expect("read L3.md");
        let flattened = spec.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flattened.contains(&expected),
            "docs/spec/L3.md §3.7 no longer matches the shipped manifests; expected: {expected}",
        );
    }

    /// Windowed regions are distinct: two rules with different N read
    /// different text, and a top-anchored window reads only the banner.
    #[test]
    fn windowed_regions_read_their_own_text() {
        let set = compile(
            r#"
kind = "w"
binaries = ["w"]
[[rules]]
id = "last-row-only"
state = "working"
priority = 10
region = "bottom-lines(1)"
match = { contains = "spinner" }
[[rules]]
id = "footer-block"
state = "blocked"
priority = 20
region = "bottom-lines(6)"
match = { contains = "spinner" }
[[rules]]
id = "banner"
state = "idle"
priority = 30
region = "top-non-empty-lines"
match = { contains = "thinking" }
"#,
        );
        let got = eval(
            &set,
            "w",
            "",
            "",
            &lines(&["spinner", "a", "b", "c", "d", "e"]),
        );
        assert_eq!(got.matched.as_deref(), Some("footer-block"));
        let got = eval(&set, "w", "", "", &lines(&["a", "spinner"]));
        assert_eq!(got.matched.as_deref(), Some("footer-block"), "20 beats 10");
        let got = eval(&set, "w", "", "", &lines(&["", "  thinking...", "x"]));
        assert_eq!(got.matched.as_deref(), Some("banner"));
        let got = eval(&set, "w", "", "", &lines(&["  header", "  thinking..."]));
        assert_eq!(got.state, None, "a bare top window is one row");
    }

    /// The explainer previews the default regions plus every window the
    /// manifest names, once each, in the spelling an operator types.
    #[test]
    fn the_explainer_previews_every_window_the_manifest_names() {
        const DEFAULTS: [&str; 6] = [
            "title",
            "osc-progress",
            "prompt-box",
            "after-last-rule",
            "bottom-lines",
            "viewport",
        ];
        let set = compile(
            "kind = \"w\"\nbinaries = [\"w\"]\n\
             [[rules]]\nid = \"wide\"\nstate = \"idle\"\nregion = \"bottom-lines(14)\"\n\
             match = { contains = \"zzz\" }\n\
             [[rules]]\nid = \"banner\"\nstate = \"idle\"\nregion = \"top-non-empty-lines(2)\"\n\
             match = { contains = \"zzz\" }\n",
        );
        let buf = lines(&["a", "b", "c"]);
        let screen = Screen {
            title: "",
            progress: "",
            lines: &buf,
        };
        let explained = set.manifest("w").expect("manifest").explain(&screen);
        let names: Vec<String> = explained.regions.iter().map(|(r, _)| r.as_str()).collect();
        let windows = ["bottom-lines(14)", "top-non-empty-lines(2)"];
        assert_eq!(names, [&DEFAULTS[..], &windows[..]].concat());
        let rules: Vec<&str> = explained.rules.iter().map(|r| r.region.as_str()).collect();
        assert_eq!(rules, windows);

        // A bare `bottom-lines` is the default window, already listed.
        let set = compile(SAMPLE);
        let explained = set.manifest("sample").expect("manifest").explain(&screen);
        let names: Vec<String> = explained.regions.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(names, DEFAULTS);
    }

    // --- Load-time bounds: an over-bound manifest is rejected whole --------

    fn manifest_with_rules(count: usize) -> String {
        let body = (0..count)
            .map(|idx| {
                format!(
                    "[[rules]]\nid = \"r{idx}\"\nstate = \"idle\"\nregion = \"title\"\n\
                     match = {{ contains = \"x{idx}\" }}"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("kind = \"big\"\nbinaries = [\"big\"]\n{body}\n")
    }

    fn contains_rule(needle: &str) -> String {
        one_rule(&format!(
            "state = \"idle\"\nregion = \"title\"\nmatch = {{ contains = \"{needle}\" }}"
        ))
    }

    #[test]
    fn rule_count_and_pattern_length_are_capped_inclusively() {
        let set = install(&manifest_with_rules(super::MAX_RULES_PER_MANIFEST)).expect("at cap");
        assert_eq!(set.manifest("big").expect("manifest").rules.len(), 128);

        let spec: ManifestSpec =
            toml::from_str(&manifest_with_rules(super::MAX_RULES_PER_MANIFEST + 1))
                .expect("parses");
        let mut set = RuleSet::default();
        assert!(
            set.install(spec)
                .expect_err("over cap")
                .contains("over the")
        );
        assert!(
            set.is_empty() && set.kind_for_binary("big").is_none(),
            "no partial state"
        );

        let cap = super::MAX_MATCHER_CHARS;
        assert!(install(&contains_rule(&"a".repeat(cap))).is_ok());
        let err = install(&contains_rule(&"a".repeat(cap + 1))).expect_err("too long");
        assert!(err.contains("over the"), "{err}");
        // Counted in characters, so multi-byte agent chrome is not penalized.
        assert!(install(&contains_rule(&"\u{2500}".repeat(cap))).is_ok());
    }

    /// The schema bounds nesting itself (depth 8 loads, 9 does not), and a
    /// pathological document is refused by the TOML parser, never
    /// overflowing the stack.
    #[test]
    fn deep_nesting_is_bounded_and_never_overflows_the_stack() {
        let nested = |depth: usize| {
            one_rule(&format!(
                "state = \"idle\"\nregion = \"title\"\nmatch = {}{{ contains = \"x\" }}{}",
                "{ not = ".repeat(depth),
                " }".repeat(depth),
            ))
        };
        assert!(install(&nested(super::MAX_PREDICATE_DEPTH - 1)).is_ok());
        let err = install(&nested(super::MAX_PREDICATE_DEPTH)).expect_err("too deep");
        assert!(err.contains("nests deeper than"), "{err}");
        assert!(toml::from_str::<ManifestSpec>(&nested(20_000)).is_err());
    }

    /// The matcher budget is per rule and also cumulative across the
    /// manifest (otherwise the manifest cap would be unreachable).
    #[test]
    fn the_matcher_budget_is_per_rule_and_cumulative() {
        let rule = |id: usize, matchers: usize| {
            let children: Vec<String> = (0..matchers)
                .map(|idx| format!("{{ contains = \"x{idx}\" }}"))
                .collect();
            format!(
                "[[rules]]\nid = \"r{id}\"\nstate = \"idle\"\nregion = \"title\"\n\
                 match = {{ any = [{}] }}\n",
                children.join(", ")
            )
        };
        let manifest = |rules: usize, matchers: usize| {
            (0..rules).fold(
                String::from("kind = \"k\"\nbinaries = [\"k\"]\n"),
                |mut m, id| {
                    m.push_str(&rule(id, matchers));
                    m
                },
            )
        };
        let per_rule = super::MAX_MATCHERS_PER_RULE;
        assert!(install(&manifest(2, per_rule)).is_ok());
        let err = install(&manifest(1, per_rule + 1)).expect_err("per rule");
        assert!(err.contains("matchers in one rule"), "{err}");
        let rules_needed = super::MAX_MATCHERS_PER_MANIFEST / per_rule + 1;
        let err = install(&manifest(rules_needed, per_rule)).expect_err("aggregate");
        assert!(err.contains("matchers in the manifest"), "{err}");
    }

    /// Every built-in compiles well inside the bounds and owns each declared
    /// binary alias; otherwise that agent silently disappears in production.
    #[test]
    fn every_builtin_manifest_compiles_and_indexes_its_binaries() {
        let expected = [
            ("claude", &["claude", "claude-code"][..]),
            ("codex", &["codex"][..]),
            ("opencode", &["opencode", "opencode2", "@opencode-ai"][..]),
            ("pi", &["pi"][..]),
            ("omp", &["omp"][..]),
            ("grok", &["grok"][..]),
            ("amp", &["amp"][..]),
            ("cursor-agent", &["cursor-agent"][..]),
        ];
        for (kind, binaries) in expected {
            let spec: ManifestSpec = toml::from_str(builtin(kind)).expect("builtin parses");
            assert!(
                spec.rules.len() <= super::MAX_RULES_PER_MANIFEST / 4,
                "{kind}"
            );
            let set = compile(builtin(kind));
            for binary in binaries {
                assert_eq!(set.kind_for_binary(binary), Some(kind));
            }
            let manifest = set.manifest(kind).expect("manifest");
            assert_eq!(manifest.name, kind);
            assert!(!manifest.rules.is_empty());
        }
        let set = global();
        assert!(std::rc::Rc::ptr_eq(&set, &global()), "memoized");
        assert_eq!(set.kind_for_binary("codex"), Some("codex"));
        assert_eq!(set.kind_for_binary("opencode2"), Some("opencode"));
    }

    // --- Shipped manifests against captured screens ------------------------
    //
    // The fixtures are REAL viewports (`phux snapshot --json`) and titles
    // captured from each CLI. Synthetic screens only test the matcher against
    // itself; re-capture when an agent's TUI changes, never hand-edit.

    /// Claude titles: an animated prefix while busy (braille in 2.1.207,
    /// half circles in 2.1.228), a static U+2733 otherwise. The quiet title
    /// covers idle AND a permission dialog, so it must assert nothing.
    const CLAUDE_BUSY: [&str; 4] = [
        "\u{2802} phux",
        "\u{2810} phux",
        "\u{25D0} phux",
        "\u{25D1} phux",
    ];
    const CLAUDE_QUIET: &str = "\u{2733} phux";

    /// Every OMP title spinner frame: braille (default), dots, pulse, line,
    /// and the static WSL/Windows separator.
    const OMP_WORKING_SEPARATORS: [&str; 26] = [
        "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}",
        "\u{2827}", "\u{2807}", "\u{280f}", "\u{2801}", "\u{2802}", "\u{2804}", "\u{2820}",
        "\u{2810}", "\u{2808}", "\u{25cb}", "\u{25d4}", "\u{25d1}", "\u{25d5}", "\u{25cf}", "-",
        "\\", "|", "/", ":",
    ];

    /// `kind | title | progress | screen | want`, one golden case per line.
    /// `screen` is a fixture path or an `@name` from [`named_screen`]; `want`
    /// is `<state> <rule>`, `nothing` (the fail-safe decides), `not-blocked`,
    /// or `freeze`. A false `blocked` destroys trust in the feature, so
    /// transcript-quoted dialogs are pinned `not-blocked`.
    const GOLDEN: &str = "
claude |  | 4;3; | claude/idle_prompt.txt | working osc-progress-working
claude |  | 4;0; | claude/idle_prompt.txt | idle osc-progress-idle
claude |  | 4;3; | claude/blocked_permission.txt | blocked prompt-permission-dialog
claude | \u{2733} phux |  | claude/blocked_permission.txt | blocked prompt-permission-dialog
claude |  |  | claude/blocked_permission.txt | blocked prompt-permission-dialog
claude | \u{2733} phux |  | claude/idle_prompt.txt | nothing
claude | \u{2802} phux |  | claude/working.txt | working title-busy-spinner
claude |  |  | claude/working.txt | working screen-status-elapsed-backstop
claude | \u{2733} phux |  | @claude-transcript-dialog | not-blocked
claude | \u{2733} phux |  | @bare-dialog | not-blocked
claude | \u{2733} phux |  | @claude-pager | freeze
codex | tmp |  | codex/blocked_approval.txt | blocked prompt-command-approval
codex | tmp |  | codex/working.txt | working screen-working-footer-backstop
codex | tmp |  | codex/idle_prompt.txt | nothing
codex | tmp |  | @codex-prose | not-blocked
opencode | OC | whatever |  | opencode/working.txt | working footer-interrupt-affordance
opencode | OC | whatever |  | opencode/blocked_permission.txt | blocked permission-required-dialog
opencode | OpenCode |  | opencode/idle_prompt.txt | nothing
opencode | OC | esc interrupt |  | opencode/idle_prompt.txt | nothing
grok |  |  | grok/working.txt | working screen-status-elapsed-backstop
grok | grok |  | grok/blocked_trust.txt | blocked folder-trust-dialog
grok | grok |  | grok/idle_prompt.txt | nothing
grok | grok |  | @grok-quoted-dialog | not-blocked
amp | \u{280a} Terminal haiku - amp - /tmp/ws |  | @amp-idle | working title-busy-spinner
amp |  |  | @amp-working | working prompt-box-activity-footer
amp | Terminal haiku - amp - /tmp/ws |  | @amp-idle | nothing
cursor-agent |  |  | @cursor-login | nothing
pi |  |  | pi/idle_prompt.txt | nothing
pi |  |  | pi/working.txt | working bottom-working-status
pi |  |  | pi/blocked_trust.txt | blocked project-trust-dialog
pi |  |  | @pi-dialog-above-idle | not-blocked
omp |  |  | omp/idle_prompt.txt | nothing
omp |  |  | omp/working.txt | working bottom-running-status
omp |  |  | omp/blocked_tool_approval.txt | blocked tool-approval-dialog
omp |  |  | @omp-dialog-above-idle | not-blocked
omp | \u{3c0} > work |  | omp/v18_idle_new_session.txt | nothing
omp | \u{3c0} > Run sleep and echo finished |  | omp/v18_idle_prompt.txt | nothing
omp | \u{3c0} > Run sleep and echo finished |  | omp/v18_idle_interrupted.txt | nothing
omp | \u{3c0} \u{2807} work |  | omp/v18_working_thinking.txt | working title-working-spinner
omp | \u{3c0} \u{2839} Run sleep then echo finished |  | omp/v18_working.txt | working title-working-spinner
omp | \u{3c0} \u{280f} Run sleep and echo command |  | omp/v18_working_queued.txt | working title-working-spinner
omp | \u{3c0} ! Run sleep and echo finished |  | omp/v18_blocked_tool_approval.txt | blocked title-attention
omp | \u{3c0} ! Run sleep and echo finished |  | omp/v18_blocked_ask.txt | blocked title-attention
omp | \u{3c0} > Run sleep then echo finished |  | omp/v18_field_idle_prompt.txt | nothing
omp | \u{3c0} \u{2819} work |  | omp/v18_field_working_thinking.txt | working title-working-spinner
omp | \u{3c0} \u{2826} Run sleep and echo finished |  | omp/v18_field_working.txt | working title-working-spinner
omp | \u{3c0} ! Run sleep then echo finished |  | omp/v18_field_blocked_tool_approval.txt | blocked title-attention
omp |  |  | omp/v18_idle_new_session.txt | nothing
omp |  |  | omp/v18_idle_prompt.txt | nothing
omp |  |  | omp/v18_idle_interrupted.txt | nothing
omp |  |  | omp/v18_field_idle_prompt.txt | nothing
omp |  |  | omp/v18_working_thinking.txt | working status-spinner-elapsed
omp |  |  | omp/v18_working.txt | working status-spinner-elapsed
omp |  |  | omp/v18_working_queued.txt | working status-spinner-elapsed
omp |  |  | omp/v18_field_working_thinking.txt | working status-spinner-elapsed
omp | \u{3c0}: Run sleep and echo finished |  | omp/v18_field_working.txt | working status-spinner-elapsed
omp |  |  | omp/v18_blocked_tool_approval.txt | blocked tool-approval-dialog
omp |  |  | omp/v18_field_blocked_tool_approval.txt | blocked tool-approval-dialog
omp |  |  | omp/v18_blocked_ask.txt | nothing
omp | \u{3c0} > Run sleep and echo finished |  | @omp18-dialog-above-idle | not-blocked
omp |  |  | @omp18-dialog-above-idle | not-blocked
";

    /// Synthetic screens built from, or around, the captures.
    fn named_screen(name: &str) -> Vec<String> {
        match name {
            "claude-transcript-dialog" => [
                lines(&[
                    "  Here is what that prompt looks like:",
                    "  > Do you want to proceed?",
                    "  > \u{276f} 1. Yes",
                    "  > 2. No",
                ]),
                golden("claude/idle_prompt.txt"),
            ]
            .concat(),
            "bare-dialog" => lines(&["  Do you want to proceed?", "  \u{276f} 1. Yes", "  2. No"]),
            "claude-pager" => lines(&[
                "  Do you want to proceed?",
                "  1. Yes",
                "  Showing detailed transcript \u{00b7} ctrl+o to toggle \u{00b7} \u{2191}\u{2193} scroll",
            ]),
            "codex-prose" => lines(&[
                "  I was going to ask: would you like to run the following command?",
                "  ...but I decided against it.",
            ]),
            "grok-quoted-dialog" => {
                let mut screen = golden("grok/idle_prompt.txt");
                screen.splice(
                    4..4,
                    lines(&[
                        "Do you trust the contents of this directory?",
                        "Yes, proceed                 y",
                        "No, quit                     n",
                    ]),
                );
                screen
            }
            "amp-idle" => lines(&["╰──────────────── /tmp/ws ─╯"]),
            "amp-working" => lines(&["╰ ≈ Waiting ───── /tmp/ws ─╯"]),
            "cursor-login" => lines(&["Press any key to log in", "Signing in with the browser"]),
            "pi-dialog-above-idle" => {
                [golden("pi/blocked_trust.txt"), golden("pi/idle_prompt.txt")].concat()
            }
            "omp-dialog-above-idle" => [
                golden("omp/blocked_tool_approval.txt"),
                golden("omp/idle_prompt.txt"),
            ]
            .concat(),
            "omp18-dialog-above-idle" => [
                golden("omp/v18_blocked_tool_approval.txt"),
                golden("omp/v18_idle_prompt.txt"),
            ]
            .concat(),
            other => panic!("unknown screen @{other}"),
        }
    }

    /// Every golden case as `(kind, title, progress, screen, want)`, plus
    /// the title sweeps: every Claude busy frame, the whole Codex braille
    /// block, and the captured Grok title lists.
    fn golden_cases() -> Vec<(String, String, String, Vec<String>, String)> {
        let mut cases: Vec<_> = GOLDEN
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let fields: Vec<&str> = line.split(" | ").map(str::trim).collect();
                let [kind, title @ .., progress, screen, want] = fields.as_slice() else {
                    panic!("bad golden row: {line}");
                };
                let screen = screen
                    .strip_prefix('@')
                    .map_or_else(|| golden(screen), named_screen);
                (
                    (*kind).to_owned(),
                    title.join(" | "),
                    (*progress).to_owned(),
                    screen,
                    (*want).to_owned(),
                )
            })
            .collect();
        let sweep = |kind: &str, fixture: &str, titles: Vec<String>, want: &str| {
            titles
                .into_iter()
                .map(|title| {
                    (
                        kind.to_owned(),
                        title,
                        String::new(),
                        golden(fixture),
                        want.to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let busy = "working title-busy-spinner";
        cases.extend(sweep(
            "claude",
            "claude/idle_prompt.txt",
            CLAUDE_BUSY.map(str::to_owned).to_vec(),
            busy,
        ));
        cases.extend(sweep(
            "codex",
            "codex/idle_prompt.txt",
            [
                '\u{2800}', '\u{280b}', '\u{280f}', '\u{2834}', '\u{283c}', '\u{28ff}',
            ]
            .map(|cp| format!("{cp} tmp"))
            .to_vec(),
            busy,
        ));
        // OMP's title contract (17.1.2 and 18.6.1 source): `π <sep> <label>`,
        // with every `tui.titleSpinner` frame and the static `:` as working.
        cases.extend(sweep(
            "omp",
            "omp/v18_idle_prompt.txt",
            OMP_WORKING_SEPARATORS
                .iter()
                .flat_map(|sep| [format!("\u{3c0} {sep} label"), format!("\u{3c0} {sep}")])
                .collect(),
            "working title-working-spinner",
        ));
        cases.extend(sweep(
            "omp",
            "omp/v18_idle_prompt.txt",
            [
                "\u{3c0} > label",
                "\u{3c0} >",
                "\u{3c0}: label",
                "\u{3c0}",
                "custom title",
            ]
            .map(str::to_owned)
            .to_vec(),
            "nothing",
        ));
        cases.extend(sweep(
            "omp",
            "omp/v18_idle_prompt.txt",
            ["\u{3c0} ! label", "\u{3c0} !"].map(str::to_owned).to_vec(),
            "blocked title-attention",
        ));
        let grok_titles = |text: &str| text.lines().map(str::to_owned).collect();
        cases.extend(sweep(
            "grok",
            "grok/idle_prompt.txt",
            grok_titles(include_str!("fixtures/grok/titles_working.txt")),
            busy,
        ));
        cases.extend(sweep(
            "grok",
            "grok/idle_prompt.txt",
            grok_titles(include_str!("fixtures/grok/titles_idle.txt")),
            "nothing",
        ));
        cases
    }

    fn state_word(state: Option<DetectedState>) -> &'static str {
        state.map_or("", DetectedState::as_str)
    }

    /// Each shipped manifest reads its captured screens as exactly their
    /// live state.
    #[test]
    fn shipped_manifests_read_captured_screens_correctly() {
        let mut sets = std::collections::HashMap::new();
        for (kind, title, progress, screen, want) in golden_cases() {
            let set = sets
                .entry(kind.clone())
                .or_insert_with(|| compile(builtin(&kind)));
            let got = eval(set, &kind, &title, &progress, &screen);
            let ok = match want.as_str() {
                "nothing" => got.state.is_none() && !got.freeze,
                "not-blocked" => got.state != Some(Blocked),
                "freeze" => got.freeze,
                expected => {
                    let actual = format!(
                        "{} {}",
                        state_word(got.state),
                        got.matched.as_deref().unwrap_or("")
                    );
                    actual == expected
                }
            };
            assert!(
                ok,
                "{kind} title={title:?} progress={progress:?}: want {want}, got {got:?}"
            );
        }
    }

    /// Claude's busy-title rule answers to REAL captured OSC 0 bytes, not
    /// this module's constants: the 2.1.228 glyph change went unnoticed
    /// while tests and rule agreed with each other. The capture's titles must
    /// partition into a quiet one (asserts nothing) and busy frames
    /// (`working`).
    #[test]
    fn every_busy_title_in_the_committed_capture_reads_as_working() {
        let capture = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR")
                .expect("the test runner sets CARGO_MANIFEST_DIR"),
        )
        .join("../../research/2026-08-12-osc-9-4-claude-code/claude-title-enabled.rawcap");
        let raw = std::fs::read_to_string(&capture).unwrap_or_else(|e| {
            panic!(
                "{}: {e}. If this capture is removed, re-verify `title-busy-spinner` against a \
                 fresh one rather than deleting this test.",
                capture.display(),
            )
        });
        let mut titles: Vec<&str> = raw
            .split("\u{1b}]0;")
            .skip(1)
            .filter_map(|rest| rest.split('\u{7}').next())
            .filter(|t| !t.is_empty())
            .collect();
        titles.sort_unstable();
        titles.dedup();

        let set = compile(builtin("claude"));
        let idle = golden("claude/idle_prompt.txt");
        let (mut busy, mut quiet) = (0, 0);
        for title in titles {
            let got = eval(&set, "claude", title, "", &idle);
            if title.starts_with('\u{2733}') {
                quiet += 1;
                assert_eq!(got.state, None, "{title:?}");
            } else {
                busy += 1;
                assert_eq!(
                    got.matched.as_deref(),
                    Some("title-busy-spinner"),
                    "{title:?}"
                );
            }
        }
        assert!(busy >= 2 && quiet >= 1, "busy={busy} quiet={quiet}");
    }

    /// The non-short-circuiting trace walker must agree with the production
    /// matcher on every rule of every golden, and must evaluate every child
    /// of a combinator so an author can see which conjunct failed.
    #[test]
    fn the_trace_agrees_with_the_production_evaluator() {
        let mut sets = std::collections::HashMap::new();
        for (kind, _, _, screen, _) in golden_cases() {
            let set = sets
                .entry(kind.clone())
                .or_insert_with(|| compile(builtin(&kind)));
            let manifest = set.manifest(&kind).expect("manifest");
            for title in ["", CLAUDE_BUSY[0], CLAUDE_QUIET, "\u{280b} tmp"] {
                let screen = Screen {
                    title,
                    progress: "",
                    lines: &screen,
                };
                let explained = manifest.explain(&screen);
                assert_eq!(manifest.evaluate(&screen), explained.evaluation, "{kind}");
                assert_eq!(explained.rules.len(), manifest.rules.len(), "{kind}");
                for trace in &explained.rules {
                    assert_eq!(trace.matched, trace.evidence.matched, "{kind}/{}", trace.id);
                }
            }
        }

        let set = compile(SAMPLE);
        let buf = lines(&["nothing here at all"]);
        let explained = set.manifest("sample").expect("manifest").explain(&Screen {
            title: "",
            progress: "",
            lines: &buf,
        });
        let rule = explained
            .rules
            .iter()
            .find(|r| r.id == "screen-blocked")
            .expect("rule reported");
        assert!(!rule.matched);
        assert_eq!(rule.evidence.op, "all");
        assert_eq!(rule.evidence.children.len(), 2);
        assert!(
            rule.evidence
                .children
                .iter()
                .all(|c| !c.matched && c.pattern.is_some())
        );
    }
}
