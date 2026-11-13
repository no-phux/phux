//! App commands from key chords and menu picks, including tab navigation.

use phux_client_core::layout::{Direction, SplitDir};
use phux_client_runtime::engine::Scroll;
use rune::Tx;

use crate::desk::{Desk, NoticeKind};
use crate::keys::Shortcut;

impl Desk {
    pub fn shortcut(&mut self, action: Shortcut, tx: &mut Tx) {
        match action {
            Shortcut::Copy => self.copy(tx),
            Shortcut::SelectAll => self.select_all(tx),
            Shortcut::Paste => self.paste_clipboard(tx),
            Shortcut::NewTerminal => self.new_tab(),
            Shortcut::Tab(index) => self.select_tab(usize::from(index)),
            Shortcut::RenameSession => self.rename_selected_session(),
            Shortcut::RenameTab => self.rename_active_tab(),
            Shortcut::LockTab => self.toggle_tab_lock(),
            Shortcut::CloseSession => self.close_session(),
            Shortcut::NewSession => self.new_session(),
            Shortcut::NextTerminal => self.cycle(1),
            Shortcut::PreviousTerminal => self.cycle(-1),
            Shortcut::NextTerminalInSession => self.cycle_in_session(1),
            Shortcut::PreviousTerminalInSession => self.cycle_in_session(-1),
            Shortcut::NextSession => self.step_session(1),
            Shortcut::PreviousSession => self.step_session(-1),
            Shortcut::ZoomIn => self.zoom(Some(self.font_size + 1.0)),
            Shortcut::ZoomOut => self.zoom(Some(self.font_size - 1.0)),
            Shortcut::ZoomReset => self.zoom(None),
            Shortcut::ScrollTop => self.scroll(Scroll::Top),
            Shortcut::ScrollBottom => self.scroll(Scroll::Bottom),
            Shortcut::PromptUp => self.scroll(Scroll::Prompt(-1)),
            Shortcut::PromptDown => self.scroll(Scroll::Prompt(1)),
            Shortcut::Palette => self.toggle_palette(),
            Shortcut::ToggleSidebar => self.toggle_sidebar(),
            Shortcut::Inbox => self.toggle_inbox(),
            Shortcut::Changes => self.open_changes(),
            Shortcut::Popup => self.ask_popup(),
            Shortcut::Galaxy => self.toggle_galaxy(),
            Shortcut::Reef => self.toggle_reef(),
            Shortcut::Pitch => self.toggle_pitch(),
            Shortcut::Kitchen => self.toggle_kitchen(),
            Shortcut::PitLane => self.toggle_pitlane(),
            Shortcut::Hive => self.toggle_hive(),
            Shortcut::Settings => self.toggle_settings(),
            Shortcut::ChangeTheme => self.open_themes(),
            Shortcut::SwitchSession => self.switch_sessions(),
            Shortcut::InsertPath => self.open_paths(),
            Shortcut::Screenshot => self.screenshot(),
            Shortcut::CheckForUpdates => self.updater.check_now(),
            Shortcut::InstallUpdate => self.install_update(tx),
            Shortcut::Machines => self.toggle_machines(),
            Shortcut::PairPhone => self.open_pairing(),
            Shortcut::RetryConnection => self.reconnect(),
            Shortcut::CopyDiagnostics => {
                self.copy_diagnostics(tx);
                self.say(NoticeKind::Info, "Diagnostics copied.");
            }
            Shortcut::ToggleFullScreen => tx.toggle_full_screen(),
            Shortcut::KeyboardShortcuts => self.show_shortcuts(),
            Shortcut::EssentialShortcuts => self.show_essentials(),
            Shortcut::GetStarted => self.open_onboarding(),
            Shortcut::JumpToAttention => self.jump_to_attention(),
            Shortcut::SplitRight => self.split(SplitDir::Horizontal),
            Shortcut::SplitDown => self.split(SplitDir::Vertical),
            Shortcut::ZoomPane => self.toggle_zoom(),
            Shortcut::CloseTerminal => self.close_terminal(),
            Shortcut::CloseTab => self.close_active_tab(),
            Shortcut::NextTab => self.step_tab(1),
            Shortcut::PreviousTab => self.step_tab(-1),
            Shortcut::FocusLeft => self.focus_toward(Direction::Left),
            Shortcut::FocusRight => self.focus_toward(Direction::Right),
            Shortcut::FocusUp => self.focus_toward(Direction::Up),
            Shortcut::FocusDown => self.focus_toward(Direction::Down),
            Shortcut::Find => self.open_find(),
            Shortcut::FindNext => self.step_find(-1),
            Shortcut::FindPrevious => self.step_find(1),
            Shortcut::FindSelection => self.find_selection(),
            Shortcut::CopyMode => self.toggle_copy_mode(),
            Shortcut::CopyConfig => self.copy_config(tx),
            Shortcut::RestoreSessions => self.restore_sessions(),
            Shortcut::Bare => self.toggle_bare(),
        }
    }

    fn active_tab(&self) -> Option<usize> {
        self.tabs.list.iter().position(|tab| tab.active)
    }

    fn close_active_tab(&mut self) {
        if let Some(index) = self.active_tab() {
            self.close_tab(index);
        }
    }

    /// The tab `step` along from the active one, wrapping.
    fn step_tab(&mut self, step: isize) {
        let count = self.tabs.list.len() as isize;
        if let Some(index) = self.active_tab() {
            let next = (index as isize + step).rem_euclid(count) as usize;
            self.select_tab(next);
        }
    }
}
