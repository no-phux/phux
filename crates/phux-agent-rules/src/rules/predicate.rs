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
