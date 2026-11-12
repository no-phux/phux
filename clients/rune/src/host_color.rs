//! Each machine's colour: phux's shared palette and hash, with the override
//! `phux host ls --json` reports, eased just enough to read on the ground it
//! sits on. Colour is never the only cue: every dot sits beside a name.

use std::collections::HashMap;
use std::sync::{LazyLock, PoisonError, RwLock};

use phux_config_core::host_color::{PALETTE, auto_index, index_of};
use rune::{Color, El, el};
use serde_json::Value;

use crate::machines::LOCAL;
use crate::theme::{color, contrast, mix, on_fill};

/// What the registry last said: each host's `0xRRGGBB` and whether an
/// override chose it. This machine is keyed by its short host name.
#[derive(Default)]
struct Known {
    local: Option<String>,
    hosts: HashMap<String, (u32, bool)>,
}

// ponytail: one process-wide table, so every view can colour a name without
// threading the registry through it; per-window state if windows ever differ.
static KNOWN: LazyLock<RwLock<Known>> = LazyLock::new(RwLock::default);

/// The name phux hashes for this machine: the one `host ls` reports, else
/// its short host name.
fn local_key(known: &Known) -> String {
    known
        .local
        .clone()
        .unwrap_or_else(phux_config_core::instance::local_host_name)
}

fn hex(value: &Value) -> Option<(u32, bool)> {
    let hex = value["hex"].as_str()?.strip_prefix('#')?;
    let rgb = u32::from_str_radix(hex, 16).ok()?;
    Some((rgb, value["override"] == true))
}

/// Take in a `phux host ls --json` document. An older phux reports no
/// colours, and every name falls back to the hash.
pub fn learn(doc: &Value) {
    let mut hosts = HashMap::new();
    for host in doc["hosts"].as_array().into_iter().flatten() {
        if let (Some(name), Some(color)) = (host["name"].as_str(), hex(&host["color"])) {
            hosts.entry(name.to_owned()).or_insert(color);
        }
    }
    let local = doc["local"]["name"].as_str().map(str::to_owned);
    if let (Some(name), Some(color)) = (&local, hex(&doc["local"]["color"])) {
        hosts.insert(name.clone(), color);
    }
    *KNOWN.write().unwrap_or_else(PoisonError::into_inner) = Known { local, hosts };
}

/// Show a pick before the CLI has written it: `id` is a palette id or `auto`.
pub fn pick(name: &str, id: &str) {
    let mut known = KNOWN.write().unwrap_or_else(PoisonError::into_inner);
    let key = if name == LOCAL {
        local_key(&known)
    } else {
        name.to_owned()
    };
    let chosen = index_of(id).map(|i| (PALETTE[i].1, true));
    known.hosts.insert(
        key.clone(),
        chosen.unwrap_or((PALETTE[auto_index(&key)].1, false)),
    );
}

/// `name`'s base colour and whether it was picked; [`LOCAL`] is this machine.
pub fn entry(name: &str) -> (u32, bool) {
    let known = KNOWN.read().unwrap_or_else(PoisonError::into_inner);
    let key = if name == LOCAL {
        local_key(&known)
    } else {
        name.to_owned()
    };
    known
        .hosts
        .get(&key)
        .copied()
        .unwrap_or((PALETTE[auto_index(&key)].1, false))
}

/// `name`'s colour as 0–255 channels, for the scenes' own painters.
pub fn channels(name: &str) -> [f32; 3] {
    let rgb = entry(name).0;
    [16, 8, 0].map(|shift| ((rgb >> shift) & 0xff) as f32)
}

/// `name`'s colour moved toward white or black just far enough to read at
/// 3:1 on `ground`, at most 50% (olive on Windows 98 silver needs ~45%);
/// the hue stays.
pub fn on(name: &str, ground: Color) -> Color {
    eased(entry(name).0, ground)
}

fn eased(rgb: u32, ground: Color) -> Color {
    let end = if on_fill(ground) == Color::WHITE {
        0xff_ffff
    } else {
        0
    };
    let step = |s: u8| mix(rgb, end, f32::from(s) / 100.0);
    color(
        (0..=50)
            .map(step)
            .find(|c| contrast(color(*c), ground) >= 3.0)
            .unwrap_or_else(|| step(50)),
    )
}

/// A short bar in `color` down the selected row's leading edge: the fill
/// stays the theme's, so its text reads as before.
pub fn stripe(color: Color) -> El {
    el().pin(Some(0.0), Some(7.0), None, Some(7.0))
        .w(3.0)
        .radius(1.5)
        .fill(color)
        .inert()
        .a11y_hidden()
}

/// A round mark in `name`'s colour.
pub fn dot(name: &str, size: f32, ground: Color) -> El {
    el().size(size, size)
        .radius(size / 2.0)
        .fill(on(name, ground))
        .a11y_hidden()
}

/// The window's top edge in `name`'s colour, so each window says which
/// machine it is on; thin enough to stay out of the titlebar's way.
pub fn edge(name: &str, ground: Color) -> El {
    el().pin(Some(0.0), Some(0.0), Some(0.0), None)
        .h(2.5)
        .fill(on(name, ground))
        .a11y_hidden()
}

/// The machine `text` names first, as a whole word, of `names`.
// ponytail: a word match over a notice's text; carry the host on the notice
// if a name ever collides with ordinary words.
pub fn named<'a>(text: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '-');
    names
        .into_iter()
        .filter(|name| !name.is_empty())
        .filter_map(|name| {
            text.match_indices(name)
                .find(|(at, _)| {
                    edge(text[..*at].chars().next_back())
                        && edge(text[at + name.len()..].chars().next())
                })
                .map(|(at, _)| (at, name))
        })
        .min_by_key(|(at, _)| *at)
        .map(|(_, name)| name)
}

/// A palette colour by index, eased for `ground`: the picker's swatches.
pub fn swatch(index: usize, ground: Color) -> Color {
    eased(PALETTE[index].1, ground)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_shared_vectors_hold_here_too() {
        for (name, id) in [
            ("mini", "magenta"),
            ("macbook", "olive"),
            ("powerhouse-1", "blue"),
            ("This Mac", "slate"),
        ] {
            assert_eq!(PALETTE[auto_index(name)].0, id, "{name}");
        }
    }

    #[test]
    fn the_registry_wins_and_older_phux_falls_back_to_the_hash() {
        learn(&json!({ "hosts": [
            { "name": "box-a", "color": { "id": "violet", "hex": "#9A78E8", "override": true } },
            { "name": "box-b" },
        ], "local": { "name": "studio", "color": { "hex": "#3BAFDA", "override": true } } }));
        assert_eq!(entry("box-a"), (0x9A_78E8, true));
        assert_eq!(entry("box-b"), (PALETTE[auto_index("box-b")].1, false));
        assert_eq!(entry(LOCAL), (0x3B_AFDA, true));
        pick("box-a", "auto");
        assert!(!entry("box-a").1);
        pick(LOCAL, "copper");
        assert_eq!(entry(LOCAL), (0xC9_926B, true));
    }

    #[test]
    fn a_notice_names_its_machine_by_whole_word() {
        let names = ["mini", "mac", LOCAL];
        let text = format!("mini isn't available, so this is {LOCAL}.");
        assert_eq!(named(&text, names), Some("mini"));
        assert_eq!(named("macbook-mini set up", names), None);
        assert_eq!(named("Switch to mac now", names), Some("mac"));
    }

    #[test]
    fn every_swatch_reads_on_light_and_dark_grounds() {
        // The system theme's sheets, sidebar and window, light then dark.
        for ground in [0xfb_fbfc, 0xf1_f1f2, 0xff_ffff, 0x1e_1f22, 0x14_1517] {
            for (i, (id, _)) in PALETTE.iter().enumerate() {
                let c = swatch(i, color(ground));
                let bar = contrast(c, color(ground));
                assert!(bar >= 3.0, "{id} on {ground:06x}: {bar}");
            }
        }
    }
}
