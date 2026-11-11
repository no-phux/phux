//! Standing mirrors (`[[mirrors]]`): displays and windows the server keeps
//! mirrored (`docs/resource-kinds.md#standing-mirrors`).

use serde::{Deserialize, Serialize};

/// One display or window the server keeps a `phux agent surface mirror`
/// running for, in its own session named by [`Self::session_name`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfigEntry {
    /// What to mirror, as `phux agent surface mirror` takes it: `display`,
    /// `display:OUTPUT`, `App`, or `App:title`.
    pub target: String,
    /// Frame encoding, `png` (default) or `jpeg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<MirrorFormat>,
    /// JPEG quality, 1 to 100 (default 70); needs `format = "jpeg"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    /// Scale frames down to at most this many pixels wide.
    #[serde(default, rename = "max-width", skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    /// Milliseconds between captures while no input arrives (default 1000).
    #[serde(
        default,
        rename = "interval-ms",
        skip_serializing_if = "Option::is_none"
    )]
    pub interval_ms: Option<u64>,
}

/// A standing mirror's frame encoding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MirrorFormat {
    /// Lossless PNG.
    Png,
    /// JPEG at `quality`.
    Jpeg,
}

/// Every standing mirror's session name starts with this; the server owns
/// such sessions and closes one whose entry is gone.
pub const MIRROR_SESSION_PREFIX: &str = "mirror-";

/// `prefix` and `label`, lowercased, every run of other characters a `-`.
pub(crate) fn session_name(prefix: &str, label: &str) -> String {
    let mut name = prefix.to_owned();
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c.to_ascii_lowercase());
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    name.trim_end_matches('-').to_owned()
}

impl MirrorConfigEntry {
    /// The session the mirror runs in: `mirror-` and the target, lowercased,
    /// every run of other characters a `-` (`display:HDMI-A-1` is
    /// `mirror-display-hdmi-a-1`).
    #[must_use]
    pub fn session_name(&self) -> String {
        session_name(MIRROR_SESSION_PREFIX, &self.target)
    }

    /// The `phux` arguments that run this mirror.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["agent", "surface", "mirror"].map(str::to_owned).into();
        let mut flag = |name: &str, value: Option<String>| {
            if let Some(value) = value {
                args.extend([format!("--{name}"), value]);
            }
        };
        flag("interval", self.interval_ms.map(|v| v.to_string()));
        flag(
            "format",
            self.format.map(|f| {
                match f {
                    MirrorFormat::Png => "png",
                    MirrorFormat::Jpeg => "jpeg",
                }
                .to_owned()
            }),
        );
        flag("quality", self.quality.map(|v| v.to_string()));
        flag("max-width", self.max_width.map(|v| v.to_string()));
        args.extend(["--".to_owned(), self.target.clone()]);
        args
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_entry_parses_names_its_session_and_builds_its_argv() {
        let cfg: crate::Config = toml::from_str(
            "[[mirrors]]\ntarget = \"display:HDMI-A-1\"\nformat = \"jpeg\"\nmax-width = 1280\n\n\
             [[mirrors]]\ntarget = \"display\"\n",
        )
        .expect("parse");
        let [hdmi, display] = cfg.mirrors.as_slice() else {
            panic!("two entries");
        };
        assert_eq!(hdmi.session_name(), "mirror-display-hdmi-a-1");
        assert_eq!(display.session_name(), "mirror-display");
        assert_eq!(
            hdmi.args(),
            [
                "agent",
                "surface",
                "mirror",
                "--format",
                "jpeg",
                "--max-width",
                "1280",
                "--",
                "display:HDMI-A-1"
            ]
        );
        assert_eq!(
            display.args(),
            ["agent", "surface", "mirror", "--", "display"]
        );
        assert!(toml::from_str::<crate::Config>("[[mirrors]]\ntarget = \"x\"\nfps = 3\n").is_err());
        assert!(toml::from_str::<crate::Config>("[[mirrors]]\nformat = \"png\"\n").is_err());
    }
}
