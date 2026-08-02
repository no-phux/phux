//! `cwd` widget — the focused pane's live working directory (phux-foz.4).
//!
//! Backed by [`WidgetContext::cwd`], which the TUI feeds from the server's
//! `cwd_changed` agent events (kernel-queried PTY-child cwd, projected per
//! `docs/consumers/tui.md` §8.1 category 1). Renders nothing while the cwd
//! is unknown (`ctx.cwd == ""`), so a bar configured with this widget stays
//! clean until real data arrives.

use std::collections::BTreeMap;

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec,
    reject_unknown_opts,
};

/// Widget kind, used in error messages.
const KIND: &str = "cwd";
/// Default render format; `{cwd}` is the (possibly home-collapsed,
/// possibly truncated) directory.
const DEFAULT_FORMAT: &str = "{cwd}";

/// Doc spec — the factory validates against this same const, so the
/// documented option surface is the enforced one (phux-i0e8.11.3).
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

/// `cwd` widget.
///
/// Pipeline per render: home-collapse (`$HOME` prefix → `~`), then
/// left-truncate to `truncate` chars (keeping the path's *tail* — the
/// discriminating end of a deep path), then substitute into `format`.
#[derive(Debug, Clone)]
pub struct CwdWidget {
    /// Render format; every `{cwd}` occurrence is replaced.
    pub format: String,
    /// Maximum displayed `char` count of the directory itself (format
    /// literals not counted). `None` ⇒ unbounded. Truncation keeps the
    /// trailing characters.
    pub truncate: Option<usize>,
    /// The home directory to collapse to `~`, when known. Injected (not
    /// read from the environment at render time) so render stays a pure
    /// function of construction + context.
    pub home: Option<String>,
}

impl CwdWidget {
    /// Construct a `CwdWidget` with an explicit home directory (or `None`
    /// to skip home collapsing).
    #[must_use]
    pub const fn new(format: String, truncate: Option<usize>, home: Option<String>) -> Self {
        Self {
            format,
            truncate,
            home,
        }
    }

    /// Home-collapse + truncate `cwd` per this widget's options.
    fn display_path(&self, cwd: &str) -> String {
        let collapsed = match self.home.as_deref() {
            Some(home) if !home.is_empty() && cwd == home => "~".to_owned(),
            Some(home) if !home.is_empty() && cwd.starts_with(home) => {
                // Only collapse at a path-component boundary: `/home/ab`
                // must not collapse inside `/home/abc`.
                cwd[home.len()..]
                    .strip_prefix('/')
                    .map_or_else(|| cwd.to_owned(), |rest| format!("~/{rest}"))
            }
            _ => cwd.to_owned(),
        };
        match self.truncate {
            Some(max) => {
                let chars: Vec<char> = collapsed.chars().collect();
                if chars.len() > max {
                    chars[chars.len() - max..].iter().collect()
                } else {
                    collapsed
                }
            }
            None => collapsed,
        }
    }
}

impl StatusWidget for CwdWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        if ctx.cwd.is_empty() {
            return WidgetCells::from_text("");
        }
        let text = self.format.replace("{cwd}", &self.display_path(ctx.cwd));
        WidgetCells::from_text(&text)
    }

    // No `poll_interval` — cwd repaints are event-driven (the bar redraws
    // when a `cwd_changed` event lands or focus moves).
}

/// Factory: builds a [`CwdWidget`] from a TOML `opts` map.
///
/// Accepted keys (per [`SPEC`], rendered into `docs/reference/widgets.md`):
/// - `format` (string, optional, default `"{cwd}"`).
/// - `truncate` (integer, optional, `> 0`) — max displayed chars of the
///   directory, keeping the trailing end.
///
/// The home directory for `~`-collapsing is read from `$HOME` once at
/// build time.
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option, a
/// wrong-typed value, or a non-positive `truncate`.
pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let format = match opts.get("format") {
        None => DEFAULT_FORMAT.to_owned(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`format` must be a string, got {}", other.type_str()),
            });
        }
    };
    let truncate = match opts.get("truncate") {
        None => None,
        Some(toml::Value::Integer(n)) if *n > 0 => {
            Some(usize::try_from(*n).map_err(|_| WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`truncate` does not fit in usize: {n}"),
            })?)
        }
        Some(toml::Value::Integer(n)) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`truncate` must be > 0, got {n}"),
            });
        }
        Some(other) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`truncate` must be an integer, got {}", other.type_str()),
            });
        }
    };
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
    Ok(Box::new(CwdWidget::new(format, truncate, home)))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
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
    fn renders_context_cwd_verbatim_by_default() {
        let w = CwdWidget::new("{cwd}".to_owned(), None, None);
        assert_eq!(render(&w, "/tmp/project"), "/tmp/project");
    }

    #[test]
    fn unknown_cwd_renders_nothing() {
        let w = CwdWidget::new("{cwd}".to_owned(), None, None);
        assert_eq!(render(&w, ""), "");
    }

    #[test]
    fn collapses_home_prefix_to_tilde() {
        let w = CwdWidget::new("{cwd}".to_owned(), None, Some("/Users/phall".to_owned()));
        assert_eq!(render(&w, "/Users/phall/work/phux"), "~/work/phux");
        assert_eq!(render(&w, "/Users/phall"), "~");
    }

    #[test]
    fn home_collapse_respects_component_boundary() {
        // `/Users/phallip` must NOT collapse under home `/Users/phall`.
        let w = CwdWidget::new("{cwd}".to_owned(), None, Some("/Users/phall".to_owned()));
        assert_eq!(render(&w, "/Users/phallip/x"), "/Users/phallip/x");
    }

    #[test]
    fn truncate_keeps_the_path_tail() {
        let w = CwdWidget::new("{cwd}".to_owned(), Some(8), None);
        assert_eq!(render(&w, "/very/deep/tree/leaf"), "ree/leaf");
        // Shorter than the cap renders unchanged.
        assert_eq!(render(&w, "/leaf"), "/leaf");
    }

    #[test]
    fn format_substitutes_cwd_placeholder() {
        let w = CwdWidget::new("dir: {cwd} |".to_owned(), None, None);
        assert_eq!(render(&w, "/tmp"), "dir: /tmp |");
    }

    #[test]
    fn factory_rejects_bad_options() {
        use crate::schema::WidgetSpec;
        use crate::widget::WidgetRegistry;
        let reg = WidgetRegistry::with_builtins();
        for (key, value) in [
            ("truncate", toml::Value::Integer(0)),
            ("truncate", toml::Value::String("ten".to_owned())),
            ("format", toml::Value::Integer(3)),
        ] {
            let spec = WidgetSpec {
                kind: "cwd".to_owned(),
                opts: std::iter::once((key.to_owned(), value)).collect(),
            };
            assert!(
                matches!(reg.build(&spec), Err(WidgetError::InvalidOption { .. })),
                "{key} should be rejected"
            );
        }
    }
}
