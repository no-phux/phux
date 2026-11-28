//! The marks' column: a theme's swatch, and the glyph for every row that
//! is not a terminal, so the column never has a hole in it.

use rune::{El, Icon, el};

use super::super::icons;
use super::super::tokens::{Paint, Tokens};
use crate::keys::Shortcut;
use crate::palette::{Item, Target};

/// A theme's swatch: its ground with its accent beside it, ringed so a
/// ground like the panel's still has an edge, at a line icon's weight.
pub(super) fn swatch(id: Option<&'static str>, size: f32, t: &Tokens) -> El {
    let theme = id.and_then(crate::theme::find);
    let (fill, accent) = theme.map_or(
        (t.roles.layer.ground.paint(), t.roles.text.dim.paint()),
        |t| {
            (
                crate::theme::color(t.background),
                crate::theme::color(t.accent),
            )
        },
    );
    let half = size / 2.0;
    el().size(size, size)
        .radius(t.radius.full)
        .clip_rounded(half)
        .fill(fill)
        .border(t.space.hairline, t.roles.border.control.paint())
        .child(
            el().pin(Some(half), Some(0.0), Some(0.0), Some(0.0))
                .fill(accent)
                .inert(),
        )
        .a11y_hidden()
}

/// The glyph for a row that is not a terminal or a theme. Every row has
/// one, so the column the marks keep never has a hole in it.
pub(super) fn glyph(target: &Target, item: &Item) -> &'static Icon {
    match target {
        Target::Action(action) => action_mark(*action),
        Target::Session(_) => &icons::WINDOW,
        Target::NewSession(_) => &icons::PLUS,
        Target::NewWorktree(_) => &icons::BRANCH,
        Target::Path(path) if path.ends_with('/') || item.title.ends_with('/') => &icons::FOLDER,
        Target::Path(_) => &icons::DOCUMENT,
        Target::Mirror(_) | Target::StartMirror(..) | Target::Screens | Target::Screen(..) => {
            &icons::WINDOW
        }
        Target::MoveToTab => &icons::BRING,
        Target::KeepAwake(_) => &icons::AWAKE,
        Target::SizeLock(_) => &icons::LOCK,
        Target::Popup(_) => &icons::BOLT,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Target::Extension(_) => &icons::PUZZLE,
        Target::Terminal { .. } | Target::Theme(_) => &icons::COMMAND,
    }
}

/// A command's own glyph. The terminal's prompt is never one: a command
/// must not read as a terminal. A command with no glyph of its own takes
/// the command key's.
pub(super) fn action_mark(action: Shortcut) -> &'static Icon {
    use Shortcut as S;
    match action {
        S::NewTerminal | S::NewSession => &icons::PLUS,
        S::Paste | S::Copy | S::CopyConfig | S::CopyDiagnostics | S::CopyMode | S::SelectAll => {
            &icons::CLIPBOARD
        }
        S::Find | S::FindNext | S::FindPrevious | S::FindSelection | S::Palette => &icons::SEARCH,
        S::SwitchSession | S::DisplayPanes | S::ZoomPane | S::ToggleFullScreen | S::Bare => {
            &icons::WINDOW
        }
        S::SplitRight => &icons::SPLIT_RIGHT,
        S::SplitDown => &icons::SPLIT_DOWN,
        S::ZoomIn
        | S::ZoomOut
        | S::ZoomReset
        | S::InterfaceBigger
        | S::InterfaceSmaller
        | S::InterfaceReset => &icons::TEXT_SIZE,
        S::Settings => &icons::SETTINGS,
        S::Machines | S::PairPhone => &icons::MACHINES,
        S::CloseTerminal | S::CloseTab | S::CloseSession | S::ClearSelection => &icons::CLOSE,
        S::Inbox => &icons::INBOX,
        S::JumpToAttention => &icons::BELL,
        S::ChangeTheme => &icons::PALETTE,
        S::RenameSession | S::RenameTab => &icons::PENCIL,
        S::LockTab => &icons::LOCK,
        S::ToggleSidebar | S::FocusSidebar => &icons::SIDEBAR,
        S::RestoreSessions | S::RetryConnection | S::CheckForUpdates => &icons::REFRESH,
        S::Screenshot => &icons::CAMERA,
        S::KeyboardShortcuts | S::EssentialShortcuts | S::SyncPanes => &icons::KEYBOARD,
        S::InsertPath => &icons::FOLDER,
        S::ReviewRawRelaunch | S::Changes | S::QuickLook => &icons::DOCUMENT,
        S::Popup => &icons::BOLT,
        S::GetStarted | S::WhatWorksHere => &icons::HAND,
        S::Galaxy | S::Reef | S::Pitch | S::Kitchen | S::PitLane | S::Hive | S::Playground => {
            &icons::PUZZLE
        }
        _ => arrow_mark(action),
    }
}

/// Moving, scrolling and stepping commands: the way they go.
fn arrow_mark(action: Shortcut) -> &'static Icon {
    use Shortcut as S;
    match action {
        S::ScrollToTop
        | S::ScrollPageUp
        | S::ScrollHalfPageUp
        | S::PromptUp
        | S::FocusUp
        | S::PreviousSession => &icons::ARROW_UP,
        S::ScrollToBottom
        | S::ScrollPageDown
        | S::ScrollHalfPageDown
        | S::PromptDown
        | S::FocusDown
        | S::NextSession
        | S::InstallUpdate => &icons::ARROW_DOWN,
        S::PreviousTerminal
        | S::PreviousTerminalInSession
        | S::PreviousTab
        | S::PreviousSlotTab
        | S::LastPane
        | S::LastTab
        | S::FocusLeft => &icons::ARROW_LEFT,
        S::NextTerminal
        | S::NextTerminalInSession
        | S::NextTab
        | S::NextSlotTab
        | S::FocusRight
        | S::Tab(_) => &icons::ARROW_RIGHT,
        _ => &icons::COMMAND,
    }
}
