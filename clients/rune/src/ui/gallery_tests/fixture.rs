//! The design fixture every platform's gallery paints: the same six
//! looks, the accessibility modes and three locales.

use std::sync::OnceLock;

use crate::theme::Theme;

/// The largest chrome text scale the gallery checks (Rune `TextSize` max).
pub(super) const LARGEST_TEXT: f32 = 1.4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Look {
    HouseDark,
    HouseLight,
    SolarizedLight,
    /// Mobile's Gruvbox, which Rune does not ship: a derived theme from the
    /// shared catalog.
    Gruvbox,
    Windows98,
    /// A synthetic edge the solver can only best-effort: its warnings show.
    LowContrastSolarized,
}

impl Look {
    pub(super) const ALL: [Self; 6] = [
        Self::HouseDark,
        Self::HouseLight,
        Self::SolarizedLight,
        Self::Gruvbox,
        Self::Windows98,
        Self::LowContrastSolarized,
    ];

    pub(super) const fn tag(self) -> &'static str {
        match self {
            Self::HouseDark => "dark",
            Self::HouseLight => "light",
            Self::SolarizedLight => "solarized-light",
            Self::Gruvbox => "gruvbox",
            Self::Windows98 => "windows-98",
            Self::LowContrastSolarized => "edge-low-contrast",
        }
    }

    /// The system appearance the window runs in.
    pub(super) fn dark(self) -> bool {
        self.theme().map_or(self == Self::HouseDark, |t| t.dark)
    }

    pub(super) fn theme(self) -> Option<&'static Theme> {
        match self {
            Self::HouseDark | Self::HouseLight => None,
            Self::SolarizedLight => crate::theme::find("solarized-light"),
            Self::Windows98 => crate::theme::find("windows-98"),
            Self::Gruvbox => Some(catalog_theme("gruvbox-material")),
            Self::LowContrastSolarized => Some(edge("edge/low-contrast-solarized")),
        }
    }
}

const fn hex(c: phux_design::Rgb8) -> u32 {
    c.to_u32()
}

/// A catalog theme Rune does not ship, as a Rune theme.
fn catalog_theme(id: &str) -> &'static Theme {
    static GRUVBOX: OnceLock<Theme> = OnceLock::new();
    GRUVBOX.get_or_init(|| {
        let t = phux_design::catalog().find(id).expect("in the catalog");
        Theme {
            id: t.id,
            name: t.name,
            dark: t.dark,
            ansi: t.ansi.map(hex),
            foreground: hex(t.fg),
            background: hex(t.bg),
            cursor: hex(t.cursor),
            accent: hex(t.accent.unwrap_or(t.ansi[6])),
            selection: t.selection_bg.map(hex),
            bold: None,
        }
    })
}

/// A synthetic edge from the brand's golden vectors.
fn edge(id: &'static str) -> &'static Theme {
    static EDGE: OnceLock<Theme> = OnceLock::new();
    EDGE.get_or_init(|| {
        let doc: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../crates/phux-design/tokens/derive-vectors.subset.json"
        ))
        .unwrap();
        let case = (doc["cases"].as_array().unwrap().iter())
            .find(|c| c["id"] == id)
            .expect("the edge case");
        let input = &case["input"];
        let c = |v: &serde_json::Value| hex(phux_design::Rgb8::parse(v.as_str().unwrap()).unwrap());
        let mut ansi = [0; 16];
        for (slot, v) in ansi.iter_mut().zip(input["ansi"].as_array().unwrap()) {
            *slot = c(v);
        }
        Theme {
            id,
            name: "Low-contrast Solarized",
            dark: case["expect"]["mode"] == "dark",
            ansi,
            foreground: c(&input["fg"]),
            background: c(&input["bg"]),
            cursor: input["cursor"]
                .as_str()
                .map_or(ansi[7], |_| c(&input["cursor"])),
            accent: input["accent"]
                .as_str()
                .map_or(ansi[6], |_| c(&input["accent"])),
            selection: input["selection_bg"]
                .as_str()
                .map(|_| c(&input["selection_bg"])),
            bold: None,
        }
    })
}

/// The fixture's own words, as a locale would set them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Locale {
    En,
    /// `en-XA`: accented and padded 40%, so truncation shows.
    PseudoLong,
    /// `ar`: right-to-left script. Rune has no layout direction yet, so the
    /// page shows the script, not mirrored chrome (`DEVELOP.md`).
    Arabic,
}

impl Locale {
    pub(super) const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::PseudoLong => "en-XA",
            Self::Arabic => "ar",
        }
    }

    pub(super) fn say(self, s: &str) -> String {
        match self {
            Self::En => s.to_owned(),
            Self::PseudoLong => pseudo(s),
            Self::Arabic => arabic(s).unwrap_or(s).to_owned(),
        }
    }
}

fn pseudo(s: &str) -> String {
    let accented: String = s
        .chars()
        .map(|c| match c {
            'a' => 'á',
            'e' => 'é',
            'i' => 'í',
            'o' => 'ö',
            'u' => 'ü',
            'c' => 'ç',
            'n' => 'ñ',
            's' => 'š',
            'y' => 'ý',
            other => other,
        })
        .collect();
    let pad = (s.chars().count() * 2).div_ceil(5);
    format!("[{accented}{}]", "·".repeat(pad))
}

fn arabic(s: &str) -> Option<&'static str> {
    Some(match s {
        "phux" => "فوكس",
        "website" => "الموقع",
        "phux-mobile" => "فوكس للهاتف",
        "restyle sidebar" => "إعادة تصميم الشريط الجانبي",
        "cargo test" => "اختبار cargo",
        "review #2617" => "مراجعة ‎#2617",
        "docs build" => "بناء المستندات",
        "ios snapshots" => "لقطات iOS",
        "android build" => "بناء أندرويد",
        _ => return None,
    })
}
