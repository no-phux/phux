//! The generated status-bar widgets reference: every registered widget
//! kind with its options and defaults.
//!
//! Renders from `phux_config::widget::BUILTIN_WIDGET_SPECS` — doc-spec
//! consts colocated with the widget factories, each of which validates
//! its options against its own spec, so the documented surface is the
//! enforced one. A unit test in phux-config pins the spec list to
//! `WidgetRegistry::with_builtins()`, and the freshness test in
//! `super::tests` forces this page's regeneration whenever either moves.

use phux_config::widget::BUILTIN_WIDGET_SPECS;

use super::Page;

/// Render `docs/reference/widgets.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "A `[[status.widgets]]` entry (or a plugin's `[[widgets]]` \
         contribution) names one of the kinds below plus kind-specific \
         options. The set is closed twice over: an unknown `kind` fails \
         the bar build, and every factory rejects an option outside its \
         documented set with a did-you-mean suggestion — `phux config \
         check` surfaces both as located findings. Each section here \
         renders from the same spec const the factory validates against, \
         so these options are exactly the ones the binary accepts.\n\n",
    );
    for spec in BUILTIN_WIDGET_SPECS {
        let _ = write!(body, "## `{}`\n\n{}\n\n", spec.kind, spec.summary);
        if spec.options.is_empty() {
            body.push_str("No kind-specific options.\n\n");
            continue;
        }
        body.push_str("Options:\n\n");
        for opt in spec.options {
            let aliases = if opt.aliases.is_empty() {
                String::new()
            } else {
                format!(
                    " (also spelled {})",
                    opt.aliases
                        .iter()
                        .map(|alias| format!("`{alias}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let _ = writeln!(body, "- `{}`{aliases} — {}", opt.name, opt.doc);
        }
        body.push('\n');
    }

    body.push_str(
        "## The universal `style` option\n\n\
         Every kind additionally accepts a `style` table with optional \
         `fg`, `bg` (color strings: names, `#rrggbb`, or palette indices) \
         and the boolean attributes `bold`, `dim`, `italic`, `underline`, \
         `reverse`. The registry applies it uniformly before the factory \
         runs, so no widget can opt out. Precedence: cells the widget \
         styles itself keep their own style — `windows`' \
         `active`/`inactive` segments, `exec`'s SGR-parsed output, \
         `help-hints`' dim base all win — and only cells the widget left \
         plain inherit the widget-level `style`. A `style` value that is \
         not a table, or a table with an unknown field, fails the bar \
         build and is flagged by `phux config check`.\n",
    );

    body.push_str(
        "\n## The universal `min-cols` / `max-cols` options\n\n\
         Every kind also accepts `min-cols` and `max-cols`: integer \
         bounds on the width of the **whole status row** (not the \
         widget's own share) outside which the widget renders nothing at \
         all. A hidden widget costs no width, so the widgets that remain \
         get the columns it would have taken.\n\n\
         Use them to make one lineup change shape with the terminal \
         rather than shrink inside it. The honest answer to a narrow \
         window is often not \"show this smaller\" but \"do not show \
         this\": a clock is worth four columns at 120 and worth none at \
         45. The shipped `[status]` block uses exactly this to trade the \
         session name and clock for a `switch` chip below 65 columns.\n\n\
         ```toml\n\
         right = [\n\
         \x20 { kind = \"session-name\", min-cols = 65 },\n\
         \x20 { kind = \"time\", format = \" %a %H:%M\", min-cols = 65 },\n\
         \x20 { kind = \"switch\", max-cols = 64 },\n\
         ]\n\
         ```\n\n\
         Both bounds are inclusive and either may be given alone. A \
         `min-cols` above `max-cols` describes a widget that could never \
         render and fails the bar build, as does a non-integer value; \
         `phux config check` flags both.\n",
    );

    Page {
        file: "widgets.md",
        title: "phux status-bar widgets reference",
        summary: "Every registered status-bar widget kind with its options \
                  and defaults.",
        tldr: "Every status-bar widget kind the binary registers, with the \
               exact options and defaults each factory accepts, plus the \
               universal `style` table and the `min-cols` / `max-cols` \
               responsive-visibility bounds. Rendered from the same spec \
               consts the factories validate options against, so a kind or \
               option is listed here exactly when the binary accepts it.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use phux_config::widget::WidgetRegistry;

    use super::page;

    /// The page documents exactly the kinds `with_builtins()` registers —
    /// the page-level face of the registry-vs-spec pin in phux-config. In
    /// particular, the seven never-registered kinds the old hand table
    /// promised (`window`, `pane`, `host`, `mode`, `key-indicator`,
    /// `text`, `spacer`) can never reappear without being registered.
    #[test]
    fn widgets_page_sections_are_exactly_the_registered_kinds() {
        let page = page();
        let sections: Vec<&str> = page
            .body
            .lines()
            .filter_map(|line| line.strip_prefix("## `"))
            .filter_map(|rest| rest.strip_suffix('`'))
            .collect();
        assert_eq!(
            sections,
            WidgetRegistry::with_builtins().kinds(),
            "generated widgets reference drifted from the builtin registry"
        );
    }

    /// The closed-surface details the old hand table carried must survive:
    /// session-name's `format` option and the universal style attributes.
    #[test]
    fn widgets_page_keeps_the_load_bearing_details() {
        let page = page();
        for needle in [
            "`format`",
            "`max-len` (also spelled `max_len`)",
            "`parse-ansi` (also spelled `parse_ansi`)",
            "## The universal `style` option",
            "`underline`",
        ] {
            assert!(
                page.body.contains(needle),
                "generated widgets reference lost {needle:?}"
            );
        }
    }
}
