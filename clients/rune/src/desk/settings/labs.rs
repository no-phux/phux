//! Developer mode and labs: switches Settings writes back to config.toml,
//! where the server, the terminal UI and the CLI read the same keys.

use phux_config_core::labs::{Lab, PREDICTIVE_ECHO};

use super::super::{Desk, NoticeKind};
use super::Category;

impl Desk {
    /// Developer mode on or off, saved to `[developer] mode` in config.toml
    /// so the terminal UI and `phux config labs` see the same switch.
    pub fn toggle_developer_mode(&mut self) {
        let on = !self.developer_mode;
        if !self.save_to_file("developer.mode", Some(on)) {
            return;
        }
        self.developer_mode = on;
        if !on && self.settings_ui.category == Some(Category::Labs) {
            self.settings_ui.category = Some(Category::Developer);
        }
    }

    /// Whether `lab` is on here: the file's choice, else its default. A lab
    /// that decides per use (predictive echo) reads off until chosen.
    pub fn lab_on(&self, lab: &Lab) -> bool {
        self.labs.get(lab.id).or(lab.default).unwrap_or(false)
    }

    /// Flip `lab` and save it under `[labs]` in config.toml, where every
    /// part of phux reads it.
    pub fn toggle_lab(&mut self, lab: &'static Lab) {
        let on = !self.lab_on(lab);
        if !self.save_to_file(&format!("labs.{}", lab.id), Some(on)) {
            return;
        }
        self.labs.flags.insert(lab.id.to_owned(), on);
        if lab.id == PREDICTIVE_ECHO {
            self.predictive_echo = Some(on);
        }
    }

    /// Write one boolean key of config.toml (`None` removes it), keeping the
    /// rest of the file. False, with the reason said, when it did not save.
    /// Tests never write the real file.
    fn save_to_file(&mut self, key: &str, value: Option<bool>) -> bool {
        if cfg!(test) {
            return true;
        }
        let edit = value.map_or(phux_config::settings::Edit::Unset, |on| {
            phux_config::settings::Edit::Set(toml::Value::Boolean(on))
        });
        let path = phux_config_core::loader::config_path();
        match phux_config::settings::write_edit(&path, key, edit) {
            Ok(_) => true,
            Err(err) => {
                self.say(NoticeKind::Error, format!("Couldn't save {key}: {err}"));
                false
            }
        }
    }
}
