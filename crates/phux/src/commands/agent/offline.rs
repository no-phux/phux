//! `phux agent explain --file`: evaluate the compiled detection manifests
//! against a captured screen, with no server in the loop (ADR-0046).
//!
//! # Why this verb exists
//!
//! ADR-0046's Tradeoffs section records the most expensive mistake made
//! building the detector: the first Claude manifest was written against an
//! imagined TUI. Every screen rule matched nothing in the shipped CLI.
//! Nothing failed loudly, because `idle` is the detector's fail-safe, and the
//! unit tests went green because they fed the matcher the same invented
//! screens the rules had been derived from. Three rules were deleted.
//!
//! The blocker on writing more manifests is not effort; it is that a rule is
//! unverifiable without a real captured viewport and a way to see what the
//! rules did to it. This is that. The output leads with **what each region
//! resolved to**, because a rule scoped to a region that comes back empty
//! cannot match however well-written its predicate is, and an empty region is
//! invisible from every other vantage point.
//!
//! # No server, no runtime
//!
//! The rules engine is compiled into the binary (`phux_server::agent_explain`
//! is a facade over it), so this path allocates no tokio runtime and opens no
//! socket. It works on a machine with no phux running.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_server::agent_explain::{self, Capture, EvaluatedRule, Explanation, PredicateEvidence};

use crate::commands::json_err::{self, CliError, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// Schema version of the offline explanation document.
const SCHEMA_VERSION: u8 = 1;

/// Rows of a region preview printed before eliding. A `viewport` region is a
/// whole screen; printing all of it buries the narrow regions that are the
/// point. The JSON form is never elided.
const PREVIEW_ROWS: usize = 12;

/// Run the offline explainer. `format` is `auto` (the default), `json`, or
/// `text`.
///
/// The OSC title comes from the capture itself when it carries one — a
/// `phux snapshot --json` document does, since ADR-0077 added
/// `ScreenState::title` — and `title` overrides it. A plain-text capture has
/// nowhere to put a title, so there `--title` is the only way to exercise
/// title-scoped rules. That distinction matters more than it looks: the
/// detector ranks by `(is_title, priority, idx)`, so a title rule beats every
/// screen rule regardless of priority, and silently blanking the region would
/// misreport every title-derived rule in the manifest (phux-w7z2.41).
pub(super) fn run(
    path: &Path,
    kind: Option<&str>,
    title: Option<&str>,
    format: Option<&str>,
    json: bool,
) -> ExitCode {
    let raw = match read_capture(path) {
        Ok(raw) => raw,
        Err(err) => return json_err::emit(json, &err, EXIT_FAILURE),
    };
    let parsed = match parse_capture(&raw, format.unwrap_or("auto")) {
        Ok(parsed) => parsed,
        Err(err) => return json_err::emit(json, &err, EXIT_FAILURE),
    };
    let ParsedCapture {
        lines,
        title: captured_title,
        source,
    } = parsed;
    let kind = match resolve_kind(kind) {
        Ok(kind) => kind,
        Err(err) => return json_err::emit(json, &err, EXIT_USAGE),
    };
    let (title, title_origin) = resolve_title(title, captured_title);
    let capture = Capture { title, lines };
    let Some(explanation) = agent_explain::explain(&kind, &capture) else {
        // `resolve_kind` already proved a manifest exists, so this is
        // unreachable in practice; it stays an error rather than an unwrap.
        return json_err::emit(
            json,
            &CliError::new(
                codes::INTERNAL_ERROR,
                format!("manifest for `{kind}` disappeared between lookup and evaluation"),
                "re-run; if it persists this is a bug in phux",
            ),
            EXIT_FAILURE,
        );
    };

    if json {
        emit_json(path, &source, title_origin, &capture, &explanation)
    } else {
        emit_prose(path, &source, title_origin, &capture, &explanation);
        ExitCode::SUCCESS
    }
}

/// How the capture was read, for the report's provenance line.
#[derive(Debug)]
struct Source {
    /// `json` or `text` — what the bytes were actually parsed as.
    format: &'static str,
    /// Grid width, when the capture declared one.
    cols: Option<u16>,
}

/// A capture, parsed: the viewport rows, the title the document carried, and
/// how the bytes were read.
#[derive(Debug)]
struct ParsedCapture {
    /// Viewport rows, top to bottom.
    lines: Vec<String>,
    /// The `ScreenState::title` the document carried (ADR-0077). Always
    /// `None` for a text capture, which has nowhere to put one, and for a
    /// JSON capture written by a producer that predates the field.
    title: Option<String>,
    /// Provenance for the report's header line.
    source: Source,
}

/// Where the evaluated OSC title came from, for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleOrigin {
    /// `--title` supplied it, overriding whatever the capture carried.
    Flag,
    /// The capture document carried it (`ScreenState::title`).
    Capture,
    /// Nothing supplied one: every `title`-scoped rule sees an empty region.
    None,
}

impl TitleOrigin {
    /// The word the JSON document reports.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Capture => "capture",
            Self::None => "none",
        }
    }
}

/// Pick the title to evaluate title-scoped rules against.
///
/// `--title` wins so a capture can be re-explained under a hypothetical
/// title, which is how a title rule gets authored. Otherwise the capture's
/// own title is used — dropping it was phux-w7z2.41, and it made every
/// title-scoped rule evaluate against an empty region even though the title
/// was sitting in the file. An empty string from either source is "no title":
/// a pane that set no title and a pane that set an empty one are the same
/// thing to a region matcher.
fn resolve_title(flag: Option<&str>, captured: Option<String>) -> (String, TitleOrigin) {
    if let Some(title) = flag.filter(|title| !title.is_empty()) {
        return (title.to_owned(), TitleOrigin::Flag);
    }
    captured.filter(|title| !title.is_empty()).map_or_else(
        || (String::new(), TitleOrigin::None),
        |title| (title, TitleOrigin::Capture),
    )
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Read the capture from `path`, or from stdin when it is `-`.
fn read_capture(path: &Path) -> Result<String, CliError> {
    if path == Path::new("-") {
        return std::io::read_to_string(std::io::stdin()).map_err(|err| {
            CliError::new(
                codes::CAPTURE_UNREADABLE,
                format!("could not read the capture from stdin: {err}"),
                "pipe a screen in, e.g. `phux snapshot --json | phux agent explain --file - \
                 --kind claude`",
            )
        });
    }
    std::fs::read_to_string(path).map_err(|err| {
        CliError::new(
            codes::CAPTURE_UNREADABLE,
            format!("could not read the capture at {}: {err}", path.display()),
            "capture one with `phux snapshot --json > screen.json`, or pass `-` to read stdin",
        )
    })
}

/// Parse a capture into viewport rows plus whatever else the document
/// carried.
///
/// Two shapes are accepted, because two shapes are what people have: the
/// `phux snapshot --json` document (`ScreenState`, ADR-0022) and a plain text
/// screen, one viewport row per line — which is what a human pastes and what
/// the committed goldens under `crates/phux-server/src/agent_detect/fixtures/`
/// already are.
///
/// The JSON form carries `title` since ADR-0077, so it is read here rather
/// than left to `--title`; the text form has no room for one and returns
/// `None`, which is what makes "the region really was empty" still reportable.
fn parse_capture(raw: &str, format: &str) -> Result<ParsedCapture, CliError> {
    let looks_json = raw.trim_start().starts_with('{');
    let as_json = match format {
        "json" => true,
        "text" => false,
        // "auto", and anything clap would have rejected already.
        _ => looks_json,
    };

    let (lines, cols, title) = if as_json {
        let screen: phux_core::screen::ScreenState = serde_json::from_str(raw).map_err(|err| {
            CliError::new(
                codes::CAPTURE_INVALID,
                format!("the capture is not a `phux snapshot --json` screen: {err}"),
                "re-capture with `phux snapshot --json`, or pass `--format text` for a \
                     plain screen dump",
            )
        })?;
        (screen.lines, Some(screen.cols), screen.title)
    } else {
        (
            raw.lines().map(str::to_owned).collect::<Vec<_>>(),
            None,
            None,
        )
    };

    if lines.iter().all(|line| line.trim().is_empty()) {
        return Err(CliError::new(
            codes::CAPTURE_INVALID,
            "the capture has no non-empty rows".to_owned(),
            "capture a screen with content on it: `phux snapshot --json > screen.json`",
        ));
    }

    Ok(ParsedCapture {
        lines,
        title,
        source: Source {
            format: if as_json { "json" } else { "text" },
            cols,
        },
    })
}

/// Resolve `--kind` against the loaded manifests, accepting a binary alias.
///
/// Offline the detector's own identification step is unavailable: it reads
/// the PTY's foreground process group, and a file has no process. So the kind
/// is required, and the miss enumerates what is actually loaded — including
/// any operator override, so a manifest that failed to compile shows up as an
/// absence here instead of a `warn` line in a server log nobody read.
fn resolve_kind(kind: Option<&str>) -> Result<String, CliError> {
    let available = agent_explain::kinds();
    let roster = if available.is_empty() {
        "no manifests are loaded (is PHUX_AGENT_DETECT=0 set?)".to_owned()
    } else {
        format!("known kinds: {}", available.join(", "))
    };
    let Some(kind) = kind else {
        return Err(CliError::new(
            codes::UNKNOWN_AGENT_KIND,
            "--kind is required with --file: offline there is no foreground process group to \
             identify the agent from"
                .to_owned(),
            roster,
        ));
    };
    agent_explain::resolve_kind(kind).ok_or_else(|| {
        CliError::new(
            codes::UNKNOWN_AGENT_KIND,
            format!("no detection manifest for `{kind}`"),
            format!(
                "{roster}; drop a manifest in $PHUX_AGENT_RULES_DIR (else \
                 $XDG_CONFIG_HOME/phux/agent-rules) to add one"
            ),
        )
    })
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn emit_json(
    path: &Path,
    source: &Source,
    title_origin: TitleOrigin,
    capture: &Capture,
    explanation: &Explanation,
) -> ExitCode {
    let document = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "capture": {
            "path": display_path(path),
            "format": source.format,
            "rows": capture.lines.len(),
            "cols": source.cols,
            "title": capture.title,
            // Additive since phux-w7z2.41: `flag`, `capture`, or `none`. A
            // title-scoped rule that missed reads very differently depending
            // on which of the three produced the region it read.
            "title_source": title_origin.as_str(),
        },
        "explain": explanation,
    });
    match serde_json::to_string_pretty(&document) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &CliError::new(
                codes::JSON_SERIALIZE,
                format!("could not render the explanation as JSON: {err}"),
                "re-run without --json",
            ),
            EXIT_FAILURE,
        ),
    }
}

fn emit_prose(
    path: &Path,
    source: &Source,
    title_origin: TitleOrigin,
    capture: &Capture,
    explanation: &Explanation,
) {
    let cols = source
        .cols
        .map_or_else(String::new, |cols| format!(", {cols} cols"));
    outln!(
        "capture  {} ({}, {} rows{cols})",
        display_path(path),
        source.format,
        capture.lines.len(),
    );
    outln!("kind     {} ({})", explanation.kind, explanation.name);
    match title_origin {
        TitleOrigin::Flag => outln!("title    {:?}  (from --title)", capture.title),
        TitleOrigin::Capture => outln!("title    {:?}  (from the capture)", capture.title),
        TitleOrigin::None => {
            outln!("title    <none — every `title` rule sees an empty region>");
            if source.format == "text" {
                outln!("         a text capture carries no title; pass --title to supply one");
            }
        }
    }

    outln!();
    match (&explanation.state, &explanation.matched_rule) {
        (Some(state), Some(rule)) => {
            outln!("verdict  {state}  (rule `{rule}`)");
        }
        _ => outln!("verdict  {}", explanation.detector_state),
    }
    if let Some(reason) = &explanation.fallback_reason {
        outln!("         {reason}");
    }
    let flags = positive_flags(explanation);
    if !flags.is_empty() {
        outln!("         positive evidence: {}", flags.join(", "));
    }

    outln!();
    outln!("regions  (what each region resolved to on THIS screen)");
    for region in &explanation.regions {
        if region.empty {
            outln!(
                "  {:<16} EMPTY — no rule scoped here can match",
                region.region
            );
            continue;
        }
        let count = region.lines.len();
        let plural = if count == 1 { "" } else { "s" };
        outln!("  {:<16} {count} line{plural}", region.region);
        for line in region.lines.iter().take(PREVIEW_ROWS) {
            outln!("    | {line}");
        }
        if region.lines.len() > PREVIEW_ROWS {
            outln!(
                "    | ... {} more (use --json for all of them)",
                region.lines.len() - PREVIEW_ROWS,
            );
        }
    }

    let matched = explanation
        .evaluated_rules
        .iter()
        .filter(|rule| rule.matched)
        .count();
    outln!();
    outln!(
        "rules    ({} evaluated, {matched} matched)",
        explanation.evaluated_rules.len(),
    );
    for rule in &explanation.evaluated_rules {
        print_rule(rule);
    }
}

fn print_rule(rule: &EvaluatedRule) {
    let mark = if rule.matched { "[match]" } else { "[ miss]" };
    let state = rule.state.as_deref().unwrap_or("-");
    let mut flags = Vec::new();
    if rule.visible_blocker {
        flags.push("visible-blocker");
    }
    if rule.visible_idle {
        flags.push("visible-idle");
    }
    if rule.visible_working {
        flags.push("visible-working");
    }
    if rule.skip_state_update {
        flags.push("skip-state-update");
    }
    let flags = if flags.is_empty() {
        String::new()
    } else {
        format!("  {}", flags.join(" "))
    };
    outln!(
        "  {mark} {:<28} {state:<8} p{:<4} {}{flags}",
        rule.id,
        rule.priority,
        rule.region,
    );
    print_evidence(&rule.evidence, 4);
}

fn print_evidence(node: &PredicateEvidence, indent: usize) {
    let pad = " ".repeat(indent);
    let mark = if node.matched { "+" } else { "-" };
    match &node.pattern {
        Some(pattern) => outln!("{pad}{mark} {} {pattern:?}", node.op),
        None => outln!("{pad}{mark} {}", node.op),
    }
    for child in &node.children {
        print_evidence(child, indent + 2);
    }
}

fn positive_flags(explanation: &Explanation) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if explanation.visible_blocker {
        flags.push("visible-blocker");
    }
    if explanation.visible_idle {
        flags.push("visible-idle");
    }
    if explanation.visible_working {
        flags.push("visible-working");
    }
    if explanation.freeze {
        flags.push("skip-state-update");
    }
    flags
}

fn display_path(path: &Path) -> String {
    if path == Path::new("-") {
        "<stdin>".to_owned()
    } else {
        PathBuf::from(path).display().to_string()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{TitleOrigin, parse_capture, resolve_kind, resolve_title};

    /// A REAL committed golden: the captured Claude Code permission dialog
    /// the detector itself is pinned against. Not a screen written for this
    /// test — a fixture invented here would test the parser against itself,
    /// which is the exact failure ADR-0046 records.
    const CLAUDE_BLOCKED: &str = include_str!(
        "../../../../phux-server/src/agent_detect/fixtures/claude/blocked_permission.txt"
    );

    /// A `ScreenState` document with a `title`, as `phux snapshot --json`
    /// writes it since ADR-0077.
    fn json_capture(title: Option<&str>) -> String {
        let document = serde_json::json!({
            "schema_version": 1,
            "pane": 7,
            "cols": 120,
            "rows": 3,
            "cursor": null,
            "lines": ["one", "two", "three"],
            "title": title,
        });
        serde_json::to_string(&document).expect("serialize")
    }

    #[test]
    fn a_plain_text_capture_parses_as_one_row_per_line() {
        let parsed = parse_capture(CLAUDE_BLOCKED, "auto").expect("text capture parses");
        assert_eq!(parsed.source.format, "text");
        assert_eq!(parsed.source.cols, None);
        assert_eq!(parsed.lines.len(), CLAUDE_BLOCKED.lines().count());
        assert!(parsed.lines.iter().any(|l| l.contains("Do you want")));
    }

    #[test]
    fn a_snapshot_json_capture_parses_and_carries_the_grid_width() {
        let raw = json_capture(None);
        let parsed = parse_capture(&raw, "auto").expect("json capture parses");
        assert_eq!(parsed.source.format, "json");
        assert_eq!(parsed.source.cols, Some(120));
        assert_eq!(parsed.lines, vec!["one", "two", "three"]);
    }

    /// phux-w7z2.41: the title is right there in the document, and the
    /// detector ranks title rules above every screen rule — dropping it made
    /// `agent explain --file` misreport every title-derived rule.
    #[test]
    fn a_json_capture_carries_its_osc_title_without_the_flag() {
        let raw = json_capture(Some("claude — ~/repo"));
        let parsed = parse_capture(&raw, "auto").expect("json capture parses");
        assert_eq!(parsed.title.as_deref(), Some("claude — ~/repo"));

        let (title, origin) = resolve_title(None, parsed.title);
        assert_eq!(title, "claude — ~/repo");
        assert_eq!(origin, TitleOrigin::Capture);
    }

    /// A text capture genuinely has nowhere to put a title, so the report has
    /// to keep saying the region was empty rather than inventing one.
    #[test]
    fn a_text_capture_still_needs_the_title_flag() {
        let parsed = parse_capture(CLAUDE_BLOCKED, "text").expect("text capture parses");
        assert_eq!(parsed.title, None);

        let (title, origin) = resolve_title(None, parsed.title);
        assert!(title.is_empty());
        assert_eq!(origin, TitleOrigin::None);
    }

    /// `--title` stays the override, which is how a title rule gets authored
    /// against a capture taken before the rule existed.
    #[test]
    fn an_explicit_title_overrides_the_capture() {
        let raw = json_capture(Some("from the capture"));
        let parsed = parse_capture(&raw, "auto").expect("json capture parses");

        let (title, origin) = resolve_title(Some("hypothetical"), parsed.title);
        assert_eq!(title, "hypothetical");
        assert_eq!(origin, TitleOrigin::Flag);
    }

    /// A pane that set an empty title and a pane that set none are the same
    /// thing to a region matcher, from either source.
    #[test]
    fn an_empty_title_from_either_source_is_no_title() {
        assert_eq!(resolve_title(Some(""), None).1, TitleOrigin::None);
        assert_eq!(
            resolve_title(Some(""), Some(String::new())).1,
            TitleOrigin::None
        );
        assert_eq!(
            resolve_title(Some(""), Some("real".to_owned())),
            ("real".to_owned(), TitleOrigin::Capture)
        );
    }

    /// `--format text` overrides the sniffer, so a screen that happens to
    /// start with `{` is still read as a screen.
    #[test]
    fn an_explicit_format_overrides_the_sniffer() {
        let raw = "{ this is a shell prompt, not JSON\nsecond row";
        let parsed = parse_capture(raw, "text").expect("forced text parses");
        assert_eq!(parsed.source.format, "text");
        assert_eq!(parsed.lines.len(), 2);

        let err = parse_capture(raw, "json").expect_err("forced json must fail");
        assert_eq!(err.code, "capture_invalid");
    }

    #[test]
    fn an_empty_capture_is_rejected_rather_than_explained() {
        let err = parse_capture("\n   \n\n", "text").expect_err("blank capture");
        assert_eq!(err.code, "capture_invalid");
        assert!(err.message.contains("no non-empty rows"));
    }

    /// Offline the kind cannot be inferred, so omitting it is an error that
    /// names the roster rather than a silent guess.
    #[test]
    fn a_missing_kind_reports_the_available_roster() {
        let err = resolve_kind(None).expect_err("kind is required offline");
        assert_eq!(err.code, "unknown_agent_kind");
        assert!(err.message.contains("--kind is required"));
        assert!(err.remedy.contains("claude"), "roster: {}", err.remedy);
    }

    #[test]
    fn a_binary_alias_resolves_to_its_kind() {
        assert_eq!(
            resolve_kind(Some("claude-code")).ok(),
            Some("claude".into())
        );
        assert_eq!(resolve_kind(Some("claude")).ok(), Some("claude".into()));
        let err = resolve_kind(Some("nope")).expect_err("unknown kind");
        assert_eq!(err.code, "unknown_agent_kind");
        assert!(err.remedy.contains("PHUX_AGENT_RULES_DIR"));
    }
}
