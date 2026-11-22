//! Plugin `[[widgets]]` contributions merged into the status lineup.

use crate::plugin::{PluginManifest, PluginWidgetSlot};
use crate::schema::{StatusCfg, Widget, WidgetSpec};
use crate::widget::validate;

/// Append enabled plugins' `[[widgets]]` contributions to their slots.
///
/// A contribution that does not build is dropped with a warning, so one
/// broken plugin cannot break the whole bar.
pub fn merge_widget_contributions(status: &mut StatusCfg, manifests: &[PluginManifest]) {
    for manifest in manifests {
        for widget in &manifest.widgets {
            let spec = WidgetSpec {
                kind: widget.kind.clone(),
                opts: widget.opts.clone(),
            };
            if let Err(err) = validate(&spec) {
                tracing::warn!(
                    plugin = %manifest.id,
                    widget = %widget.id,
                    error = %err,
                    "dropping plugin status-widget contribution that failed validation",
                );
                continue;
            }
            let slot = match widget.slot {
                PluginWidgetSlot::Left => &mut status.left,
                PluginWidgetSlot::Center => &mut status.center,
                PluginWidgetSlot::Right => &mut status.right,
            };
            slot.push(Widget::Spec(spec));
        }
    }
}
