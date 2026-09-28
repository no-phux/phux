//! `exit` widget — the focused pane's last command exit code (OSC 133 `D`).
//! Renders nothing until a code is known.

use std::collections::BTreeMap;

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec,
    reject_unknown_opts, string_opt,
};

const KIND: &str = "exit";

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The focused pane's last command exit code, fed by the OSC-133 \
              `D`-mark (`command_finished.exit_code`), so it requires shell \
              integration. Renders nothing until a command finishes with a \
              reported code.",
    options: &[WidgetOptSpec {
        name: "format",
        aliases: &[],
        doc: "string, default `\"{code}\"` — render template; every \
              `{code}` occurrence is replaced with the decimal exit code.",
    }],
};

/// `exit` widget.
#[derive(Debug, Clone)]
pub struct ExitWidget {
    /// Render format (`{code}`).
    pub format: String,
}

impl StatusWidget for ExitWidget {
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "`{code}` is this widget's TOML placeholder"
    )]
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        ctx.last_exit.map_or_else(
            || WidgetCells { cells: Vec::new() },
            |code| WidgetCells::from_text(&self.format.replace("{code}", &code.to_string())),
        )
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let format = string_opt(KIND, opts, "format")?.unwrap_or_else(|| "{code}".to_owned());
    Ok(Box::new(ExitWidget { format }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn renders_the_code_through_the_format_or_nothing() {
        let render = |format: &str, last_exit| -> String {
            let ctx = WidgetContext {
                last_exit,
                ..WidgetContext::new(UNIX_EPOCH, "", "C-a", &[])
            };
            let w = ExitWidget {
                format: format.to_owned(),
            };
            w.render(&ctx)
                .cells
                .iter()
                .filter_map(|c| c.text.first())
                .collect()
        };
        assert_eq!(render("{code}", Some(127)), "127");
        assert_eq!(render("rc={code}", Some(1)), "rc=1");
        assert_eq!(render("{code}", None), "");
    }
}
