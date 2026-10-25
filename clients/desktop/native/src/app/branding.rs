//! Process-wide, device-local branding; no workspace can override another's icon.

#[cfg(any(target_os = "linux", test))]
mod linux;

use crate::workspace::persist::{FileLayoutStore, StoreValue};
use anyhow::{Context as _, Result};
use base64::Engine as _;
use gpui::Context;
use serde::Deserialize;
use serde_json::json;
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Profile {
    pub id: String,
    pub name: String,
    icon_png: String,
}

impl Profile {
    pub(crate) fn png(&self) -> Result<Vec<u8>> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.icon_png)
            .context("Invalid approved brand image")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    schema: u32,
    #[serde(rename = "defaultID")]
    default_id: String,
    profiles: Vec<Profile>,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let value: Catalog =
            serde_json::from_str(include_str!("../../../packaging/brand-profiles.json"))
                .expect("Approved runtime brand catalog");
        assert!(value.schema == 1 && value.profiles.iter().any(|p| p.id == value.default_id));
        value
    })
}

pub(crate) fn profiles() -> &'static [Profile] {
    &catalog().profiles
}

pub(crate) fn profile(id: &str) -> &'static Profile {
    profiles()
        .iter()
        .find(|p| p.id == id)
        .or_else(|| profiles().iter().find(|p| p.id == catalog().default_id))
        .expect("Approved default brand profile")
}

pub(crate) struct Branding {
    pub id: String,
    pub error: Option<String>,
    store: FileLayoutStore,
    explicit: bool,
}

impl Branding {
    pub fn load() -> Self {
        Self::from_store(FileLayoutStore::new(
            crate::updates::config_dir().join("appearance.json"),
            None,
        ))
    }

    pub(crate) fn from_store(store: FileLayoutStore) -> Self {
        let saved = match store.read() {
            StoreValue::Present(value) => value["brandProfile"].as_str().map(str::to_owned),
            StoreValue::Missing | StoreValue::Damaged => None,
        };
        Self {
            id: profile(saved.as_deref().unwrap_or("")).id.clone(),
            error: None,
            explicit: saved.is_some(),
            store,
        }
    }

    pub fn restore(&mut self, cx: &mut Context<Self>) {
        if self.explicit {
            self.error = apply(profile(&self.id), cx)
                .err()
                .map(|e| format!("Could not restore the app icon: {e:#}"));
        }
    }

    pub fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.error = self
            .choose(id, |p| apply(p, cx))
            .err()
            .map(|e| format!("Could not change the brand profile: {e:#}"));
        cx.notify();
        cx.refresh_windows();
    }

    /// Commit only after OS acceptance; roll the icon back if saving fails.
    fn choose(&mut self, id: &str, mut apply: impl FnMut(&Profile) -> Result<()>) -> Result<()> {
        let next = profiles()
            .iter()
            .find(|p| p.id == id)
            .context("Unknown brand profile")?;
        apply(next)?;
        if let Err(error) = self.store.write(&json!({"brandProfile": next.id})) {
            apply(profile(&self.id))
                .context("Saving failed and the previous app icon could not be restored")?;
            return Err(error).context("Could not save appearance preferences");
        }
        self.id.clone_from(&next.id);
        self.explicit = true;
        Ok(())
    }
}

fn apply(profile: &Profile, cx: &gpui::App) -> Result<()> {
    // GUI tests use GPUI's simulated native adapter on every host. The Linux
    // launcher writer is verified separately against disposable directories.
    #[cfg(any(test, target_os = "macos", target_os = "windows"))]
    {
        cx.set_app_icon(&profile.png()?)
    }
    #[cfg(all(target_os = "linux", not(test)))]
    {
        let _ = cx;
        linux::apply(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    pub(super) fn location() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "phux-brand-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn every_choice_has_real_pixels_and_survives_a_new_controller() {
        let root = location();
        let path = root.join("appearance.json");
        let mut state = Branding::from_store(FileLayoutStore::new(&path, None));
        for next in profiles() {
            let image = image::load_from_memory(&next.png().unwrap()).unwrap();
            assert_eq!(image.width(), 1024);
            state.choose(&next.id, |_| Ok(())).unwrap();
            let reopened = Branding::from_store(FileLayoutStore::new(&path, None));
            assert_eq!(reopened.id, next.id);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejected_icons_and_failed_saves_keep_the_previous_choice() {
        let root = location();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("appearance.json");
        let mut state = Branding::from_store(FileLayoutStore::new(&path, None));
        let old = state.id.clone();
        let next = profiles().iter().find(|p| p.id != old).unwrap();
        assert!(
            state
                .choose(&next.id, |_| anyhow::bail!("OS refused icon"))
                .is_err()
        );
        assert_eq!(state.id, old);
        assert!(!path.exists());
        fs::create_dir(&path).unwrap(); // A directory cannot be replaced with a preference file.
        let mut applied = Vec::new();
        assert!(
            state
                .choose(&next.id, |p| {
                    applied.push(p.id.clone());
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(applied, [next.id.clone(), old.clone()]);
        assert_eq!(state.id, old);
        assert!(
            state
                .choose("unknown", |_| panic!(
                    "Unknown artwork must not reach the OS"
                ))
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_saved_profiles_resolve_to_the_release_default() {
        let root = location();
        let store = FileLayoutStore::new(root.join("appearance.json"), None);
        store.write(&json!({"brandProfile":"retired"})).unwrap();
        assert_eq!(Branding::from_store(store).id, catalog().default_id);
        fs::remove_dir_all(root).unwrap();
    }
}

/// Exercise the real platform icon interface against disposable state, without a window.
#[cfg(all(feature = "ui-fixtures", target_os = "macos"))]
pub fn verify_runtime_branding(cx: &mut gpui::App) -> Result<()> {
    use gpui::AppContext as _;
    let root = std::env::temp_dir().join(format!("phux-brand-native-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let path = root.join("appearance.json");
    let entity = cx.new(|_| Branding::from_store(FileLayoutStore::new(&path, None)));
    for profile in profiles() {
        entity.update(cx, |state, cx| state.select(&profile.id, cx));
        anyhow::ensure!(
            entity.read(cx).error.is_none(),
            "{:?}",
            entity.read(cx).error
        );
        anyhow::ensure!(Branding::from_store(FileLayoutStore::new(&path, None)).id == profile.id);
        println!(
            "native app icon and persistent choice verified: {}",
            profile.id
        );
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}
