//! `exec` widget — the first output line of a user-supplied program
//! (`docs/status-bar.md#widget-kinds`).
//!
//! The widget never runs anything: the host runs each [`ExecFeed`]'s command
//! on its interval and pushes stdout through [`ExecFeed::apply_output`], so
//! render only reads a cached strip and never blocks the paint loop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phux_config::widget::kinds::exec::{ExecOpts, parse};

use super::{Cell, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// The bar repaints once a second, so a faster cadence is never visible.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Shared handle between an [`ExecWidget`] (reader) and the host's interval
/// runner (writer); clones share one cache.
#[derive(Debug, Clone)]
pub struct ExecFeed {
    /// Command argv; a TOML string command resolves to `/bin/sh -c`.
    argv: Vec<String>,
    interval: Duration,
    /// Interpret SGR escapes into cell styles, or strip them.
    parse_ansi: bool,
    cells: Arc<Mutex<WidgetCells>>,
}

impl ExecFeed {
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
    /// only, SGR-parsed or escape-stripped per `parse-ansi`.
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

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let ExecOpts {
        argv,
        interval,
        parse_ansi,
    } = parse(opts)?;
    Ok(Box::new(ExecWidget::new(argv, interval, parse_ansi)))
}

// ---------------------------------------------------------------------------
// ANSI / SGR line parsing
// ---------------------------------------------------------------------------

/// Interpret one output line into styled cells: SGR sequences update the
/// running [`CellStyle`]; every other escape and control byte is stripped.
fn parse_ansi_line(line: &str) -> WidgetCells {
    let mut cells = Vec::new();
    let mut style = CellStyle::default();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ESC {
            consume_escape(&mut chars, &mut style);
            continue;
        }
        if c.is_control() {
            continue;
        }
        cells.push(Cell {
            text: smallvec::smallvec![c],
            style: (!style.is_plain()).then(|| style.clone()),
            hit: None,
        });
    }
    WidgetCells { cells }
}

const ESC: char = '\u{1b}';
const BEL: char = '\u{7}';

type LineChars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Consume the escape sequence that follows an ESC: an SGR sequence folds
/// into `style`; anything else is discarded.
fn consume_escape(chars: &mut LineChars<'_>, style: &mut CellStyle) {
    match chars.peek() {
        Some('[') => {
            chars.next();
            if let Some(params) = read_csi_sgr_params(chars) {
                apply_sgr(style, &params);
            }
        }
        Some(']') => {
            chars.next();
            skip_osc(chars);
        }
        _ => skip_short_escape(chars),
    }
}

/// Read a CSI's parameters through its final byte, returning them only when
/// that final byte is `m` (SGR). An unterminated CSI yields `None`.
fn read_csi_sgr_params(chars: &mut LineChars<'_>) -> Option<String> {
    let mut params = String::new();
    for n in chars.by_ref() {
        if is_csi_final_byte(n) {
            return (n == 'm').then_some(params);
        }
        params.push(n);
    }
    None
}

fn is_csi_final_byte(c: char) -> bool {
    ('\u{40}'..='\u{7e}').contains(&c)
}

/// Skip an OSC body through its terminator: BEL or ST (ESC `\`).
fn skip_osc(chars: &mut LineChars<'_>) {
    while let Some(n) = chars.next() {
        if n == BEL {
            return;
        }
        if n == ESC && chars.peek() == Some(&'\\') {
            chars.next();
            return;
        }
    }
}

/// Skip a two-char escape; charset designations carry one more char.
fn skip_short_escape(chars: &mut LineChars<'_>) {
    if let Some(n) = chars.next()
        && matches!(n, '(' | ')' | '*' | '+')
    {
        chars.next();
    }
}

/// Strip escape sequences and control bytes without interpreting them.
fn strip_escapes(line: &str) -> String {
    parse_ansi_line(line)
        .cells
        .iter()
        .filter_map(|c| c.text.first())
        .collect()
}

/// Fold one SGR parameter list into a running [`CellStyle`], ignoring
/// unknown parameters.
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

/// Consume an extended-color payload after `38` / `48`: `5;n` (256-color
/// index) or `2;r;g;b` (`#rrggbb`). `None` aborts the whole sequence.
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
mod tests;
