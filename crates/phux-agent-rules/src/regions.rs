//! Structural sub-slices of the live viewport (ADR-0046 §C).
//!
//! A rule matches a region, never the whole screen: `"do you want to
//! proceed?"` in a printed diff means nothing, while the same text in the
//! live prompt box means the agent is blocked. Every extractor fails safe to
//! an empty region rather than widening, because a false `blocked` is the
//! failure that destroys trust in the feature.

/// The live screen a rule set is evaluated against.
#[derive(Debug, Clone, Copy)]
pub struct Screen<'a> {
    /// The pane's current OSC 0/2 title, as libghostty tracks it.
    pub title: &'a str,
    /// Last ConEmu-style OSC 9;4 payload, with the leading `9;` removed.
    pub progress: &'a str,
    /// Right-trimmed live viewport rows, top to bottom (no scrollback).
    pub lines: &'a [String],
}

/// A named sub-slice of [`Screen`]. The two windowed variants carry a line
/// count (`bottom-lines(1)` anchors on a status row, `bottom-lines(14)`
/// reaches a footer block).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Region {
    /// The OSC 0/2 window title: the cheapest, most direct signal.
    Title,
    /// The latest OSC 9;4 progress payload, independent of title rendering.
    OscProgress,
    /// The last N non-empty rows (`bottom-lines[(N)]`).
    BottomLines(u16),
    /// The first N non-empty rows (`top-non-empty-lines[(N)]`).
    TopNonEmptyLines(u16),
    /// Everything below the last horizontal rule: the live chrome of agents
    /// that fence their transcript with one.
    AfterLastRule,
    /// The body of the live prompt box, borders stripped; empty when none.
    PromptBox,
    /// The whole live viewport. An escape hatch; prefer a narrower region.
    Viewport,
}

impl Region {
    /// The regions the offline explainer always previews (windows in their
    /// default spelling; a manifest's own windows are added to these).
    pub(crate) const ALL: [Self; 6] = [
        Self::Title,
        Self::OscProgress,
        Self::PromptBox,
        Self::AfterLastRule,
        Self::BottomLines(DEFAULT_BOTTOM_LINES),
        Self::Viewport,
    ];

    /// The manifest spelling, N included; round-trips through
    /// [`Self::parse`].
    pub(crate) fn as_str(self) -> String {
        match self {
            Self::Title => "title".to_owned(),
            Self::OscProgress => "osc-progress".to_owned(),
            Self::BottomLines(n) if n == DEFAULT_BOTTOM_LINES => "bottom-lines".to_owned(),
            Self::BottomLines(n) => format!("bottom-lines({n})"),
            Self::TopNonEmptyLines(n) if n == DEFAULT_TOP_NON_EMPTY_LINES => {
                "top-non-empty-lines".to_owned()
            }
            Self::TopNonEmptyLines(n) => format!("top-non-empty-lines({n})"),
            Self::AfterLastRule => "after-last-rule".to_owned(),
            Self::PromptBox => "prompt-box".to_owned(),
            Self::Viewport => "viewport".to_owned(),
        }
    }

    /// Parse a manifest's `region = "..."`: a word, with a parenthesized
    /// line count on the windowed regions. The error is the author's whole
    /// diagnostic, since a bad region drops the manifest.
    fn parse(spec: &str) -> Result<Self, String> {
        let trimmed = spec.trim();
        let (word, arg) = match trimmed.strip_suffix(')') {
            Some(head) => match head.split_once('(') {
                Some((word, count)) => (word.trim(), Some(count.trim())),
                None => return Err(format!("region `{spec}`: `)` without a matching `(`")),
            },
            None => (trimmed, None),
        };

        // A count on an unwindowed region must not be silently ignored.
        let unwindowed = |region: Self| -> Result<Self, String> {
            if arg.is_some() {
                return Err(format!("region `{spec}`: `{word}` takes no line count"));
            }
            Ok(region)
        };
        let windowed = |default: u16| -> Result<u16, String> {
            let Some(text) = arg else { return Ok(default) };
            let count: u16 = text
                .parse()
                .map_err(|_| format!("region `{spec}`: `{text}` is not a line count"))?;
            if count == 0 {
                return Err(format!(
                    "region `{spec}`: a line count of 0 selects nothing, so no rule scoped \
                     to it could ever match"
                ));
            }
            // An overshoot means "all of them": clamp rather than reject.
            Ok(count.min(MAX_REGION_LINES))
        };

        match word {
            "title" => unwindowed(Self::Title),
            "osc-progress" => unwindowed(Self::OscProgress),
            "after-last-rule" => unwindowed(Self::AfterLastRule),
            "prompt-box" => unwindowed(Self::PromptBox),
            "viewport" => unwindowed(Self::Viewport),
            "bottom-lines" => Ok(Self::BottomLines(windowed(DEFAULT_BOTTOM_LINES)?)),
            "top-non-empty-lines" => Ok(Self::TopNonEmptyLines(windowed(
                DEFAULT_TOP_NON_EMPTY_LINES,
            )?)),
            _ => Err(format!(
                "region `{spec}`: unknown region; expected one of title, osc-progress, prompt-box, \
                 after-last-rule, bottom-lines[(N)], top-non-empty-lines[(N)], viewport"
            )),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Region {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// How many trailing non-empty rows a bare `bottom-lines` yields.
const DEFAULT_BOTTOM_LINES: u16 = 6;

/// How many leading non-empty rows a bare `top-non-empty-lines` yields: a
/// banner is the first row.
const DEFAULT_TOP_NON_EMPTY_LINES: u16 = 1;

/// The largest window either parameterized region resolves (larger clamps).
const MAX_REGION_LINES: u16 = 512;

/// How many non-box status rows [`Region::PromptBox`] skips below the box
/// before giving up, so it never drifts up into a transcript box.
const PROMPT_BOX_TRAILING_SLACK: usize = 4;

/// Characters a horizontal rule may be built from.
const RULE_CHARS: &str = "─━═╌┄┈—-_╭╮╯╰┌┐└┘├┤┬┴┼│┃┏┓┗┛";

/// Characters that can open a box-drawn line's left border.
const BOX_OPEN_CHARS: [char; 8] = ['│', '╭', '╰', '┌', '└', '┃', '┏', '┗'];

/// The subset of [`BOX_OPEN_CHARS`] that opens a border row, which is chrome
/// even when it carries a label (`╭─ Input ─╮`).
const BOX_CORNER_CHARS: [char; 6] = ['╭', '╰', '┌', '└', '┏', '┗'];

/// Minimum width for a line to count as a horizontal rule.
const RULE_MIN_WIDTH: usize = 8;

/// Whether `line` is a horizontal rule: at least [`RULE_MIN_WIDTH`]
/// characters, every one of them drawn from [`RULE_CHARS`].
pub(crate) fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.chars().count() >= RULE_MIN_WIDTH && trimmed.chars().all(|c| RULE_CHARS.contains(c))
}

/// Whether `line`'s first non-space character opens a box border.
fn is_box_line(line: &str) -> bool {
    line.trim_start()
        .chars()
        .next()
        .is_some_and(|c| BOX_OPEN_CHARS.contains(&c))
}

/// Strip a box line's borders: `│ > hello   │` becomes `> hello`, and a
/// border row (labelled or not) becomes empty.
fn strip_borders(line: &str) -> &str {
    let s = line.trim();
    if s.starts_with(|c| BOX_CORNER_CHARS.contains(&c)) {
        return "";
    }
    let s = s.strip_prefix(|c| BOX_OPEN_CHARS.contains(&c)).unwrap_or(s);
    let s = s
        .strip_suffix(|c: char| {
            c == '│' || c == '╮' || c == '╯' || c == '┐' || c == '┘' || c == '┃'
        })
        .unwrap_or(s);
    let s = s.trim();
    if !s.is_empty() && s.chars().all(|c| RULE_CHARS.contains(c)) {
        return "";
    }
    s
}

/// Extract `region` from `screen` as a list of borrowed lines.
pub(crate) fn extract<'a>(region: Region, screen: &Screen<'a>) -> Vec<&'a str> {
    match region {
        Region::Title => vec![screen.title],
        Region::OscProgress => vec![screen.progress],
        Region::Viewport => screen.lines.iter().map(String::as_str).collect(),
        Region::BottomLines(count) => bottom_lines(screen.lines, count as usize),
        Region::TopNonEmptyLines(count) => top_non_empty_lines(screen.lines, count as usize),
        Region::AfterLastRule => after_last_rule(screen.lines),
        Region::PromptBox => prompt_box(screen.lines),
    }
}

/// The last `count` non-empty rows, in screen order.
fn bottom_lines(lines: &[String], count: usize) -> Vec<&str> {
    let mut picked: Vec<&str> = lines
        .iter()
        .rev()
        .map(String::as_str)
        .filter(|l| !l.trim().is_empty())
        .take(count)
        .collect();
    picked.reverse();
    picked
}

/// The first `count` non-empty rows (blank padding is skipped, not counted).
fn top_non_empty_lines(lines: &[String], count: usize) -> Vec<&str> {
    lines
        .iter()
        .map(String::as_str)
        .filter(|l| !l.trim().is_empty())
        .take(count)
        .collect()
}

/// Everything strictly below the last horizontal rule; empty (never the
/// whole viewport) when there is no rule.
fn after_last_rule(lines: &[String]) -> Vec<&str> {
    let Some(last) = lines.iter().rposition(|l| is_rule(l)) else {
        return Vec::new();
    };
    lines[last + 1..].iter().map(String::as_str).collect()
}

/// The body of the live prompt box, borders stripped: either a box-drawn
/// run of lines or a body fenced by two horizontal rules (Claude Code).
///
/// Scans up from the bottom, skipping blanks and at most
/// [`PROMPT_BOX_TRAILING_SLACK`] status rows; the first border or rule
/// decides the form. Empty when no box is found.
fn prompt_box(lines: &[String]) -> Vec<&str> {
    let Some(end) = prompt_box_bottom(lines) else {
        return Vec::new();
    };
    // A box before a rule: a box's bottom border is all rule characters, and
    // treating it as a fence would climb past the box into the transcript
    // (a false `blocked` on a printed question).
    let body = if is_box_line(&lines[end]) {
        Some(box_run_ending_at(lines, end))
    } else {
        rule_fenced_body(lines, end)
    };
    body.map_or_else(Vec::new, |body| {
        body.iter().map(|l| strip_borders(l)).collect()
    })
}

/// The bottom-most border or rule row, found scanning up past blanks and at
/// most [`PROMPT_BOX_TRAILING_SLACK`] other rows.
fn prompt_box_bottom(lines: &[String]) -> Option<usize> {
    let mut slack = PROMPT_BOX_TRAILING_SLACK;
    for (idx, line) in lines.iter().enumerate().rev() {
        if line.trim().is_empty() {
            continue;
        }
        if is_rule(line) || is_box_line(line) {
            return Some(idx);
        }
        if slack == 0 {
            return None;
        }
        slack -= 1;
    }
    None
}

/// The contiguous run of box-drawn rows ending at `end`, inclusive.
fn box_run_ending_at(lines: &[String], end: usize) -> &[String] {
    let start = lines[..end]
        .iter()
        .rposition(|l| !is_box_line(l))
        .map_or(0, |above| above + 1);
    &lines[start..=end]
}

/// The rows between the rule at `end` and the opening rule above it. A box
/// line on the way, or no opening fence (a lone separator), means no box.
fn rule_fenced_body(lines: &[String], end: usize) -> Option<&[String]> {
    let open = lines[..end]
        .iter()
        .rposition(|l| is_box_line(l) || is_rule(l))?;
    if is_box_line(&lines[open]) {
        return None;
    }
    Some(&lines[open + 1..end])
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{Region, Screen, extract, is_rule};

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    fn get(region: Region, raw: &[&str]) -> Vec<String> {
        let buf = lines(raw);
        let screen = Screen {
            title: "\u{2802} claude",
            progress: "4;3;",
            lines: &buf,
        };
        extract(region, &screen)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn parse(spec: &str) -> Result<Region, String> {
        serde_json::from_str::<Region>(&format!("\"{spec}\"")).map_err(|e| e.to_string())
    }

    /// Every spelling round-trips (what the explainer prints is what an
    /// operator types back), bare windows keep their defaults, huge counts
    /// clamp, and malformed specs are rejected rather than reinterpreted.
    #[test]
    fn region_specs_parse_and_round_trip() {
        for region in [
            Region::Title,
            Region::OscProgress,
            Region::PromptBox,
            Region::AfterLastRule,
            Region::Viewport,
            Region::BottomLines(6),
            Region::BottomLines(1),
            Region::BottomLines(14),
            Region::TopNonEmptyLines(1),
            Region::TopNonEmptyLines(3),
        ] {
            assert_eq!(parse(&region.as_str()), Ok(region));
        }
        for (spec, want) in [
            ("bottom-lines", Region::BottomLines(6)),
            ("top-non-empty-lines", Region::TopNonEmptyLines(1)),
            ("bottom-lines( 8 )", Region::BottomLines(8)),
            ("bottom-lines(60000)", Region::BottomLines(512)),
        ] {
            assert_eq!(parse(spec), Ok(want), "{spec}");
        }
        for (spec, needle) in [
            ("bottom-lines(0)", "selects nothing"),
            ("top-non-empty-lines(0)", "selects nothing"),
            ("title(3)", "takes no line count"),
            ("prompt-box(2)", "takes no line count"),
            ("bottom-lines(x)", "is not a line count"),
            ("bottom-lines(-1)", "is not a line count"),
            ("bottom-lines)", "without a matching"),
            ("bottom_lines", "unknown region"),
        ] {
            let err = parse(spec).expect_err(spec);
            assert!(err.contains(needle), "{spec}: {err}");
        }
    }

    #[test]
    fn simple_regions_extract_their_rows() {
        let rows = ["1", "", "2", "3", "", "4", "5", "6", "7", "8", ""];
        let top = ["", "  banner  ", "", "second", "third", "fourth"];
        let fenced = ["head", "────────", "mid", "════════", "tail-a", "tail-b"];
        let cases: &[(Region, &[&str], &[&str])] = &[
            (Region::Title, &["a"], &["\u{2802} claude"]),
            (Region::OscProgress, &[], &["4;3;"]),
            (Region::Viewport, &["a", "", "b"], &["a", "", "b"]),
            (
                Region::BottomLines(6),
                &rows,
                &["3", "4", "5", "6", "7", "8"],
            ),
            (Region::BottomLines(1), &rows, &["8"]),
            (
                Region::BottomLines(512),
                &rows,
                &["1", "2", "3", "4", "5", "6", "7", "8"],
            ),
            (Region::BottomLines(6), &["only"], &["only"]),
            (Region::TopNonEmptyLines(1), &top, &["  banner  "]),
            (
                Region::TopNonEmptyLines(3),
                &top,
                &["  banner  ", "second", "third"],
            ),
            (Region::TopNonEmptyLines(2), &[], &[]),
            (Region::AfterLastRule, &fenced, &["tail-a", "tail-b"]),
            // No rule means no live chrome, never the whole viewport.
            (
                Region::AfterLastRule,
                &["Do you want to proceed?", "1. Yes"],
                &[],
            ),
        ];
        for &(region, screen, want) in cases {
            assert_eq!(get(region, screen), want, "{region:?} over {screen:?}");
        }
        assert!(is_rule("  ━━━━━━━━━━  "));
        for not_rule in ["───", "──── x ────", ""] {
            assert!(!is_rule(not_rule), "{not_rule:?}");
        }
    }

    /// The prompt box is the live input and nothing else. Boxes are drawn at
    /// realistic width, where a box border also satisfies `is_rule`.
    #[test]
    fn prompt_box_extracts_only_the_live_input() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            (
                "rule-delimited (Claude Code 2.1.207)",
                &[
                    "  transcript",
                    "────────────────",
                    "\u{276f} hello",
                    "────────────────",
                    "  Opus 4.8  \u{2387} main",
                    "  -- INSERT --",
                ],
                &["\u{276f} hello"],
            ),
            (
                "a lone rule is a separator, not a box",
                &[
                    "  transcript",
                    "────────────────",
                    " Do you want to proceed?",
                ],
                &[],
            ),
            (
                "borders stripped, hint row skipped",
                &[
                    "transcript line",
                    "╭──────────╮",
                    "│ > hello           │",
                    "╰──────────╯",
                    "  ? for shortcuts",
                ],
                &["", "> hello", ""],
            ),
            (
                // Treating the bottom border as a fence would climb past the
                // titled box and swallow the printed question: a false blocked.
                "a titled box is a box, not a rule fence",
                &[
                    "assistant text",
                    "────────────",
                    "  Do you want to proceed?",
                    "  \u{276f} 1. Yes",
                    "    2. No",
                    "  more prose",
                    "╭─ Input ───────╮",
                    "│ \u{276f}                │",
                    "╰───────────────╯",
                    "  ? for shortcuts",
                ],
                &["", "\u{276f}", ""],
            ),
            (
                "a rule-fenced body never fences against a transcript box",
                &[
                    "╭──────────╮",
                    "│ a rendered diff  │",
                    "╰──────────╯",
                    "  Do you want to proceed?",
                    "────────────",
                    "\u{276f} typing here",
                ],
                &[],
            ),
            ("no box at all", &["just", "plain", "text"], &[]),
            (
                "a box past the trailing slack is transcript",
                &[
                    "╭──────────╮",
                    "│ old diff          │",
                    "╰──────────╯",
                    "a",
                    "b",
                    "c",
                    "d",
                    "e",
                ],
                &[],
            ),
            (
                "the bottom-most box wins",
                &[
                    "╭──────────╮",
                    "│ old               │",
                    "╰──────────╯",
                    "prose in between",
                    "╭──────────╮",
                    "│ live              │",
                    "╰──────────╯",
                ],
                &["", "live", ""],
            ),
        ];
        for &(what, screen, want) in cases {
            assert_eq!(get(Region::PromptBox, screen), want, "{what}");
        }
    }
}
