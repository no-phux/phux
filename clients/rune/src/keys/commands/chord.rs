//! A chord: a key and exactly the modifiers held with it, and how each
//! platform writes it.

/// The key of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Char(char),
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
}

impl Cap {
    fn accelerator_name(self) -> String {
        match self {
            Self::Char(c) => c.to_string(),
            Self::Enter => "enter".to_owned(),
            Self::Left => "left".to_owned(),
            Self::Right => "right".to_owned(),
            Self::Up => "up".to_owned(),
            Self::Down => "down".to_owned(),
            Self::Home => "home".to_owned(),
            Self::End => "end".to_owned(),
            Self::PageUp => "pageup".to_owned(),
            Self::PageDown => "pagedown".to_owned(),
            Self::Insert => "insert".to_owned(),
        }
    }

    pub(super) fn mac_name(self) -> String {
        match self {
            Self::Char(c) => c.to_ascii_uppercase().to_string(),
            Self::Enter => "↩".to_owned(),
            Self::Left => "←".to_owned(),
            Self::Right => "→".to_owned(),
            Self::Up => "↑".to_owned(),
            Self::Down => "↓".to_owned(),
            Self::Home => "↖".to_owned(),
            Self::End => "↘".to_owned(),
            Self::PageUp => "⇞".to_owned(),
            Self::PageDown => "⇟".to_owned(),
            Self::Insert => "Insert".to_owned(),
        }
    }

    pub(super) fn linux_name(self) -> String {
        match self {
            Self::Enter => "Enter".to_owned(),
            Self::Home => "Home".to_owned(),
            Self::End => "End".to_owned(),
            Self::PageUp => "PgUp".to_owned(),
            Self::PageDown => "PgDn".to_owned(),
            other => other.mac_name(),
        }
    }
}

/// A key and exactly the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub cap: Cap,
    /// ⌘ on macOS, Super elsewhere.
    pub command: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    /// The key alone.
    pub(super) const fn bare(cap: Cap) -> Self {
        Self {
            cap,
            command: false,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub(super) const fn cmd(cap: Cap) -> Self {
        Self {
            command: true,
            ..Self::bare(cap)
        }
    }

    pub(super) const fn ctrl(cap: Cap) -> Self {
        Self::bare(cap).and_ctrl()
    }

    pub(super) const fn shift(self) -> Self {
        Self {
            shift: true,
            ..self
        }
    }

    /// The key with Shift alone.
    pub(super) const fn shifted(cap: Cap) -> Self {
        Self::bare(cap).shift()
    }

    pub(super) const fn alt(self) -> Self {
        Self { alt: true, ..self }
    }

    /// The key with Option (Alt) alone.
    pub(super) const fn opt(cap: Cap) -> Self {
        Self::bare(cap).alt()
    }

    pub(super) const fn and_ctrl(self) -> Self {
        Self { ctrl: true, ..self }
    }

    /// The accelerator Rune parses for a native menu: `cmd+shift+d`.
    pub fn accelerator(self) -> String {
        let mods = [
            (self.command, "cmd"),
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.shift, "shift"),
        ];
        let held = mods.iter().filter(|(held, _)| *held).map(|(_, name)| *name);
        let key = self.cap.accelerator_name();
        held.chain([key.as_str()]).collect::<Vec<_>>().join("+")
    }

    /// As this platform writes it.
    pub(super) fn label(self) -> String {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            self.mac_label()
        } else {
            self.linux_label()
        }
    }

    /// As macOS writes it: `⇧⌘D`.
    pub(super) fn mac_label(self) -> String {
        let marks = [
            (self.ctrl, "⌃"),
            (self.alt, "⌥"),
            (self.shift, "⇧"),
            (self.command, "⌘"),
        ];
        let held = marks
            .iter()
            .filter(|(held, _)| *held)
            .map(|(_, mark)| *mark);
        held.chain([self.cap.mac_name().as_str()]).collect()
    }

    /// As Linux desktops write it: `Ctrl+Shift+D`.
    pub(super) fn linux_label(self) -> String {
        let marks = [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.command, "Super"),
        ];
        let held = marks
            .iter()
            .filter(|(held, _)| *held)
            .map(|(_, mark)| *mark);
        let name = self.cap.linux_name();
        held.chain([name.as_str()]).collect::<Vec<_>>().join("+")
    }
}
