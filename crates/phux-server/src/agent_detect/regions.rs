//! Structural sub-slices of the live screen (ADR-0046 §C).
//!
//! A rule never matches against the whole screen. It matches against a
//! **region**: a structurally derived sub-slice of the *live viewport*
//! (never scrollback). This is the decisive idea of the detector. The
//! string `"do you want to proceed?"` sitting inside a diff that an agent
//! just printed means nothing; the same string inside the live prompt box
//! means the agent is blocked on a human. Region selection converts a
//! fuzzy text-matching problem into a mostly structural one, which is what
//! makes a false `blocked` — the one failure mode that destroys trust in
//! the whole feature — rare enough to ship.
//!
//! Every extractor borrows from the caller's line buffer; none allocate a
//! new `String`.

/// The live screen a rule set is evaluated against.
///
/// `lines` are the right-trimmed rows of the **live viewport**, top to
/// bottom, exactly as [`crate::grid::SnapshotSynthesizer::screen_state_with_scrollback`]
/// projects them with `scrollback = None`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Screen<'a> {
    /// The pane's current OSC 0/2 title, as libghostty tracks it.
    pub(crate) title: &'a str,
    /// Live viewport rows, top to bottom.
    pub(crate) lines: &'a [String],
}

/// A named sub-slice of [`Screen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Region {
    /// The OSC 0/2 window title. The cheapest and highest-value signal:
    /// it flips the instant the agent's state changes, costs no grid lock
    /// and no allocation, and agent CLIs already maintain it.
    Title,
    /// The last [`BOTTOM_LINES`] **non-empty** rows, in screen order. Where
    /// a TUI agent paints its status line and key hints.
    BottomLines,
    /// Everything below the last horizontal rule (see [`is_rule`]). Agents
    /// that separate their transcript from their live chrome with a rule
    /// make this the exact "live chrome" slice.
    AfterLastRule,
    /// The body of the live prompt box: the bottom-most contiguous run of
    /// box-drawing-bordered lines, with the border glyphs stripped. Empty
    /// when no box is found, so a `PromptBox` rule then simply cannot
    /// match — the correct fail-safe.
    PromptBox,
    /// The whole live viewport. An escape hatch; prefer a narrower region.
    Viewport,
}

impl Region {
    /// Every region a manifest may name, in the order the offline explainer
    /// previews them: cheapest and most structural first.
    ///
    /// Exhaustive by construction — [`Self::as_str`] matches on the same
    /// variants, so adding one without extending this list fails to compile
    /// there and is caught by `every_region_is_previewable`.
    pub(crate) const ALL: [Self; 5] = [
        Self::Title,
        Self::PromptBox,
        Self::AfterLastRule,
        Self::BottomLines,
        Self::Viewport,
    ];

    /// The kebab-case word a manifest spells this region with.
    ///
    /// The `Deserialize` derive owns the parse direction; this is the render
    /// direction, so `phux agent explain` can name a region in exactly the
    /// spelling an operator has to type back into their TOML.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::BottomLines => "bottom-lines",
            Self::AfterLastRule => "after-last-rule",
            Self::PromptBox => "prompt-box",
            Self::Viewport => "viewport",
        }
    }
}

/// How many trailing non-empty rows [`Region::BottomLines`] yields.
const BOTTOM_LINES: usize = 6;

/// How many non-empty, non-box rows [`Region::PromptBox`] may skip while
/// scanning up from the bottom before it gives up.
///
/// A TUI agent paints at most a hint row or two *below* its prompt box
/// (Claude Code paints exactly one: `? for shortcuts`). Bounding the skip
/// keeps the region anchored to the pane's **live** bottom chrome instead
/// of drifting up into a box that merely happens to be in the transcript
/// (a rendered diff, a tool-call frame) — which would reintroduce the very
/// false-positive the region concept exists to prevent.
const PROMPT_BOX_TRAILING_SLACK: usize = 4;

/// Characters a horizontal rule may be built from.
const RULE_CHARS: &str = "─━═╌┄┈—-_╭╮╯╰┌┐└┘├┤┬┴┼│┃┏┓┗┛";

/// Characters that can open a box-drawn line's left border.
const BOX_OPEN_CHARS: [char; 8] = ['│', '╭', '╰', '┌', '└', '┃', '┏', '┗'];

/// The subset of [`BOX_OPEN_CHARS`] that opens a box's top or bottom BORDER
/// row, as opposed to a body row (which opens with a vertical).
///
/// A border row is chrome, never text — even when the agent draws a label into
/// it (`╭─ Input ─╮`, the ratatui `Block::title` default). Its label must not
/// reach a predicate.
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

/// Strip a box line's leading and trailing border glyphs.
///
/// `│ > hello   │` becomes `> hello`. A border row (`╭────╮`, and equally the
/// labelled `╭─ Input ─╮`) becomes the empty string: it is rule, not text, and
/// letting its dashes — or its label — through would put characters into the
/// region that no predicate should ever be able to see.
fn strip_borders(line: &str) -> &str {
    let s = line.trim();
    // A corner opens a border row. Decided on the OPENING glyph, not on the
    // row's contents: a labelled border is not all rule characters, so the
    // all-rule-chars test below would let `─ Input ─` straight through.
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
        Region::Viewport => screen.lines.iter().map(String::as_str).collect(),
        Region::BottomLines => bottom_lines(screen.lines),
        Region::AfterLastRule => after_last_rule(screen.lines),
        Region::PromptBox => prompt_box(screen.lines),
    }
}

/// The last [`BOTTOM_LINES`] non-empty rows, in screen order.
fn bottom_lines(lines: &[String]) -> Vec<&str> {
    let mut picked: Vec<&str> = lines
        .iter()
        .rev()
        .map(String::as_str)
        .filter(|l| !l.trim().is_empty())
        .take(BOTTOM_LINES)
        .collect();
    picked.reverse();
    picked
}

/// Everything strictly below the last horizontal rule.
///
/// **Empty when the screen has no rule at all.** Degrading to the whole
/// viewport would be the wrong failure: a rule scoped here is scoped here
/// precisely because it must not see the transcript, and a screen with no
/// live chrome is a screen we know nothing about. Widening the region on
/// the way to knowing nothing is how a quoted `"Do you want to proceed?"`
/// in a printed diff becomes a false `blocked` — the one failure mode
/// ADR-0046 §D forbids. No rule, no region, no match, fail safe to idle.
fn after_last_rule(lines: &[String]) -> Vec<&str> {
    let Some(last) = lines.iter().rposition(|l| is_rule(l)) else {
        return Vec::new();
    };
    lines[last + 1..].iter().map(String::as_str).collect()
}

/// The body of the live prompt box, borders stripped.
///
/// TUI agents draw the input box one of two ways, and both occur in the
/// wild, so the extractor recognizes both:
///
/// * **Box-drawn** — a contiguous run of lines opened by a border glyph
///   (`╭`/`│`/`└`…).
/// * **Rule-delimited** — a body fenced by two horizontal rules, which is
///   what Claude Code ships today:
///
///   ```text
///   ────────────────   <- fence
///   ❯ some input          <- body
///   ────────────────   <- fence
///     Opus 4.8  ⎇ main    <- status rows (the trailing slack)
///   ```
///
/// Scans up from the last row, skipping blank rows freely and at most
/// [`PROMPT_BOX_TRAILING_SLACK`] non-blank rows that are neither a border
/// nor a rule (the status/hint rows an agent paints *below* its box). The
/// first border-or-rule line found decides the form and closes the box.
///
/// Empty when no box is found — a `PromptBox` rule then simply cannot
/// match, which is the correct fail-safe.
fn prompt_box(lines: &[String]) -> Vec<&str> {
    let mut end: Option<usize> = None;
    let mut slack = PROMPT_BOX_TRAILING_SLACK;
    for (idx, line) in lines.iter().enumerate().rev() {
        if line.trim().is_empty() {
            continue;
        }
        if is_rule(line) || is_box_line(line) {
            end = Some(idx);
            break;
        }
        if slack == 0 {
            break;
        }
        slack -= 1;
    }
    let Some(end) = end else { return Vec::new() };

    // A BOX IS A BOX BEFORE IT IS A RULE. This test order is the whole
    // correctness of the extractor, not a stylistic choice.
    //
    // `RULE_CHARS` contains the corner and vertical glyphs, so a closed box's
    // bottom border (`╰──────────────────╯`) is made entirely of rule
    // characters and satisfies `is_rule` too, at any width >= RULE_MIN_WIDTH.
    // Asking `is_rule` first therefore routed every real-width box into the
    // rule-delimited branch, whose upward search is not bounded to the box: it
    // would keep climbing past the box's own top border and fence the region
    // against a markdown rule the agent had printed into its TRANSCRIPT. The
    // "live prompt box" then contained transcript lines, and a blocked-asserting
    // prompt-box rule would fire on a question the agent merely PRINTED — the
    // false `blocked` that ADR-0046 §D forbids and that this whole module
    // exists to prevent. A bare fence (`─────`) is not opened by a box glyph,
    // so it still reaches the branch below.
    if is_box_line(&lines[end]) {
        let mut start = end;
        while start > 0 && is_box_line(&lines[start - 1]) {
            start -= 1;
        }
        return lines[start..=end]
            .iter()
            .map(|l| strip_borders(l))
            .collect();
    }

    // Rule-delimited: `lines[end]` is a bare horizontal fence, and the body
    // runs up to the opening fence. The search stops at the first box line it
    // meets rather than climbing over it, for the same reason: a box border is
    // a box, not a fence, and fencing against one would splice the rows
    // between a transcript box and the live input into the region. Without an
    // opening fence there is no box at all — a lone rule is a separator (the
    // permission dialog's), not an input box. No box, no region, no match:
    // fail safe.
    let mut open = None;
    for idx in (0..end).rev() {
        if is_box_line(&lines[idx]) {
            return Vec::new();
        }
        if is_rule(&lines[idx]) {
            open = Some(idx);
            break;
        }
    }
    let Some(open) = open else { return Vec::new() };
    lines[open + 1..end]
        .iter()
        .map(|l| strip_borders(l))
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{Region, Screen, extract, is_rule};

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    fn screen<'a>(title: &'a str, buf: &'a [String]) -> Screen<'a> {
        Screen { title, lines: buf }
    }

    /// `Region::ALL` is what `phux agent explain` previews. A region missing
    /// from it is a region an operator can name in a manifest but never see
    /// the resolved text of — which is the exact blindness the offline
    /// explainer exists to remove.
    #[test]
    fn every_region_is_previewable_and_round_trips_its_manifest_spelling() {
        let buf = lines(&["x"]);
        for region in Region::ALL {
            let word = region.as_str();
            let parsed: Region = serde_json::from_str(&format!("\"{word}\""))
                .unwrap_or_else(|e| panic!("`{word}` must parse back as a region: {e}"));
            assert_eq!(parsed, region);
            // And it extracts without panicking on a degenerate screen.
            let _ = extract(region, &screen("t", &buf));
        }
        // Every variant is listed exactly once.
        let mut seen: Vec<&str> = Region::ALL.iter().map(|r| r.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), Region::ALL.len());
    }

    #[test]
    fn title_region_is_the_title() {
        let buf = lines(&["a", "b"]);
        let got = extract(Region::Title, &screen("\u{2802} claude", &buf));
        assert_eq!(got, vec!["\u{2802} claude"]);
    }

    #[test]
    fn viewport_region_is_every_line() {
        let buf = lines(&["a", "", "b"]);
        assert_eq!(
            extract(Region::Viewport, &screen("", &buf)),
            vec!["a", "", "b"]
        );
    }

    #[test]
    fn bottom_lines_takes_last_six_non_empty_in_screen_order() {
        let buf = lines(&["1", "", "2", "3", "", "4", "5", "6", "7", "8", ""]);
        let got = extract(Region::BottomLines, &screen("", &buf));
        assert_eq!(got, vec!["3", "4", "5", "6", "7", "8"]);
    }

    #[test]
    fn bottom_lines_on_short_screen_takes_what_exists() {
        let buf = lines(&["only"]);
        assert_eq!(
            extract(Region::BottomLines, &screen("", &buf)),
            vec!["only"]
        );
    }

    #[test]
    fn is_rule_needs_width_and_pure_rule_chars() {
        assert!(is_rule("────────"));
        assert!(is_rule("  ━━━━━━━━━━  "));
        assert!(!is_rule("───"), "too short");
        assert!(!is_rule("──── x ────"), "not pure");
        assert!(!is_rule(""));
    }

    #[test]
    fn after_last_rule_slices_below_the_final_rule() {
        let buf = lines(&["head", "────────", "mid", "════════", "tail-a", "tail-b"]);
        let got = extract(Region::AfterLastRule, &screen("", &buf));
        assert_eq!(got, vec!["tail-a", "tail-b"]);
    }

    /// No rule on screen means no live-chrome region — NOT the whole viewport.
    /// Widening here would let a `"Do you want to proceed?"` that an agent
    /// merely printed into its transcript satisfy a rule scoped to the live
    /// chrome, which is the false `blocked` the region model exists to prevent.
    #[test]
    fn after_last_rule_without_a_rule_is_empty_not_the_whole_viewport() {
        let buf = lines(&["Do you want to proceed?", "1. Yes"]);
        assert!(
            extract(Region::AfterLastRule, &screen("", &buf)).is_empty(),
            "no rule, no region — fail safe rather than widen",
        );
    }

    /// Claude Code fences its input box with horizontal rules rather than
    /// drawing a box. Captured from 2.1.207.
    #[test]
    fn prompt_box_reads_a_rule_delimited_box() {
        let buf = lines(&[
            "  transcript",
            "────────────────",
            "\u{276f} hello",
            "────────────────",
            "  Opus 4.8  \u{2387} main",
            "  -- INSERT --",
        ]);
        let got = extract(Region::PromptBox, &screen("", &buf));
        assert_eq!(got, vec!["\u{276f} hello"]);
    }

    /// A lone rule is a separator, not a box. The permission dialog has exactly
    /// one rule above it; reading its body as "the prompt box" would be wrong.
    #[test]
    fn prompt_box_needs_two_fences_and_a_lone_rule_is_not_a_box() {
        let buf = lines(&[
            "  transcript",
            "────────────────",
            " Do you want to proceed?",
        ]);
        assert!(extract(Region::PromptBox, &screen("", &buf)).is_empty());
    }

    /// Boxes here are drawn at a REALISTIC width (>= `RULE_MIN_WIDTH`), because
    /// a narrow box is a different code path: a box border is made entirely of
    /// rule characters, so at 8 columns or more it satisfies `is_rule` as well
    /// as `is_box_line`. A 5-char box exercises a branch no production screen
    /// ever reaches.
    #[test]
    fn prompt_box_strips_borders_and_skips_the_hint_row() {
        let buf = lines(&[
            "transcript line",
            "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{2502} > hello           \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
            "  ? for shortcuts",
        ]);
        let got = extract(Region::PromptBox, &screen("", &buf));
        assert_eq!(got, vec!["", "> hello", ""]);
    }

    /// THE false-`blocked` path, and the reason `is_box_line` is tested before
    /// `is_rule`.
    ///
    /// A closed box's bottom border is made entirely of `RULE_CHARS`, so
    /// `is_rule` accepts it. Classifying it as a horizontal fence sent the
    /// extractor searching UPWARD past the box for an "opening fence" — and a
    /// markdown rule the agent had printed into its own transcript served. The
    /// "live prompt box" then contained transcript lines, so a blocked-asserting
    /// prompt-box rule (a question stem plus numbered options — exactly the
    /// shape of the shipped permission-dialog rule) would fire on a question the
    /// agent merely PRINTED. A permanently-red pane: the one failure mode
    /// ADR-0046 §D forbids, arrived at through the very region the region model
    /// exists to protect.
    ///
    /// A TITLED top border (`╭─ Input ─╮`, the ratatui default, and what codex /
    /// aider / gemini-cli draw) is what makes it bite: an untitled one is itself
    /// `is_rule`, so the upward search stops on it by luck.
    #[test]
    fn prompt_box_reads_a_titled_box_as_a_box_not_a_rule_fence() {
        let buf = lines(&[
            "assistant text",
            // A markdown horizontal rule the agent rendered into its TRANSCRIPT.
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            // Transcript text the agent merely PRINTED. Not a live prompt.
            "  Do you want to proceed?",
            "  \u{276f} 1. Yes",
            "    2. No",
            "  more prose",
            // The live input box, with a labelled top border.
            "\u{256d}\u{2500} Input \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{2502} \u{276f}                \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
            "  ? for shortcuts",
        ]);
        let got = extract(Region::PromptBox, &screen("", &buf));
        assert_eq!(
            got,
            vec!["", "\u{276f}", ""],
            "the region is the live box body and nothing else",
        );
        assert!(
            !got.iter()
                .any(|l| l.contains("proceed") || l.contains("Yes")),
            "transcript text must never appear inside the live prompt box: {got:?}",
        );
        assert!(
            !got.iter().any(|l| l.contains("Input")),
            "and neither must the border's own label: {got:?}",
        );
    }

    /// The rule-delimited branch must not fence itself against a BOX border
    /// either: a transcript box sitting above Claude's rule-fenced input box
    /// would otherwise splice the rows between them into the region.
    #[test]
    fn prompt_box_does_not_fence_a_rule_delimited_body_against_a_transcript_box() {
        let buf = lines(&[
            "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{2502} a rendered diff  \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
            "  Do you want to proceed?",
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            "\u{276f} typing here",
        ]);
        // The bottom-most fence is a LONE rule (no opening fence above it that
        // is not a box), so there is no rule-delimited box at all.
        assert!(
            extract(Region::PromptBox, &screen("", &buf)).is_empty(),
            "no box, no region — fail safe rather than swallow the transcript",
        );
    }

    #[test]
    fn prompt_box_is_empty_when_no_box_is_present() {
        let buf = lines(&["just", "plain", "text"]);
        assert!(extract(Region::PromptBox, &screen("", &buf)).is_empty());
    }

    #[test]
    fn prompt_box_gives_up_past_the_trailing_slack() {
        // Six non-box rows below the box: further than the live chrome
        // could plausibly be, so the box in the transcript is NOT the
        // prompt box.
        let buf = lines(&[
            "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{2502} old diff          \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
            "a",
            "b",
            "c",
            "d",
            "e",
        ]);
        assert!(extract(Region::PromptBox, &screen("", &buf)).is_empty());
    }

    #[test]
    fn prompt_box_picks_the_bottom_most_box_not_a_transcript_box() {
        let buf = lines(&[
            "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{2502} old               \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
            "prose in between",
            "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{2502} live              \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
        ]);
        let got = extract(Region::PromptBox, &screen("", &buf));
        assert_eq!(got, vec!["", "live", ""]);
    }
}
