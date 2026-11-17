//! `button` widget — a clickable label that runs one TUI action: the `+`
//! after the tab strip, a `+ session` chip, or any argument-free action a
//! user wants one click away.

use std::collections::BTreeMap;

use phux_config::widget::kinds::button::{ButtonOpts, parse};

use super::{CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// `button` widget.
#[derive(Debug, Clone)]
pub struct ButtonWidget {
    /// The action a click runs; a canonical vocabulary name.
    pub action: &'static str,
    /// The text, padded by one space each side.
    pub label: String,
    /// Style of every button cell.
    pub style: CellStyle,
}

impl StatusWidget for ButtonWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        let style = (!self.style.is_plain()).then(|| self.style.clone());
        let mut cells = WidgetCells::from_styled(&format!(" {} ", self.label), style);
        for cell in &mut cells.cells {
            cell.hit = Some(CellHit::Action(self.action));
        }
        cells
    }

    /// Whole or not at all: a clipped button is a smaller target claiming
    /// the same columns.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        let cells = self.render(ctx);
        if cells.len() <= budget {
            cells
        } else {
            WidgetCells { cells: Vec::new() }
        }
    }
}

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let ButtonOpts {
        action,
        label,
        style,
    } = parse(opts)?;
    Ok(Box::new(ButtonWidget {
        action,
        label,
        style,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> WidgetContext<'static> {
        WidgetContext::new(std::time::SystemTime::UNIX_EPOCH, "", "", &[])
    }

    /// Every padded cell is the action's click target.
    #[test]
    fn renders_a_padded_label_stamped_with_its_action() {
        let widget = ButtonWidget {
            action: "new-window",
            label: "+".to_owned(),
            style: CellStyle::default(),
        };
        let cells = widget.render(&ctx()).cells;
        assert_eq!(cells.len(), 3);
        assert!(
            cells
                .iter()
                .all(|cell| cell.hit == Some(CellHit::Action("new-window")))
        );
        assert!(widget.render_within(&ctx(), 2).cells.is_empty());
    }
}
