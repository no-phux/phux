//! Kit's component bundle plus the few application icons the glass menus use.
use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

gpui_kit::assets::icon_assets!(
    MenuIcons,
    [
        SquareTerminal,
        AppWindow,
        LaptopMinimal,
        PanelsTopLeft,
        Server,
        Clock,
        FolderPlus,
        Layers,
        ListTree,
        Pencil,
        Trash,
        GitBranch,
        // Agent harness marks (`app::pane_identity::Harness::glyph`); Asterisk
        // and Bot ship in Kit's component bundle.
        CodeXml,
        Braces,
        Pi,
        SquarePi,
        Orbit,
        Zap,
        MousePointer2,
        Sparkle,
        Feather,
        WandSparkles,
        // Native conversation rows and controls.
        Brain,
        Wrench,
        Square,
        MessagesSquare
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
            IconName::Clock,
            IconName::FolderPlus,
            IconName::Layers,
            IconName::ListTree,
            IconName::Pencil,
            IconName::Trash,
            IconName::GitBranch,
            IconName::Brain,
            IconName::Wrench,
            IconName::Square,
            IconName::MessagesSquare,
            IconName::CircleCheck,
            IconName::CircleX,
            IconName::CircleAlert,
            IconName::Info,
            IconName::ChevronDown,
            IconName::ArrowUp,
            IconName::FileText,
            IconName::Globe,
            IconName::Loader,
            IconName::Minus,
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
