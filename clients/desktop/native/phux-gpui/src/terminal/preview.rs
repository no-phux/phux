//! A passive miniature of an existing publication. No runtime, input handler,
//! presentation receipt or resize proposal can be reached from this surface.

use std::sync::Arc;

use gpui::{IntoElement, canvas, prelude::*};
use phux_client_runtime::publication::GridFrame;

use super::{Settings, paint};

/// Retains the native painter's scene for one thumbnail. Drop it when hidden.
#[derive(Clone, Default)]
pub struct Preview {
    scene: paint::SceneCache,
}

impl Preview {
    /// Fit the entire published grid in the available bounds without changing
    /// its rows, columns, scroll position, selection or terminal size.
    pub fn element(&self, frame: Arc<GridFrame>, settings: Settings) -> impl IntoElement {
        let scene = self.scene.clone();
        canvas(
            move |bounds, window, cx| {
                let settings = preview_settings(settings, cx);
                paint::prepare_preview(frame, settings, bounds, &scene, window, cx)
            },
            |bounds, prepared, window, cx| {
                let _ = prepared.paint(bounds, window, cx);
            },
        )
        .size_full()
    }
}

fn preview_settings(mut settings: Settings, cx: &mut gpui::App) -> Settings {
    crate::fonts::install(cx);
    settings.sanitize();
    crate::fonts::resolve(&mut settings.font, cx);
    settings.focused = false;
    settings.cursor_visible = false;
    settings.host_fills_background = false;
    settings.size_owner = false;
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn miniatures_use_the_native_fonts_and_sanitize_before_scaling(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut settings = Settings { font: gpui::font("Missing Phux Preview Font"), font_size: f32::NAN, ..Default::default() };
            crate::fonts::install(cx);
            crate::fonts::resolve(&mut settings.font, cx);
            let expected = settings.font.clone();
            settings.font = gpui::font("Missing Phux Preview Font");
            let settings = preview_settings(settings, cx);
            assert_eq!(settings.font, expected);
            assert_eq!(settings.font.family, crate::fonts::BUNDLED);
            assert!(settings.font.fallbacks.is_some());
            assert_eq!(settings.font_size, Settings::default().font_size);
            assert!(!settings.focused && !settings.cursor_visible && !settings.size_owner);
        });
    }
}
