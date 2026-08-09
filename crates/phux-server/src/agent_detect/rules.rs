//! Declarative, region-scoped detection rules (ADR-0046 §C).
//!
//! Rules are **data, not code**: an ordered list of
//! `{id, state, priority, region, match, flags}` records shipped as TOML
//! manifests, one per agent kind. Agent TUIs churn on their own cadence;
//! keeping the rules in a manifest — a built-in compiled into the binary,
//! overridable from a config directory — decouples that churn from phux's
//! release cadence and lets an operator repair a broken detection without
//! waiting for us.
//!
//! Predicates form a recursive combinator tree (`contains` / `regex` /
//! `line-regex` / `all` / `any` / `not`), compiled **once at load** into
//! [`Predicate`]. A manifest carrying an invalid regex or an unknown state
//! word is logged at `warn` and **dropped whole** — a bad manifest must
//! never wedge a pane, and a half-applied one is worse than none.

use std::collections::HashMap;
use std::rc::Rc;

use regex::Regex;
use serde::Deserialize;
use tracing::{debug, warn};

use super::DetectedState;
use super::regions::{Region, Screen, extract};

/// Built-in manifests. Every predicate in these files is derived from the
/// shipped CLI's observable output and pinned by captured-screen tests below.
const BUILTIN_MANIFESTS: [(&str, &str); 5] = [
    ("claude", include_str!("../../rules/claude.toml")),
    ("codex", include_str!("../../rules/codex.toml")),
    ("opencode", include_str!("../../rules/opencode.toml")),
    ("pi", include_str!("../../rules/pi.toml")),
    ("omp", include_str!("../../rules/omp.toml")),
];

/// Env knob: `PHUX_AGENT_DETECT=0` disables the detector wholesale by
/// yielding an empty rule set (the actor then never constructs a detector).
const ENV_DETECT: &str = "PHUX_AGENT_DETECT";

/// Env knob: directory of `*.toml` manifests that override / extend the
/// built-ins. Defaults to `$XDG_CONFIG_HOME/phux/agent-rules`.
const ENV_RULES_DIR: &str = "PHUX_AGENT_RULES_DIR";

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
    All(Vec<PredicateSpec>),
    /// At least one child must match.
    Any(Vec<PredicateSpec>),
    /// The child must not match.
    Not(Box<PredicateSpec>),
}

/// One rule, as written in TOML.
///
/// `rename_all` is load-bearing: the manifest spells these flags
/// `visible-idle`, `skip-state-update`, and so on. `deny_unknown_fields` is
/// load-bearing too — without it a typo'd or mis-cased flag is *silently
/// ignored*, which is the worst possible failure for this struct: a
/// `skip-state-update` that never freezes, or a `visible-idle` that never
/// bypasses the hold, with nothing anywhere to say so. Now it drops the
/// manifest with a `warn`.
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
    /// The screen POSITIVELY shows the agent is blocked.
    #[serde(default)]
    pub(crate) visible_blocker: bool,
    /// The screen POSITIVELY shows the agent is idle. The only flag that
    /// changes control flow in v1: it bypasses the working -> idle hold.
    #[serde(default)]
    pub(crate) visible_idle: bool,
    /// The screen POSITIVELY shows the agent is working.
    #[serde(default)]
    pub(crate) visible_working: bool,
    /// The screen is a transcript viewer / model picker / pager and
    /// therefore carries NO information about agent state. Freeze the last
    /// derivation; do not guess.
    #[serde(default)]
    pub(crate) skip_state_update: bool,
}

/// One agent kind's manifest, as written in TOML.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct ManifestSpec {
    /// Open-vocabulary kind slug, e.g. `"claude"`. Also the override key.
    pub(crate) kind: String,
    /// Human-facing name for the record's `name` field; defaults to `kind`.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// argv basenames (and program-path components) that identify this
    /// agent. See [`super::identify`].
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
    All(Vec<Predicate>),
    /// Disjunction.
    Any(Vec<Predicate>),
    /// Negation.
    Not(Box<Predicate>),
}

impl Predicate {
    /// Compile a spec, surfacing the offending pattern on a bad regex.
    fn compile(spec: &PredicateSpec) -> Result<Self, String> {
        Ok(match spec {
            PredicateSpec::Contains(needle) => Self::Contains(needle.to_lowercase()),
            PredicateSpec::Regex(pat) => {
                Self::Regex(Regex::new(pat).map_err(|e| format!("regex `{pat}`: {e}"))?)
            }
            PredicateSpec::LineRegex(pat) => {
                Self::LineRegex(Regex::new(pat).map_err(|e| format!("line-regex `{pat}`: {e}"))?)
            }
            PredicateSpec::All(children) => Self::All(
                children
                    .iter()
                    .map(Self::compile)
                    .collect::<Result<_, _>>()?,
            ),
            PredicateSpec::Any(children) => Self::Any(
                children
                    .iter()
                    .map(Self::compile)
                    .collect::<Result<_, _>>()?,
            ),
            PredicateSpec::Not(child) => Self::Not(Box::new(Self::compile(child)?)),
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

    /// The pattern a leaf node carries, for evidence output. `None` on a
    /// combinator, whose evidence is its children.
    ///
    /// `Contains` returns the pre-lowercased needle rather than the manifest's
    /// original casing: that is the string the matcher actually compares, and
    /// the point of the evidence is to show what ran, not what was typed.
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

    /// Evaluate and record every node, for the offline explainer.
    ///
    /// Deliberately does NOT short-circuit: `eval` stops an `all` at the
    /// first false child, but the author debugging a manifest needs to know
    /// which of the three conjuncts failed, not merely that one did. The
    /// combinator results are computed from the fully-evaluated children, so
    /// the root's `matched` equals `eval`'s answer — pinned by
    /// `the_trace_agrees_with_the_production_evaluator`.
    fn trace(&self, text: &RegionText<'_>) -> PredicateTrace {
        let children: Vec<PredicateTrace> = match self {
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
        PredicateTrace {
            op: self.op(),
            pattern: self.pattern(),
            matched,
            children,
        }
    }
}

/// One node of a predicate tree, with the result it produced on one screen.
///
/// Built only by [`Predicate::trace`], which the detector never calls: the
/// production path is [`Predicate::eval`], and this exists so `phux agent
/// explain` can say *which* leaf fired rather than only whether the rule did.
#[derive(Debug, Clone)]
pub(crate) struct PredicateTrace {
    /// The manifest keyword (`contains`, `all`, ...).
    pub(crate) op: &'static str,
    /// The leaf's pattern, as compiled. `None` on a combinator.
    pub(crate) pattern: Option<String>,
    /// Whether this node matched.
    pub(crate) matched: bool,
    /// Child nodes, for a combinator.
    pub(crate) children: Vec<PredicateTrace>,
}

/// One rule's outcome on one screen, with its evidence.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "reports RuleSpec's independent flags verbatim; see that struct"
)]
pub(crate) struct RuleTrace {
    /// The rule's manifest id.
    pub(crate) id: String,
    /// The state it asserts, if any.
    pub(crate) state: Option<DetectedState>,
    /// Its priority.
    pub(crate) priority: i32,
    /// The region it read.
    pub(crate) region: Region,
    /// Whether its predicate matched.
    pub(crate) matched: bool,
    /// See [`RuleSpec::visible_blocker`].
    pub(crate) visible_blocker: bool,
    /// See [`RuleSpec::visible_idle`].
    pub(crate) visible_idle: bool,
    /// See [`RuleSpec::visible_working`].
    pub(crate) visible_working: bool,
    /// See [`RuleSpec::skip_state_update`].
    pub(crate) skip_state_update: bool,
    /// The predicate tree, annotated with what each node saw.
    pub(crate) predicate: PredicateTrace,
}

/// A whole-manifest evaluation with its working shown.
#[derive(Debug)]
pub(crate) struct Explanation {
    /// Exactly what [`CompiledManifest::evaluate`] returns for this screen.
    /// Produced by the same code path, not a reimplementation.
    pub(crate) evaluation: Evaluation,
    /// Every rule, in declaration order, matched or not.
    pub(crate) rules: Vec<RuleTrace>,
    /// The text every region resolved to on this screen, in
    /// [`Region::ALL`] order — including the regions no rule names, because
    /// "the region I scoped my rule to is empty" is the failure this is for.
    pub(crate) regions: Vec<(Region, Vec<String>)>,
}

/// A region's text, materialized once per tick and shared by every rule
/// that names that region.
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
    /// See [`RuleSpec::visible_blocker`].
    pub(crate) visible_blocker: bool,
    /// See [`RuleSpec::visible_idle`].
    pub(crate) visible_idle: bool,
    /// See [`RuleSpec::visible_working`].
    pub(crate) visible_working: bool,
    /// See [`RuleSpec::skip_state_update`].
    pub(crate) skip_state_update: bool,
}

/// A compiled manifest: one agent kind's identity plus its rules.
///
/// The `kind` slug is not repeated here — it is the key this manifest is
/// stored under in [`RuleSet`], and the detector already carries it as the
/// identity it resolved.
#[derive(Debug)]
pub(crate) struct CompiledManifest {
    /// Human-facing name written into the `phux.agent/v1` record.
    pub(crate) name: String,
    /// Rules in declaration order.
    pub(crate) rules: Vec<Rule>,
}

/// What a full rule-set evaluation concluded about one screen.
#[derive(Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the union of the matching rules' independent flags; see RuleSpec"
)]
pub(crate) struct Evaluation {
    /// The winning state, or `None` when no state-bearing rule matched
    /// (the caller's fail-safe turns that into `idle`, never `blocked`).
    pub(crate) state: Option<DetectedState>,
    /// A matching rule asserts the screen positively shows a blocker.
    pub(crate) visible_blocker: bool,
    /// A matching rule asserts the screen positively shows idleness.
    pub(crate) visible_idle: bool,
    /// A matching rule asserts the screen positively shows work.
    pub(crate) visible_working: bool,
    /// A matching rule says this screen carries no state information at
    /// all. The caller MUST freeze rather than derive.
    pub(crate) freeze: bool,
    /// The winning rule's id, for `trace` logs.
    pub(crate) matched: Option<String>,
}

impl CompiledManifest {
    /// Evaluate every rule against `screen`.
    ///
    /// Ordering: **title-derived rules outrank screen-derived rules**, then
    /// `priority` descending, then declaration order. The title is the
    /// cheapest and most direct signal an agent CLI publishes about itself;
    /// a screen rule is always an inference about pixels it happened to
    /// paint.
    pub(crate) fn evaluate(&self, screen: &Screen<'_>) -> Evaluation {
        self.run(screen, None)
    }

    /// [`Self::evaluate`], with the working shown: every rule's outcome, the
    /// evidence behind it, and the text every region resolved to.
    ///
    /// The verdict is produced by the SAME pass as `evaluate` — `run` takes
    /// the trace sink as an out-parameter rather than being reimplemented —
    /// so an explanation can never disagree with what the detector would do.
    /// A second, parallel matcher is exactly how a debugger starts lying.
    pub(crate) fn explain(&self, screen: &Screen<'_>) -> Explanation {
        let mut rules = Vec::with_capacity(self.rules.len());
        let evaluation = self.run(screen, Some(&mut rules));
        // Every region, not merely the referenced ones: an author picking a
        // region for a NEW rule needs to see which one holds their text, and
        // an author debugging an old one needs to see that theirs is empty.
        let regions = Region::ALL
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

    /// The one evaluation pass. `trace`, when supplied, collects a
    /// [`RuleTrace`] per rule — including the rules that did not match, which
    /// the production path discards.
    fn run(&self, screen: &Screen<'_>, mut trace: Option<&mut Vec<RuleTrace>>) -> Evaluation {
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
                sink.push(RuleTrace {
                    id: rule.id.clone(),
                    state: rule.state,
                    priority: rule.priority,
                    region: rule.region,
                    matched,
                    visible_blocker: rule.visible_blocker,
                    visible_idle: rule.visible_idle,
                    visible_working: rule.visible_working,
                    skip_state_update: rule.skip_state_update,
                    predicate: rule.predicate.trace(text),
                });
            }
            if !matched {
                continue;
            }
            out.visible_blocker |= rule.visible_blocker;
            out.visible_idle |= rule.visible_idle;
            out.visible_working |= rule.visible_working;
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
pub(crate) struct RuleSet {
    manifests: HashMap<String, CompiledManifest>,
    /// binary name (or program-path component) -> kind.
    by_binary: HashMap<String, String>,
}

impl RuleSet {
    /// `true` when nothing is loaded — the actor then never builds a
    /// detector, so the whole feature costs exactly zero.
    pub(crate) fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    /// The agent kind a program named `name` belongs to, if any. `name` is
    /// matched case-insensitively.
    pub(crate) fn kind_for_binary(&self, name: &str) -> Option<&str> {
        self.by_binary.get(&name.to_lowercase()).map(String::as_str)
    }

    /// The compiled manifest for `kind`.
    pub(crate) fn manifest(&self, kind: &str) -> Option<&CompiledManifest> {
        self.manifests.get(kind)
    }

    /// Every loaded kind slug, sorted. The roster `phux agent explain` names
    /// when it is handed a kind it does not have a manifest for — an operator
    /// whose override failed to compile sees its absence here rather than
    /// guessing at a silent `warn` in the server log.
    pub(crate) fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self.manifests.keys().cloned().collect();
        kinds.sort_unstable();
        kinds
    }

    /// Compile `spec` and install it, replacing any manifest of the same
    /// `kind`. Returns `Err` with a human-readable reason when the manifest
    /// is unusable; the caller drops it whole.
    pub(crate) fn install(&mut self, spec: ManifestSpec) -> Result<(), String> {
        if spec.kind.is_empty() {
            return Err("manifest has an empty `kind`".to_owned());
        }
        let mut rules = Vec::with_capacity(spec.rules.len());
        for rule in &spec.rules {
            let state = match rule.state.as_deref() {
                None => None,
                Some(word) => Some(
                    parse_state(word)
                        .ok_or_else(|| format!("rule `{}`: unknown state `{word}`", rule.id))?,
                ),
            };
            let predicate = Predicate::compile(&rule.predicate)
                .map_err(|e| format!("rule `{}`: {e}", rule.id))?;
            rules.push(Rule {
                id: rule.id.clone(),
                state,
                priority: rule.priority,
                region: rule.region,
                predicate,
                visible_blocker: rule.visible_blocker,
                visible_idle: rule.visible_idle,
                visible_working: rule.visible_working,
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
    // Sort for determinism: two overrides of the same kind must resolve the
    // same way on every boot.
    let mut paths: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    for path in paths {
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
    /// Compiled once per runtime thread, on first use. The server is a
    /// current-thread runtime (ADR-0003) with every actor on one
    /// `LocalSet`, so this is effectively process-wide, and an `Rc` clone
    /// per pane costs one refcount bump.
    static RULES: std::cell::OnceCell<Rc<RuleSet>> = const { std::cell::OnceCell::new() };
}

/// The shared, compiled rule set.
pub(crate) fn global() -> Rc<RuleSet> {
    RULES.with(|cell| Rc::clone(cell.get_or_init(|| Rc::new(build()))))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{ManifestSpec, RuleSet, global};
    use crate::agent_detect::DetectedState;
    use crate::agent_detect::regions::Screen;

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

    const SAMPLE: &str = r#"
kind = "sample"
name = "Sample"
binaries = ["sample", "sample-cli"]

[[rules]]
id = "title-working"
state = "working"
priority = 10
region = "title"
visible-working = true
match = { line-regex = "^W " }

[[rules]]
id = "screen-blocked"
state = "blocked"
priority = 90
region = "bottom-lines"
visible-blocker = true
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

    #[test]
    fn binary_index_is_case_insensitive_and_covers_every_alias() {
        let set = compile(SAMPLE);
        assert_eq!(set.kind_for_binary("sample"), Some("sample"));
        assert_eq!(set.kind_for_binary("SAMPLE-CLI"), Some("sample"));
        assert_eq!(set.kind_for_binary("nope"), None);
    }

    #[test]
    fn title_rule_outranks_a_higher_priority_screen_rule() {
        // The screen rule has priority 90 vs the title rule's 10, yet the
        // title wins: it is the agent's own statement about itself.
        let set = compile(SAMPLE);
        let manifest = set.manifest("sample").expect("manifest");
        let buf = lines(&["do you want to proceed?", " 1. Yes"]);
        let got = manifest.evaluate(&Screen {
            title: "W busy",
            lines: &buf,
        });
        assert_eq!(got.state, Some(DetectedState::Working));
        assert_eq!(got.matched.as_deref(), Some("title-working"));
        // Flags from EVERY matching rule are still collected.
        assert!(got.visible_working);
        assert!(got.visible_blocker);
    }

    #[test]
    fn priority_orders_rules_within_the_screen_class() {
        let set = compile(SAMPLE);
        let manifest = set.manifest("sample").expect("manifest");
        let buf = lines(&["ready", "do you want to proceed?", " 1. Yes"]);
        let got = manifest.evaluate(&Screen {
            title: "idle",
            lines: &buf,
        });
        assert_eq!(got.state, Some(DetectedState::Blocked), "90 beats 40");
        assert!(got.visible_idle, "the idle rule still matched, and says so");
    }

    #[test]
    fn all_combinator_needs_both_children() {
        let set = compile(SAMPLE);
        let manifest = set.manifest("sample").expect("manifest");
        // The question alone, with no numbered option line, is NOT blocked.
        let buf = lines(&["do you want to proceed?"]);
        let got = manifest.evaluate(&Screen {
            title: "",
            lines: &buf,
        });
        assert_eq!(got.state, None);
        assert!(!got.visible_blocker);
    }

    #[test]
    fn no_match_yields_no_state_so_the_caller_can_fail_safe() {
        let set = compile(SAMPLE);
        let manifest = set.manifest("sample").expect("manifest");
        let buf = lines(&["nothing interesting here"]);
        let got = manifest.evaluate(&Screen {
            title: "",
            lines: &buf,
        });
        assert_eq!(got.state, None);
        assert!(!got.freeze);
    }

    #[test]
    fn skip_state_update_is_reported_even_when_other_rules_match() {
        let set = compile(SAMPLE);
        let manifest = set.manifest("sample").expect("manifest");
        let buf = lines(&["do you want to proceed?", " 1. Yes", "-- pager --"]);
        let got = manifest.evaluate(&Screen {
            title: "",
            lines: &buf,
        });
        assert!(
            got.freeze,
            "a pager screen carries no agent-state information"
        );
    }

    #[test]
    fn a_bad_regex_drops_the_manifest_whole() {
        let spec: ManifestSpec = toml::from_str(
            r#"
kind = "broken"
binaries = ["broken"]
[[rules]]
id = "bad"
state = "idle"
region = "title"
match = { regex = "(unclosed" }
"#,
        )
        .expect("parses as TOML");
        let mut set = RuleSet::default();
        assert!(set.install(spec).is_err());
        assert!(set.is_empty(), "nothing partially applied");
    }

    #[test]
    fn an_unknown_state_word_drops_the_manifest_whole() {
        let spec: ManifestSpec = toml::from_str(
            r#"
kind = "broken"
binaries = ["broken"]
[[rules]]
id = "bad"
state = "confused"
region = "title"
match = { contains = "x" }
"#,
        )
        .expect("parses as TOML");
        let mut set = RuleSet::default();
        assert!(set.install(spec).is_err());
        assert!(set.is_empty());
    }

    /// REGRESSION. The manifest spells its flags in kebab-case. Without
    /// `rename_all` on `RuleSpec` they parse as unknown fields and are
    /// silently dropped — a `skip-state-update` that never freezes and a
    /// `visible-idle` that never bypasses the hold, with no error anywhere.
    /// `deny_unknown_fields` now turns that class of typo into a loud drop.
    #[test]
    fn kebab_case_flags_actually_bind() {
        let set = compile(
            r#"
kind = "k"
binaries = ["k"]
[[rules]]
id = "r"
state = "idle"
region = "title"
visible-idle = true
visible-blocker = true
visible-working = true
skip-state-update = true
match = { contains = "x" }
"#,
        );
        let rule = &set.manifest("k").expect("manifest").rules[0];
        assert!(rule.visible_idle, "visible-idle must bind");
        assert!(rule.visible_blocker, "visible-blocker must bind");
        assert!(rule.visible_working, "visible-working must bind");
        assert!(rule.skip_state_update, "skip-state-update must bind");
    }

    #[test]
    fn an_unknown_field_drops_the_manifest_rather_than_being_ignored() {
        let parsed: Result<ManifestSpec, _> = toml::from_str(
            r#"
kind = "k"
binaries = ["k"]
[[rules]]
id = "r"
state = "idle"
region = "title"
visible_idle = true      # snake_case: NOT the manifest spelling
match = { contains = "x" }
"#,
        );
        assert!(parsed.is_err(), "a mis-spelled flag must not pass silently");
    }

    #[test]
    fn not_combinator_negates() {
        let set = compile(
            r#"
kind = "n"
binaries = ["n"]
[[rules]]
id = "not-pager"
state = "idle"
region = "viewport"
match = { all = [ { contains = "prompt" }, { not = { contains = "pager" } } ] }
"#,
        );
        let manifest = set.manifest("n").expect("manifest");
        let with = lines(&["prompt", "pager"]);
        let without = lines(&["prompt"]);
        assert_eq!(
            manifest
                .evaluate(&Screen {
                    title: "",
                    lines: &with
                })
                .state,
            None
        );
        assert_eq!(
            manifest
                .evaluate(&Screen {
                    title: "",
                    lines: &without
                })
                .state,
            Some(DetectedState::Idle)
        );
    }

    // --- The shipped Claude Code manifest -----------------------------------
    //
    // These pin `rules/claude.toml` against faithful reproductions of the
    // screens Claude Code actually paints. They are the regression net for the
    // one thing that can silently rot: the CLI changes its chrome and our
    // manifest quietly stops matching (or, far worse, starts matching the
    // wrong thing). Each fixture's provenance is recorded on the manifest rule
    // it exercises.

    // --- Golden screens ------------------------------------------------------
    //
    // These are REAL viewports captured from Claude Code 2.1.207 running in a
    // phux pane (`phux snapshot --json`), not screens we imagined. That
    // distinction is not pedantry: the first draft of this manifest was written
    // against an invented TUI — a box-drawn dialog, a `? for shortcuts` idle
    // hint, an interrupt hint — and every one of its screen rules passed its
    // tests while matching NOTHING in the shipped CLI. Synthetic screens test
    // the matcher against itself. Only a captured screen tests it against
    // reality, so the goldens are the fixture of record. Re-capture them when
    // Claude's TUI changes; do not hand-edit them.

    /// Idle: an empty input box fenced by two horizontal rules, status below.
    fn claude_idle_screen() -> Vec<String> {
        lines(
            include_str!("fixtures/claude/idle_prompt.txt")
                .lines()
                .collect::<Vec<_>>()
                .as_slice(),
        )
    }

    /// Blocked: a live Bash permission dialog. Note it REPLACES the input box
    /// and is the only thing below the final rule.
    fn claude_blocked_screen() -> Vec<String> {
        lines(
            include_str!("fixtures/claude/blocked_permission.txt")
                .lines()
                .collect::<Vec<_>>()
                .as_slice(),
        )
    }

    /// Working: the spinner line sits ABOVE the (empty) input box, so the box
    /// alone cannot tell working from idle. The title is what distinguishes
    /// them, which is why the manifest leans on it.
    fn claude_working_screen() -> Vec<String> {
        lines(
            include_str!("fixtures/claude/working.txt")
                .lines()
                .collect::<Vec<_>>()
                .as_slice(),
        )
    }

    /// The title Claude Code writes while BUSY: an animated braille prefix
    /// (U+2802 / U+2810, alternating ~960 ms) ahead of the title text.
    const CLAUDE_TITLE_BUSY_A: &str = "\u{2802} phux";
    const CLAUDE_TITLE_BUSY_B: &str = "\u{2810} phux";
    /// The title it writes when NOT busy: a static U+2733. Note this covers
    /// idle AND waiting-on-a-dialog alike, which is exactly why the manifest
    /// gives it no rule.
    const CLAUDE_TITLE_QUIET: &str = "\u{2733} phux";

    fn claude_eval(title: &str, screen: &[String]) -> super::Evaluation {
        let set = compile(builtin("claude"));
        let manifest = set.manifest("claude").expect("claude manifest");
        manifest.evaluate(&Screen {
            title,
            lines: screen,
        })
    }

    /// Both animation frames of the busy title read as `working`.
    #[test]
    fn claude_busy_title_is_working() {
        for title in [CLAUDE_TITLE_BUSY_A, CLAUDE_TITLE_BUSY_B] {
            let got = claude_eval(title, &claude_idle_screen());
            assert_eq!(
                got.state,
                Some(DetectedState::Working),
                "the animated title prefix is the primary working signal: {title:?}",
            );
            assert_eq!(got.matched.as_deref(), Some("title-busy-spinner"));
        }
    }

    /// THE most important property of this manifest. The quiet title (U+2733)
    /// covers BOTH idle and waiting-on-a-permission-dialog, so it must assert
    /// nothing. If it ever asserted `idle`, it would outrank (title beats
    /// screen) the prompt-box rule and mask EVERY permission prompt.
    #[test]
    fn claude_quiet_title_asserts_nothing_and_never_masks_a_dialog() {
        let got = claude_eval(CLAUDE_TITLE_QUIET, &claude_blocked_screen());
        assert_eq!(
            got.state,
            Some(DetectedState::Blocked),
            "the quiet title must not outrank a live permission dialog",
        );
        assert_eq!(got.matched.as_deref(), Some("prompt-permission-dialog"));
        assert!(got.visible_blocker);
    }

    /// The captured permission dialog reads as `blocked`.
    ///
    /// This is the test the first draft could not pass. It scoped the rule to
    /// `prompt-box` — the bottom-most *box-drawn* run — but Claude 2.1.207
    /// fences its chrome with horizontal rules and draws no box at all, so the
    /// region came back empty, the rule never matched, and a pane sitting on a
    /// live permission prompt reported `idle` forever.
    #[test]
    fn claude_permission_dialog_is_blocked() {
        let got = claude_eval("", &claude_blocked_screen());
        assert_eq!(got.state, Some(DetectedState::Blocked));
        assert_eq!(got.matched.as_deref(), Some("prompt-permission-dialog"));
        assert!(got.visible_blocker);
    }

    /// The idle screen matches NO state-bearing rule, and that is the design:
    /// `idle` is the detector's fail-safe default (ADR-0046 §D, applied in
    /// `agent_detect::mod`), so it is reached by nothing matching rather than
    /// by a rule asserting it. A rule that asserted `idle` from the quiet title
    /// or the empty box would outrank the dialog rule and mask every prompt.
    #[test]
    fn claude_idle_screen_asserts_no_state_and_leaves_the_fail_safe_to_decide() {
        let got = claude_eval(CLAUDE_TITLE_QUIET, &claude_idle_screen());
        assert_eq!(got.state, None, "no rule should claim the idle screen");
        assert!(!got.visible_blocker);
        assert!(!got.freeze);
    }

    /// The working screen's input box is EMPTY — structurally identical to the
    /// idle one. Only the title separates them, which is the whole reason the
    /// title rule carries the working signal.
    #[test]
    fn claude_working_screen_is_working_only_by_its_title() {
        let by_title = claude_eval(CLAUDE_TITLE_BUSY_A, &claude_working_screen());
        assert_eq!(by_title.state, Some(DetectedState::Working));
        assert_eq!(by_title.matched.as_deref(), Some("title-busy-spinner"));

        let titleless = claude_eval("", &claude_working_screen());
        assert_eq!(
            titleless.state, None,
            "the working screen is indistinguishable from idle without the title",
        );
    }

    /// THE regression the region design exists to prevent. A permission dialog
    /// that Claude merely PRINTED into its transcript — not a live prompt —
    /// must never read as `blocked`. Here the words sit in a quoted transcript
    /// above the real, live, idle chrome.
    #[test]
    fn claude_dialog_text_quoted_in_the_transcript_is_not_blocked() {
        let mut screen = lines(&[
            "  Here is what that prompt looks like:",
            "",
            "  > Do you want to proceed?",
            "  > \u{276f} 1. Yes",
            "  > 2. No",
            "",
        ]);
        // ... and the LIVE chrome below it is the captured idle screen.
        screen.extend(claude_idle_screen());
        let got = claude_eval(CLAUDE_TITLE_QUIET, &screen);
        assert_ne!(
            got.state,
            Some(DetectedState::Blocked),
            "text in the transcript is not a live prompt; a false `blocked` is the one \
             failure that destroys trust in the feature",
        );
        assert!(!got.visible_blocker);
    }

    /// A screen with no rules at all cannot be blocked, however dialog-shaped
    /// its text. `after-last-rule` yields nothing when there is no rule, so the
    /// region is empty and the predicate has nothing to see. Guards the case
    /// where an agent prints a dialog transcript with the live chrome scrolled
    /// off entirely.
    #[test]
    fn claude_dialog_shaped_text_with_no_live_chrome_is_not_blocked() {
        let screen = lines(&["  Do you want to proceed?", "  \u{276f} 1. Yes", "  2. No"]);
        let got = claude_eval(CLAUDE_TITLE_QUIET, &screen);
        assert_ne!(got.state, Some(DetectedState::Blocked));
        assert!(!got.visible_blocker);
    }

    /// The transcript viewer (ctrl+o) is a pager over history: it carries no
    /// information about the agent's live state, so it must freeze rather than
    /// guess. Footer string verified against 2.1.207.
    #[test]
    fn claude_transcript_viewer_freezes() {
        let screen = lines(&[
            "  (scrolled-back history, possibly containing an old dialog)",
            "  Do you want to proceed?",
            "  1. Yes",
            "  Showing detailed transcript \u{00b7} ctrl+o to toggle \u{00b7} \u{2191}\u{2193} scroll",
        ]);
        let got = claude_eval(CLAUDE_TITLE_QUIET, &screen);
        assert!(got.freeze, "a pager carries no agent-state information");
    }

    fn captured(raw: &str) -> Vec<String> {
        raw.lines().map(str::to_owned).collect()
    }

    /// Pi and OMP are pinned to idle, working, and blocked viewports captured
    /// from the corresponding shipped CLI. This catches both silent signal
    /// drift and the more dangerous false-blocked regression.
    #[test]
    fn captured_agent_screens_match_only_their_live_state() {
        let fixtures = [
            (
                "pi",
                include_str!("fixtures/pi/idle_prompt.txt"),
                include_str!("fixtures/pi/working.txt"),
                include_str!("fixtures/pi/blocked_trust.txt"),
                "bottom-working-status",
                "project-trust-dialog",
            ),
            (
                "omp",
                include_str!("fixtures/omp/idle_prompt.txt"),
                include_str!("fixtures/omp/working.txt"),
                include_str!("fixtures/omp/blocked_tool_approval.txt"),
                "bottom-running-status",
                "tool-approval-dialog",
            ),
        ];

        for (kind, idle, working, blocked, working_rule, blocked_rule) in fixtures {
            let set = compile(builtin(kind));
            let manifest = set.manifest(kind).expect("manifest");

            let idle = captured(idle);
            let got = manifest.evaluate(&Screen {
                title: "",
                lines: &idle,
            });
            assert_eq!(got.state, None, "{kind}: idle is the fail-safe");
            assert!(!got.visible_blocker, "{kind}: idle is not blocked");

            let working = captured(working);
            let got = manifest.evaluate(&Screen {
                title: "",
                lines: &working,
            });
            assert_eq!(
                got.state,
                Some(DetectedState::Working),
                "{kind}: captured working screen",
            );
            assert_eq!(got.matched.as_deref(), Some(working_rule));
            assert!(got.visible_working);
            assert!(!got.visible_blocker);

            let blocked = captured(blocked);
            let got = manifest.evaluate(&Screen {
                title: "",
                lines: &blocked,
            });
            assert_eq!(
                got.state,
                Some(DetectedState::Blocked),
                "{kind}: captured blocked screen",
            );
            assert_eq!(got.matched.as_deref(), Some(blocked_rule));
            assert!(got.visible_blocker);

            let mut transcript_then_idle = blocked;
            transcript_then_idle.extend(idle);
            let got = manifest.evaluate(&Screen {
                title: "",
                lines: &transcript_then_idle,
            });
            assert_ne!(
                got.state,
                Some(DetectedState::Blocked),
                "{kind}: a historical dialog above live idle chrome must not block",
            );
            assert!(!got.visible_blocker);
        }
    }

    /// Every shipped built-in must compile and own each declared binary alias;
    /// otherwise that agent kind silently disappears in production.
    #[test]
    fn every_builtin_manifest_compiles_and_indexes_its_binaries() {
        // `global()` is env-sensitive; compile each embedded manifest directly
        // so this test is hermetic.
        let expected = [
            ("claude", &["claude", "claude-code"][..]),
            ("codex", &["codex"][..]),
            ("opencode", &["opencode", "opencode2"][..]),
            ("pi", &["pi"][..]),
            ("omp", &["omp"][..]),
        ];

        for (kind, binaries) in expected {
            let set = compile(builtin(kind));
            for binary in binaries {
                assert_eq!(set.kind_for_binary(binary), Some(kind));
            }
            let manifest = set.manifest(kind).expect("manifest");
            assert_eq!(manifest.name, kind);
            assert!(!manifest.rules.is_empty());
        }
    }

    // --- Codex goldens ------------------------------------------------------
    //
    // REAL viewports and REAL titles captured from Codex 0.145.0 running in a
    // phux pane: the screens via `phux snapshot --json`, the titles by
    // recording every `title_changed` event over one driven turn. Re-capture
    // them when Codex's TUI changes; do not hand-edit them.

    fn codex_screen(body: &str) -> Vec<String> {
        lines(body.lines().collect::<Vec<_>>().as_slice())
    }

    fn codex_idle_screen() -> Vec<String> {
        codex_screen(include_str!("fixtures/codex/idle_prompt.txt"))
    }

    fn codex_working_screen() -> Vec<String> {
        codex_screen(include_str!("fixtures/codex/working.txt"))
    }

    fn codex_blocked_screen() -> Vec<String> {
        codex_screen(include_str!("fixtures/codex/blocked_approval.txt"))
    }

    /// Two frames of the ten-frame braille spinner Codex animates in its OSC
    /// title while a turn runs. Both observed ~15 times over one turn.
    const CODEX_TITLE_BUSY_A: &str = "\u{280b} tmp";
    const CODEX_TITLE_BUSY_B: &str = "\u{2834} tmp";
    /// The title when no turn is running: the bare cwd basename, no prefix.
    /// Covers idle AND waiting-on-approval alike, which is why it has no rule.
    const CODEX_TITLE_QUIET: &str = "tmp";

    fn codex_eval(title: &str, screen: &[String]) -> super::Evaluation {
        let set = compile(builtin("codex"));
        let manifest = set.manifest("codex").expect("codex manifest");
        manifest.evaluate(&Screen {
            title,
            lines: screen,
        })
    }

    /// The spinner proves `working`, on every frame of the animation.
    #[test]
    fn codex_spinner_title_is_working() {
        for title in [CODEX_TITLE_BUSY_A, CODEX_TITLE_BUSY_B] {
            let got = codex_eval(title, &codex_working_screen());
            assert_eq!(
                got.state,
                Some(DetectedState::Working),
                "spinner frame {title:?} must read as working"
            );
        }
    }

    /// The whole Braille block is matched, not ten enumerated codepoints, so a
    /// Codex that reorders or extends its spinner keeps working.
    #[test]
    fn codex_matches_any_braille_spinner_frame() {
        for cp in ['\u{2800}', '\u{280f}', '\u{283c}', '\u{28ff}'] {
            let title = format!("{cp} tmp");
            let got = codex_eval(&title, &codex_idle_screen());
            assert_eq!(
                got.state,
                Some(DetectedState::Working),
                "braille frame {title:?} must read as working"
            );
        }
    }

    /// The bare title asserts nothing. It means "not running a turn", which
    /// covers idle AND waiting-on-approval; claiming `idle` here would outrank
    /// the screen and mask every approval prompt.
    #[test]
    fn codex_bare_title_asserts_nothing() {
        let got = codex_eval(CODEX_TITLE_QUIET, &codex_idle_screen());
        assert_ne!(
            got.state,
            Some(DetectedState::Working),
            "a bare title must not read as working"
        );
    }

    /// The captured approval dialog reads as `blocked`, with the quiet title
    /// that really accompanies it.
    #[test]
    fn codex_approval_prompt_is_blocked() {
        let got = codex_eval(CODEX_TITLE_QUIET, &codex_blocked_screen());
        assert_eq!(got.state, Some(DetectedState::Blocked));
    }

    /// The guard that matters: prose alone must not trip `blocked`. The
    /// transcript can legitimately contain the question stem inside a quoted
    /// session; without a numbered option line it is not a live dialog.
    #[test]
    fn codex_question_stem_without_an_option_list_is_not_blocked() {
        let screen = lines(&[
            "  I was going to ask: would you like to run the following command?",
            "  ...but I decided against it.",
        ]);
        let got = codex_eval(CODEX_TITLE_QUIET, &screen);
        assert_ne!(
            got.state,
            Some(DetectedState::Blocked),
            "prose without an option list must not read as blocked"
        );
    }

    /// A false `blocked` is the expensive failure (ADR-0046 D). The real idle
    /// screen must not produce one.
    #[test]
    fn codex_idle_screen_is_not_blocked() {
        let got = codex_eval(CODEX_TITLE_QUIET, &codex_idle_screen());
        assert_ne!(got.state, Some(DetectedState::Blocked));
    }

    /// The shipped built-in must compile. If this fails, `rules/codex.toml`
    /// is broken and the detector silently does nothing in production.
    #[test]
    fn builtin_codex_manifest_compiles_and_indexes_its_binaries() {
        let set = compile(builtin("codex"));
        assert_eq!(set.kind_for_binary("codex"), Some("codex"));
        let manifest = set.manifest("codex").expect("codex manifest");
        assert_eq!(manifest.name, "codex");
        assert!(!manifest.rules.is_empty());
    }

    /// Both built-ins must coexist: registering a second manifest must not
    /// displace the first, and neither kind may capture the other's binary.
    #[test]
    fn builtin_manifests_do_not_collide() {
        let set = global();
        assert_eq!(set.kind_for_binary("claude"), Some("claude"));
        assert_eq!(set.kind_for_binary("codex"), Some("codex"));
        assert_eq!(set.kind_for_binary("opencode2"), Some("opencode"));
    }

    // --- OpenCode goldens ---------------------------------------------------
    //
    // REAL viewports captured from OpenCode 1.17.18 in a phux pane. Note the
    // structural difference from Claude/Codex: `OpenCode`'s OSC title is the
    // conversation title, not a spinner, so every rule here is a screen rule
    // and the title argument is irrelevant to the outcome.

    fn opencode_idle_screen() -> Vec<String> {
        lines(
            include_str!("fixtures/opencode/idle_prompt.txt")
                .lines()
                .collect::<Vec<_>>()
                .as_slice(),
        )
    }

    fn opencode_working_screen() -> Vec<String> {
        lines(
            include_str!("fixtures/opencode/working.txt")
                .lines()
                .collect::<Vec<_>>()
                .as_slice(),
        )
    }

    fn opencode_blocked_screen() -> Vec<String> {
        lines(
            include_str!("fixtures/opencode/blocked_permission.txt")
                .lines()
                .collect::<Vec<_>>()
                .as_slice(),
        )
    }

    fn opencode_eval(title: &str, screen: &[String]) -> super::Evaluation {
        let set = compile(builtin("opencode"));
        let manifest = set.manifest("opencode").expect("opencode manifest");
        manifest.evaluate(&Screen {
            title,
            lines: screen,
        })
    }

    /// The footer's interrupt affordance is the working signal.
    #[test]
    fn opencode_footer_interrupt_is_working() {
        let got = opencode_eval("OC | whatever", &opencode_working_screen());
        assert_eq!(got.state, Some(DetectedState::Working));
    }

    /// The real idle screen must not read as working. Both screens paint the
    /// same composer, so this is the discriminator doing its job.
    #[test]
    fn opencode_idle_screen_is_not_working() {
        let got = opencode_eval("OpenCode", &opencode_idle_screen());
        assert_ne!(got.state, Some(DetectedState::Working));
    }

    /// The title is the conversation title and must not influence the
    /// verdict. A title that merely mentions the word must not flip state.
    #[test]
    fn opencode_title_does_not_decide_state() {
        let idle_a = opencode_eval("OpenCode", &opencode_idle_screen()).state;
        let idle_b = opencode_eval("OC | esc interrupt", &opencode_idle_screen()).state;
        assert_eq!(idle_a, idle_b, "the title changed the verdict");
    }

    /// The captured external-directory permission dialog reads as `blocked`;
    /// the idle and working goldens do not.
    #[test]
    fn opencode_permission_dialog_is_blocked_without_false_positives() {
        let blocked = opencode_eval("OC | whatever", &opencode_blocked_screen());
        assert_eq!(blocked.state, Some(DetectedState::Blocked));
        assert_eq!(
            blocked.matched.as_deref(),
            Some("permission-required-dialog")
        );
        assert!(blocked.visible_blocker);

        for screen in [opencode_idle_screen(), opencode_working_screen()] {
            let got = opencode_eval("OpenCode", &screen);
            assert_ne!(got.state, Some(DetectedState::Blocked));
        }
    }

    /// The shipped built-in must compile and index every observed process
    /// name, including the npm-scope path component.
    #[test]
    fn builtin_opencode_manifest_compiles_and_indexes_its_binaries() {
        let set = compile(builtin("opencode"));
        for name in ["opencode", "opencode2", "@opencode-ai"] {
            assert_eq!(set.kind_for_binary(name), Some("opencode"), "missed {name}");
        }
    }

    /// The non-short-circuiting trace walker must agree with the
    /// short-circuiting production matcher on every rule of every built-in,
    /// against every committed golden capture. Two evaluators is how a
    /// debugger starts lying about the thing it is debugging; this is the
    /// only reason a second one is tolerable at all.
    #[test]
    fn the_trace_agrees_with_the_production_evaluator() {
        let goldens: &[(&str, &[&str])] = &[
            (
                "claude",
                &[
                    include_str!("fixtures/claude/idle_prompt.txt"),
                    include_str!("fixtures/claude/working.txt"),
                    include_str!("fixtures/claude/blocked_permission.txt"),
                ],
            ),
            (
                "codex",
                &[
                    include_str!("fixtures/codex/idle_prompt.txt"),
                    include_str!("fixtures/codex/working.txt"),
                    include_str!("fixtures/codex/blocked_approval.txt"),
                ],
            ),
            (
                "opencode",
                &[
                    include_str!("fixtures/opencode/idle_prompt.txt"),
                    include_str!("fixtures/opencode/working.txt"),
                    include_str!("fixtures/opencode/blocked_permission.txt"),
                ],
            ),
            (
                "pi",
                &[
                    include_str!("fixtures/pi/idle_prompt.txt"),
                    include_str!("fixtures/pi/working.txt"),
                    include_str!("fixtures/pi/blocked_trust.txt"),
                ],
            ),
            (
                "omp",
                &[
                    include_str!("fixtures/omp/idle_prompt.txt"),
                    include_str!("fixtures/omp/working.txt"),
                    include_str!("fixtures/omp/blocked_tool_approval.txt"),
                ],
            ),
        ];

        // Titles that exercise both the spinner and the quiet arms, plus the
        // empty title a capture file supplies by default.
        let titles = ["", CLAUDE_TITLE_BUSY_A, CLAUDE_TITLE_QUIET, "\u{280b} tmp"];

        for (kind, screens) in goldens {
            let set = compile(builtin(kind));
            let manifest = set.manifest(kind).expect("manifest");
            for body in *screens {
                let buf = captured(body);
                for title in titles {
                    let screen = Screen { title, lines: &buf };
                    let direct = manifest.evaluate(&screen);
                    let explained = manifest.explain(&screen);
                    assert_eq!(
                        direct, explained.evaluation,
                        "{kind}: the traced pass changed the verdict",
                    );
                    assert_eq!(
                        explained.rules.len(),
                        manifest.rules.len(),
                        "{kind}: every rule must be reported, misses included",
                    );
                    for trace in &explained.rules {
                        assert_eq!(
                            trace.matched, trace.predicate.matched,
                            "{kind}/{}: the evidence tree's root disagrees with the matcher",
                            trace.id,
                        );
                    }
                }
            }
        }
    }

    /// A combinator's children are ALL evaluated, so an author can see which
    /// conjunct failed rather than only that the `all` did. The production
    /// matcher short-circuits; the trace must not.
    #[test]
    fn the_trace_does_not_short_circuit_a_conjunction() {
        let set = compile(SAMPLE);
        let manifest = set.manifest("sample").expect("manifest");
        // Neither conjunct of `screen-blocked` holds.
        let buf = lines(&["nothing here at all"]);
        let explained = manifest.explain(&Screen {
            title: "",
            lines: &buf,
        });
        let rule = explained
            .rules
            .iter()
            .find(|r| r.id == "screen-blocked")
            .expect("rule reported");
        assert!(!rule.matched);
        assert_eq!(rule.predicate.op, "all");
        assert_eq!(
            rule.predicate.children.len(),
            2,
            "both conjuncts must be evaluated and reported",
        );
        assert!(rule.predicate.children.iter().all(|c| !c.matched));
        assert!(
            rule.predicate.children.iter().all(|c| c.pattern.is_some()),
            "every leaf names the pattern it ran",
        );
    }

    #[test]
    fn global_is_memoized() {
        let a = global();
        let b = global();
        assert!(std::rc::Rc::ptr_eq(&a, &b));
    }
}
