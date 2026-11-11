//! A chord key by key, for drawing keycaps.

use super::{Chord, Command};

impl Chord {
    /// One string per key, as this platform names them.
    fn keycaps(self) -> Vec<String> {
        let mac = cfg!(any(target_os = "macos", target_os = "ios"));
        let marks = if mac {
            [
                (self.ctrl, "⌃"),
                (self.alt, "⌥"),
                (self.shift, "⇧"),
                (self.command, "⌘"),
            ]
        } else {
            [
                (self.ctrl, "Ctrl"),
                (self.alt, "Alt"),
                (self.shift, "Shift"),
                (self.command, "Super"),
            ]
        };
        let name = if mac {
            self.cap.mac_name()
        } else {
            self.cap.linux_name()
        };
        let held = marks.iter().filter(|(held, _)| *held);
        held.map(|(_, mark)| (*mark).to_owned())
            .chain([name])
            .collect()
    }
}

impl Command {
    /// The first chord key by key; empty with no chord.
    pub fn keycaps(&self) -> Vec<String> {
        self.chords()
            .first()
            .map_or_else(Vec::new, |chord| chord.keycaps())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Shortcut, command};

    #[test]
    fn a_chord_splits_into_one_cap_per_key() {
        let caps = command(Shortcut::Settings).expect("settings").keycaps();
        let expected: &[&str] = if cfg!(any(target_os = "macos", target_os = "ios")) {
            &["⌘", ","]
        } else {
            &["Ctrl", "Shift", ","]
        };
        assert_eq!(caps, expected);
        let none = command(Shortcut::Galaxy).expect("galaxy").keycaps();
        assert!(none.is_empty(), "no chord, no caps");
    }
}
