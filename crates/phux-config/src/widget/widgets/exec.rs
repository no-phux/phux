//! `exec` widget — render the output of a user-supplied program
//! (phux-r82.6, `docs/consumers/tui.md` §8.3).
//!
//! The widget itself never runs anything: it renders a **cached** cell
//! strip behind an [`ExecFeed`] handle. The host (the TUI client) walks
//! the composed bar for feeds ([`crate::widget::StatusBar::exec_feeds`]),
//! runs each feed's command on its interval as a bounded child process
//! (`kill_on_drop`, like plugin actions), and pushes captured stdout
//! through [`ExecFeed::apply_output`]. Render therefore never blocks the
//! paint loop — the bar shows the last completed run (empty until the
//! first one lands).
//!
//! `parse-ansi` (default `true`) interprets SGR escape sequences in the
//! output into per-cell [`CellStyle`]s; with it off (and for every
//! non-SGR escape either way) escapes are stripped. Only the first output
//! line renders — the bar is one row.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::widget::{
    Cell, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, reject_unknown_opts,
};

/// Widget kind, used in error messages.
const KIND: &str = "exec";
/// Default run interval per `docs/consumers/tui.md` §8.3.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
/// Interval floor. The bar's repaint tick is 1s, so a faster cadence
/// would only burn child processes without ever being visible.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Shared handle between an [`ExecWidget`] (reader) and the host's
/// interval runner (writer).
///
/// Cheap to clone (`Arc` inside); the widget and every clone see the same
/// cached cells.
#[derive(Debug, Clone)]
pub struct ExecFeed {
    /// Command argv; `argv[0]` is the program. A TOML string command is
    /// pre-resolved to `["/bin/sh", "-c", cmd]` at build time.
    argv: Vec<String>,
    /// Run cadence (floored to 1s).
    interval: Duration,
    /// Whether [`Self::apply_output`] interprets SGR escapes into cell
    /// styles (`true`) or strips them (`false`).
    parse_ansi: bool,
    /// The cached strip [`ExecWidget::render`] returns.
    cells: Arc<Mutex<WidgetCells>>,
}

impl ExecFeed {
    /// Build a feed with an empty cache.
    fn new(argv: Vec<String>, interval: Duration, parse_ansi: bool) -> Self {
        Self {
            argv,
            interval,
            parse_ansi,
            cells: Arc::new(Mutex::new(WidgetCells::from_text(""))),
        }
    }

    /// Command argv the host should execute.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Run cadence the host should schedule at.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// Fold one completed run's stdout into the cached strip: first line
    /// only, SGR-parsed or escape-stripped per `parse-ansi`. Called by the
    /// host's runner task; the paint loop only ever reads.
    pub fn apply_output(&self, stdout: &str) {
        let line = stdout.lines().next().unwrap_or("");
        let cells = if self.parse_ansi {
            parse_ansi_line(line)
        } else {
            WidgetCells::from_text(&strip_escapes(line))
        };
        if let Ok(mut cached) = self.cells.lock() {
            *cached = cells;
        }
    }

    /// Snapshot the cached strip (the widget's render).
    fn snapshot(&self) -> WidgetCells {
        self.cells
            .lock()
            .map_or_else(|_| WidgetCells::from_text(""), |cached| cached.clone())
    }
}

/// `exec` widget: renders the feed's cached output.
#[derive(Debug)]
pub struct ExecWidget {
    feed: ExecFeed,
}

impl ExecWidget {
    /// Construct an `ExecWidget` (and its feed) from resolved options.
    #[must_use]
    pub fn new(argv: Vec<String>, interval: Duration, parse_ansi: bool) -> Self {
        Self {
            feed: ExecFeed::new(argv, interval.max(MIN_INTERVAL), parse_ansi),
        }
    }
}

impl StatusWidget for ExecWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        self.feed.snapshot()
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(self.feed.interval)
    }

    fn exec_feed(&self) -> Option<ExecFeed> {
        Some(self.feed.clone())
    }
}

/// Factory: builds an [`ExecWidget`] from a TOML `opts` map.
///
/// Accepted keys (per `docs/consumers/tui.md` §8.3):
/// - `command` (required) — a string (run via `/bin/sh -c`, so `~` and
///   `$VAR` expand) or a non-empty array of strings (argv, run directly).
/// - `interval` (optional, default `"5s"`) — a duration string
///   (`"500ms"`, `"30s"`, `"2m"`, `"1h"`) or an integer second count.
///   Floored to 1s.
/// - `parse-ansi` (bool, optional, default `true`; `parse_ansi` also
///   accepted) — interpret SGR escapes into cell styles.
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option, when
/// `command` is missing, empty, or wrong-typed, or when `interval` /
/// `parse-ansi` do not parse.
pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(
        KIND,
        opts,
        &["command", "interval", "parse-ansi", "parse_ansi"],
    )?;
    let argv = match opts.get("command") {
        Some(toml::Value::String(s)) if !s.trim().is_empty() => {
            vec!["/bin/sh".to_owned(), "-c".to_owned(), s.clone()]
        }
        Some(toml::Value::Array(items)) if !items.is_empty() => {
            let mut argv = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    toml::Value::String(s) if !s.is_empty() => argv.push(s.clone()),
                    other => {
                        return Err(invalid(format!(
                            "`command` array entries must be non-empty strings, got {}",
                            other.type_str()
                        )));
                    }
                }
            }
            argv
        }
        Some(toml::Value::String(_) | toml::Value::Array(_)) => {
            return Err(invalid("`command` must not be empty".to_owned()));
        }
        Some(other) => {
            return Err(invalid(format!(
                "`command` must be a string or an array of strings, got {}",
                other.type_str()
            )));
        }
        None => return Err(invalid("`command` is required".to_owned())),
    };
    let interval = match opts.get("interval") {
        None => DEFAULT_INTERVAL,
        Some(toml::Value::String(s)) => parse_duration(s)?,
        Some(toml::Value::Integer(n)) if *n > 0 => {
            Duration::from_secs(u64::try_from(*n).unwrap_or(u64::MAX))
        }
        Some(other) => {
            return Err(invalid(format!(
                "`interval` must be a duration string (e.g. \"30s\") or a positive \
                 integer second count, got {}",
                other.type_str()
            )));
        }
    };
    let parse_ansi = match opts.get("parse-ansi").or_else(|| opts.get("parse_ansi")) {
        None => true,
        Some(toml::Value::Boolean(b)) => *b,
        Some(other) => {
            return Err(invalid(format!(
                "`parse-ansi` must be a boolean, got {}",
                other.type_str()
            )));
        }
    };
    Ok(Box::new(ExecWidget::new(argv, interval, parse_ansi)))
}

fn invalid(message: String) -> WidgetError {
    WidgetError::InvalidOption {
        kind: KIND.to_owned(),
        message,
    }
}

/// Parse a duration string: a positive integer followed by `ms`, `s`,
/// `m`, or `h`. A bare integer reads as seconds.
fn parse_duration(s: &str) -> Result<Duration, WidgetError> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (digits, unit) = s.split_at(split);
    let value: u64 = digits
        .parse()
        .map_err(|_| invalid(format!("invalid `interval` duration: {s:?}")))?;
    if value == 0 {
        return Err(invalid("`interval` must be > 0".to_owned()));
    }
    let duration = match unit {
        "ms" => Duration::from_millis(value),
        "" | "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value.saturating_mul(60)),
        "h" => Duration::from_secs(value.saturating_mul(3600)),
        _ => return Err(invalid(format!("invalid `interval` unit in {s:?}"))),
    };
    Ok(duration)
}

// ---------------------------------------------------------------------------
// ANSI / SGR line parsing
// ---------------------------------------------------------------------------

/// Interpret one line of program output into styled cells: SGR (`CSI …
/// m`) sequences update the running [`CellStyle`]; every other escape
/// sequence and control byte is stripped.
#[must_use]
fn parse_ansi_line(line: &str) -> WidgetCells {
    let mut cells = Vec::new();
    let mut style = CellStyle::default();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: collect parameter/intermediate bytes up to the
                    // final byte (0x40..=0x7e). Apply only `m` (SGR).
                    let mut params = String::new();
                    let mut is_sgr = false;
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            is_sgr = n == 'm';
                            break;
                        }
                        params.push(n);
                    }
                    if is_sgr {
                        apply_sgr(&mut style, &params);
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: skip to BEL or ST (ESC \).
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    // Two-char escape (ESC x): drop the introducer + one.
                    // Charset designations (ESC ( X and friends) carry one
                    // more byte.
                    if let Some(n) = chars.next()
                        && matches!(n, '(' | ')' | '*' | '+')
                    {
                        chars.next();
                    }
                }
            }
        } else if !c.is_control() {
            cells.push(Cell {
                text: smallvec::smallvec![c],
                style: (!style.is_plain()).then(|| style.clone()),
                hit: None,
            });
        }
    }
    WidgetCells { cells }
}

/// Strip escape sequences and control bytes without interpreting them.
fn strip_escapes(line: &str) -> String {
    parse_ansi_line(line)
        .cells
        .iter()
        .filter_map(|c| c.text.first())
        .collect()
}

/// Fold one SGR parameter list (the bytes between `CSI` and `m`) into a
/// running [`CellStyle`]. Unknown parameters are ignored.
fn apply_sgr(style: &mut CellStyle, params: &str) {
    let mut iter = params.split(';').map(|p| {
        if p.is_empty() {
            Ok(0)
        } else {
            p.parse::<u16>()
        }
    });
    while let Some(param) = iter.next() {
        let Ok(param) = param else { return };
        match param {
            0 => *style = CellStyle::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            7 => style.reverse = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            27 => style.reverse = false,
            30..=37 => style.fg = Some((param - 30).to_string()),
            38 | 48 => {
                let Some(color) = extended_color(&mut iter) else {
                    return;
                };
                if param == 38 {
                    style.fg = Some(color);
                } else {
                    style.bg = Some(color);
                }
            }
            39 => style.fg = None,
            40..=47 => style.bg = Some((param - 40).to_string()),
            49 => style.bg = None,
            90..=97 => style.fg = Some((param - 90 + 8).to_string()),
            100..=107 => style.bg = Some((param - 100 + 8).to_string()),
            _ => {}
        }
    }
}

/// Consume an extended-color payload after a `38` / `48` parameter:
/// `5;n` (256-color index, rendered as a decimal index string) or
/// `2;r;g;b` (truecolor, rendered as `#rrggbb`). Returns `None` on a
/// malformed payload, which aborts the whole SGR sequence.
fn extended_color<I>(iter: &mut I) -> Option<String>
where
    I: Iterator<Item = Result<u16, std::num::ParseIntError>>,
{
    match iter.next()?.ok()? {
        5 => {
            let n = iter.next()?.ok()?;
            (n <= 255).then(|| n.to_string())
        }
        2 => {
            let r = iter.next()?.ok()?;
            let g = iter.next()?.ok()?;
            let b = iter.next()?.ok()?;
            (r <= 255 && g <= 255 && b <= 255).then(|| format!("#{r:02x}{g:02x}{b:02x}"))
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::schema::WidgetSpec;
    use crate::widget::WidgetRegistry;
    use std::time::UNIX_EPOCH;

    fn build(opts: &[(&str, toml::Value)]) -> Result<Box<dyn StatusWidget>, WidgetError> {
        let spec = WidgetSpec {
            kind: "exec".to_owned(),
            opts: opts
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        };
        WidgetRegistry::with_builtins().build(&spec)
    }

    fn text_of(cells: &WidgetCells) -> String {
        cells.cells.iter().filter_map(|c| c.text.first()).collect()
    }

    fn ctx() -> WidgetContext<'static> {
        WidgetContext::new(UNIX_EPOCH, "", "C-a", &[])
    }

    #[test]
    fn string_command_resolves_to_sh_dash_c() {
        let w = build(&[("command", toml::Value::String("echo hi".to_owned()))]).unwrap();
        let feed = w.exec_feed().expect("exec widget exposes a feed");
        assert_eq!(feed.argv(), ["/bin/sh", "-c", "echo hi"]);
        assert_eq!(feed.interval(), Duration::from_secs(5), "doc default 5s");
    }

    #[test]
    fn array_command_is_argv_verbatim() {
        let w = build(&[(
            "command",
            toml::Value::Array(vec![
                toml::Value::String("battery".to_owned()),
                toml::Value::String("--percent".to_owned()),
            ]),
        )])
        .unwrap();
        let feed = w.exec_feed().unwrap();
        assert_eq!(feed.argv(), ["battery", "--percent"]);
    }

    #[test]
    fn interval_parses_units_and_floors_to_one_second() {
        for (raw, want) in [
            ("30s", Duration::from_secs(30)),
            ("2m", Duration::from_secs(120)),
            ("1h", Duration::from_secs(3600)),
            ("7", Duration::from_secs(7)),
            // Sub-second cadences floor to the 1s repaint tick.
            ("200ms", Duration::from_secs(1)),
        ] {
            let w = build(&[
                ("command", toml::Value::String("true".to_owned())),
                ("interval", toml::Value::String(raw.to_owned())),
            ])
            .unwrap();
            assert_eq!(w.exec_feed().unwrap().interval(), want, "interval {raw:?}");
            assert_eq!(w.poll_interval(), Some(want));
        }
    }

    #[test]
    fn invalid_options_are_rejected() {
        for opts in [
            vec![],                                                  // no command
            vec![("command", toml::Value::String("  ".to_owned()))], // blank
            vec![("command", toml::Value::Array(vec![]))],           // empty argv
            vec![("command", toml::Value::Integer(3))],              // wrong type
            vec![
                ("command", toml::Value::String("true".to_owned())),
                ("interval", toml::Value::String("fast".to_owned())),
            ],
            vec![
                ("command", toml::Value::String("true".to_owned())),
                ("interval", toml::Value::String("0s".to_owned())),
            ],
            vec![
                ("command", toml::Value::String("true".to_owned())),
                ("parse-ansi", toml::Value::String("yes".to_owned())),
            ],
        ] {
            assert!(
                matches!(build(&opts), Err(WidgetError::InvalidOption { .. })),
                "opts {opts:?} should be rejected"
            );
        }
    }

    #[test]
    fn render_is_empty_until_output_arrives_then_shows_cached_line() {
        let w = build(&[("command", toml::Value::String("true".to_owned()))]).unwrap();
        assert!(w.render(&ctx()).is_empty(), "no run yet -> empty strip");
        let feed = w.exec_feed().unwrap();
        feed.apply_output("BAT 87%\nsecond line ignored\n");
        assert_eq!(text_of(&w.render(&ctx())), "BAT 87%");
        // A later run replaces the cache.
        feed.apply_output("BAT 86%\n");
        assert_eq!(text_of(&w.render(&ctx())), "BAT 86%");
    }

    #[test]
    fn parse_ansi_styles_cells_from_sgr() {
        let cells = parse_ansi_line("\u{1b}[1;31mA\u{1b}[0mB");
        assert_eq!(text_of(&cells), "AB");
        let a = cells.cells[0].style.clone().expect("A styled");
        assert!(a.bold);
        assert_eq!(a.fg.as_deref(), Some("1"));
        assert!(cells.cells[1].style.is_none(), "B reset to plain");
    }

    #[test]
    fn parse_ansi_extended_colors() {
        let cells = parse_ansi_line("\u{1b}[38;5;208mX\u{1b}[48;2;16;32;48mY");
        assert_eq!(
            cells.cells[0].style.as_ref().unwrap().fg.as_deref(),
            Some("208")
        );
        let y = cells.cells[1].style.as_ref().unwrap();
        assert_eq!(y.bg.as_deref(), Some("#102030"));
        assert_eq!(y.fg.as_deref(), Some("208"), "fg persists across cells");
    }

    #[test]
    fn parse_ansi_bright_and_reset_params() {
        let cells = parse_ansi_line("\u{1b}[97;100mZ\u{1b}[39;49mQ");
        let z = cells.cells[0].style.as_ref().unwrap();
        assert_eq!(z.fg.as_deref(), Some("15"), "97 = bright white = idx 15");
        assert_eq!(z.bg.as_deref(), Some("8"), "100 = bright black bg = idx 8");
        assert!(cells.cells[1].style.is_none(), "39;49 clears both colors");
    }

    #[test]
    fn parse_ansi_strips_non_sgr_escapes_and_controls() {
        // Cursor-move CSI, an OSC title, a two-char escape, and a tab all
        // strip; printable text survives.
        let cells = parse_ansi_line("\u{1b}[2Ka\u{1b}]0;title\u{7}b\u{1b}(Bc\td");
        assert_eq!(text_of(&cells), "abcd");
        assert!(cells.cells.iter().all(|c| c.style.is_none()));
    }

    #[test]
    fn parse_ansi_disabled_strips_sgr_instead_of_styling() {
        let w = build(&[
            ("command", toml::Value::String("true".to_owned())),
            ("parse-ansi", toml::Value::Boolean(false)),
        ])
        .unwrap();
        let feed = w.exec_feed().unwrap();
        feed.apply_output("\u{1b}[31mred\u{1b}[0m\n");
        let cells = w.render(&ctx());
        assert_eq!(text_of(&cells), "red");
        assert!(cells.cells.iter().all(|c| c.style.is_none()));
    }

    #[test]
    fn status_bar_exec_feeds_walks_all_slots() {
        use crate::schema::{StatusCfg, StatusPosition, Widget};
        use crate::widget::StatusBar;
        let exec = |cmd: &str| {
            Widget::Spec(WidgetSpec {
                kind: "exec".to_owned(),
                opts: std::iter::once(("command".to_owned(), toml::Value::String(cmd.to_owned())))
                    .collect(),
            })
        };
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".to_owned()), exec("left")],
            center: vec![exec("center")],
            right: vec![exec("right")],
            position: StatusPosition::default(),
        };
        let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).unwrap();
        let feeds = bar.exec_feeds();
        let cmds: Vec<&str> = feeds.iter().map(|f| f.argv()[2].as_str()).collect();
        assert_eq!(cmds, ["left", "center", "right"]);
    }
}
