//! Kit's component bundle plus the few application icons used by the menu.
use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

gpui_kit::assets::icon_assets!(
    MenuIcons,
    [
        SquareTerminal,
        AppWindow,
        LaptopMinimal,
        PanelsTopLeft,
        Server
    ]
);

/// Shipping asset source; selected icons borrow embedded SVG bytes.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        match MenuIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(MenuIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::assets::IconName;

    #[test]
    fn menu_and_component_icons_are_embedded() {
        for icon in [
            IconName::SquareTerminal,
            IconName::AppWindow,
            IconName::LaptopMinimal,
            IconName::PanelsTopLeft,
            IconName::Server,
            IconName::ChevronLeft,
            IconName::ChevronRight,
            IconName::Folder,
            IconName::FolderOpen,
            IconName::PanelRight,
            IconName::PanelBottom,
            IconName::Copy,
            IconName::Search,
            IconName::Settings,
            IconName::Plus,
            IconName::ArrowRight,
            IconName::RefreshCw,
        ] {
            assert!(Assets.load(&icon.path()).unwrap().is_some(), "{icon:?}");
        }
    }
}
