//! `cwd` widget — the focused pane's live working directory. Renders nothing
//! while the cwd is unknown.

use std::collections::BTreeMap;

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec,
    positive_opt, reject_unknown_opts, string_opt,
};

const KIND: &str = "cwd";

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The focused pane's live working directory, fed by the \
              server's `cwd_changed` agent events (kernel-queried PTY-child \
              cwd). A `$HOME` prefix collapses to `~`; renders nothing \
              until the cwd is known.",
    options: &[
        WidgetOptSpec {
            name: "format",
            aliases: &[],
            doc: "string, default `\"{cwd}\"` — render template; every \
                  `{cwd}` occurrence is replaced with the (home-collapsed, \
                  truncated) directory.",
        },
        WidgetOptSpec {
            name: "truncate",
            aliases: &[],
            doc: "integer `> 0`, optional — maximum displayed characters \
                  of the directory itself (format literals not counted); \
                  truncation keeps the path's trailing end.",
        },
    ],
};

/// `cwd` widget: home-collapse, keep the last `truncate` characters (the
/// discriminating end of a deep path), then substitute into `format`.
#[derive(Debug, Clone)]
pub struct CwdWidget {
    /// Render format (`{cwd}`).
    pub format: String,
    /// Maximum displayed `char` count of the directory; `None` is unbounded.
    pub truncate: Option<usize>,
    /// Home directory to collapse to `~`, injected so render stays pure.
    pub home: Option<String>,
}

impl CwdWidget {
    /// Construct a `CwdWidget` (`home = None` skips home collapsing).
    #[must_use]
    pub const fn new(format: String, truncate: Option<usize>, home: Option<String>) -> Self {
        Self {
            format,
            truncate,
            home,
        }
    }

    fn display_path(&self, cwd: &str) -> String {
        let collapsed = match self.home.as_deref() {
            Some(home) if !home.is_empty() && cwd == home => "~".to_owned(),
            // Only at a component boundary: `/home/ab` is not inside `/home/abc`.
            Some(home) if !home.is_empty() && cwd.starts_with(home) => cwd[home.len()..]
                .strip_prefix('/')
                .map_or_else(|| cwd.to_owned(), |rest| format!("~/{rest}")),
            _ => cwd.to_owned(),
        };
        let chars: Vec<char> = collapsed.chars().collect();
        match self.truncate {
            Some(max) if chars.len() > max => chars[chars.len() - max..].iter().collect(),
            _ => collapsed,
        }
    }
}

impl StatusWidget for CwdWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        if ctx.cwd.is_empty() {
            return WidgetCells { cells: Vec::new() };
        }
        WidgetCells::from_text(&self.format.replace("{cwd}", &self.display_path(ctx.cwd)))
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let format = string_opt(KIND, opts, "format")?.unwrap_or_else(|| "{cwd}".to_owned());
    let truncate = positive_opt(KIND, opts, "truncate", None)?;
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
    Ok(Box::new(CwdWidget::new(format, truncate, home)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn render(widget: &CwdWidget, cwd: &str) -> String {
        let ctx = WidgetContext {
            cwd,
            ..WidgetContext::new(UNIX_EPOCH, "", "C-a", &[])
        };
        widget
            .render(&ctx)
            .cells
            .iter()
            .filter_map(|c| c.text.first())
            .collect()
    }

    #[test]
    fn renders_per_format_home_and_truncate() {
        let home = Some("/Users/phall".to_owned());
        let plain = CwdWidget::new("{cwd}".to_owned(), None, home);
        assert_eq!(render(&plain, ""), "");
        assert_eq!(render(&plain, "/tmp/project"), "/tmp/project");
        assert_eq!(render(&plain, "/Users/phall/work/phux"), "~/work/phux");
        assert_eq!(render(&plain, "/Users/phall"), "~");
        assert_eq!(render(&plain, "/Users/phallip/x"), "/Users/phallip/x");

        let short = CwdWidget::new("{cwd}".to_owned(), Some(8), None);
        assert_eq!(render(&short, "/very/deep/tree/leaf"), "ree/leaf");
        assert_eq!(render(&short, "/leaf"), "/leaf");

        let framed = CwdWidget::new("dir: {cwd} |".to_owned(), None, None);
        assert_eq!(render(&framed, "/tmp"), "dir: /tmp |");
    }
}
