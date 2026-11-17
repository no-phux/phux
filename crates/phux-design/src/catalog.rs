//! The unified theme catalog, `tokens/themes.json`, in one id namespace.
//!
//! The file is flat (strings, booleans, null, arrays of strings and one
//! number), so a small hand parser reads it and the crate keeps no
//! dependencies. It is embedded, parsed once, and checked by the tests.

use std::sync::OnceLock;

use crate::color::Rgb8;
use crate::solve::Theme;

const SOURCE: &str = include_str!("../tokens/themes.json");

/// One theme: a terminal palette, the chrome accent, which clients ship it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTheme {
    pub id: &'static str,
    pub name: &'static str,
    pub bg: Rgb8,
    pub fg: Rgb8,
    pub cursor: Rgb8,
    pub accent: Option<Rgb8>,
    pub selection_bg: Option<Rgb8>,
    pub dark: bool,
    pub ansi: [Rgb8; 16],
    /// `rune`, `mobile`, `tui`.
    pub ships: Vec<&'static str>,
    /// Whether it has an alternate app icon.
    pub icon: bool,
}

impl CatalogTheme {
    /// The solver's input.
    #[must_use]
    pub const fn input(&self) -> Theme {
        Theme {
            bg: self.bg,
            fg: self.fg,
            ansi: self.ansi,
            cursor: Some(self.cursor),
            accent: self.accent,
            selection_bg: self.selection_bg,
            dark: Some(self.dark),
        }
    }

    #[must_use]
    pub fn ships_in(&self, client: &str) -> bool {
        self.ships.contains(&client)
    }
}

/// The catalog and the ids older clients stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub themes: Vec<CatalogTheme>,
    /// Old id → catalog id, for stored preferences.
    pub aliases: Vec<(&'static str, &'static str)>,
    /// Mobile's own older ids.
    pub legacy_mobile: Vec<(&'static str, &'static str)>,
}

impl Catalog {
    /// The theme `id` names, through an alias if it is an old id.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&CatalogTheme> {
        let id = self
            .aliases
            .iter()
            .find(|(old, _)| *old == id)
            .map_or(id, |(_, new)| *new);
        self.themes.iter().find(|t| t.id == id)
    }

    /// The themes `client` ships, in catalog order.
    pub fn shipped<'a>(&'a self, client: &'a str) -> impl Iterator<Item = &'a CatalogTheme> + 'a {
        self.themes.iter().filter(move |t| t.ships_in(client))
    }
}

/// The embedded catalog.
///
/// # Panics
///
/// Never for the synced file: `tests` parse it, and brand's schema closes it.
#[must_use]
#[allow(clippy::panic, reason = "the embedded file is checked by the tests")]
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| parse(SOURCE).unwrap_or_else(|e| panic!("tokens/themes.json: {e}")))
}

// --- the parser ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Number,
    Str(&'static str),
    Array(Vec<Self>),
    Object(Vec<(&'static str, Self)>),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(fields) => fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    const fn str(&self) -> Option<&'static str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct Parser {
    src: &'static str,
    at: usize,
}

impl Parser {
    fn ws(&mut self) {
        let rest = &self.src[self.at..];
        self.at += rest.len() - rest.trim_start().len();
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.at).copied()
    }

    fn eat(&mut self, b: u8) -> Result<(), String> {
        self.ws();
        if self.peek() == Some(b) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", char::from(b), self.at))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => {
                let rest = &self.src[self.at..];
                let len = rest
                    .find(|c: char| !matches!(c, '-' | '+' | '.' | 'e' | 'E' | '0'..='9'))
                    .unwrap_or(rest.len());
                self.at += len;
                Ok(Value::Number)
            }
            _ => Err(format!("unexpected input at byte {}", self.at)),
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
        if self.src[self.at..].starts_with(w) {
            self.at += w.len();
            Ok(v)
        } else {
            Err(format!("expected {w} at byte {}", self.at))
        }
    }

    /// A string with no escapes: the catalog never needs one.
    fn string(&mut self) -> Result<&'static str, String> {
        self.eat(b'"')?;
        let rest = &self.src[self.at..];
        let end = rest.find('"').ok_or("unterminated string")?;
        let s = &rest[..end];
        if s.contains('\\') {
            return Err(format!("escaped string at byte {}", self.at));
        }
        self.at += end + 1;
        Ok(s)
    }

    fn array(&mut self) -> Result<Value, String> {
        self.eat(b'[')?;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Array(out));
        }
        loop {
            out.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(out));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.at)),
            }
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.eat(b'{')?;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Value::Object(out));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.eat(b':')?;
            out.push((key, self.value()?));
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(out));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.at)),
            }
        }
    }
}

fn color(v: Option<&Value>, what: &str) -> Result<Rgb8, String> {
    v.and_then(Value::str)
        .and_then(Rgb8::parse)
        .ok_or_else(|| format!("{what} is not a colour"))
}

fn optional_color(v: Option<&Value>, what: &str) -> Result<Option<Rgb8>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        some => color(some, what).map(Some),
    }
}

fn pairs(v: Option<&Value>) -> Vec<(&'static str, &'static str)> {
    match v {
        Some(Value::Object(fields)) => fields
            .iter()
            .filter_map(|(k, v)| v.str().map(|v| (*k, v)))
            .collect(),
        _ => Vec::new(),
    }
}

fn theme(v: &Value) -> Result<CatalogTheme, String> {
    let id = v
        .get("id")
        .and_then(Value::str)
        .ok_or("a theme has no id")?;
    let field = |k: &str| v.get(k);
    let Some(Value::Array(ansi)) = field("ansi") else {
        return Err(format!("{id}: no ansi"));
    };
    if ansi.len() != 16 {
        return Err(format!("{id}: ansi holds {} colours", ansi.len()));
    }
    let mut palette = [Rgb8::BLACK; 16];
    for (slot, c) in palette.iter_mut().zip(ansi) {
        *slot = color(Some(c), id)?;
    }
    let ships = match field("ships") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::str).collect(),
        _ => Vec::new(),
    };
    Ok(CatalogTheme {
        id,
        name: field("name")
            .and_then(Value::str)
            .ok_or_else(|| format!("{id}: no name"))?,
        bg: color(field("bg"), id)?,
        fg: color(field("fg"), id)?,
        cursor: color(field("cursor"), id)?,
        accent: optional_color(field("accent"), id)?,
        selection_bg: optional_color(field("selection_bg"), id)?,
        dark: matches!(field("dark"), Some(Value::Bool(true))),
        ansi: palette,
        ships,
        icon: matches!(field("icon"), Some(Value::Bool(true))),
    })
}

fn parse(src: &'static str) -> Result<Catalog, String> {
    let mut p = Parser { src, at: 0 };
    let root = p.value()?;
    p.ws();
    if p.at != src.len() {
        return Err(format!("trailing input at byte {}", p.at));
    }
    let Some(Value::Array(themes)) = root.get("themes") else {
        return Err("no themes".to_owned());
    };
    Ok(Catalog {
        themes: themes.iter().map(theme).collect::<Result<_, _>>()?,
        aliases: pairs(root.get("aliases")),
        legacy_mobile: pairs(root.get("legacy").and_then(|l| l.get("mobile"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_parses_with_unique_ids() {
        let c = catalog();
        assert_eq!(c.themes.len(), 43);
        for (i, t) in c.themes.iter().enumerate() {
            assert!(c.themes[i + 1..].iter().all(|o| o.id != t.id), "{}", t.id);
        }
        assert_eq!(c.themes[0].id, "phux", "the house terminal is first");
        assert_eq!(c.find("midnight").map(|t| t.id), Some("phux"));
        assert_eq!(c.find("catppuccin").map(|t| t.id), Some("catppuccin-mocha"));
        assert!(c.find("gone").is_none());
        assert!(c.shipped("rune").all(|t| t.ships_in("rune")));
        assert!(c.legacy_mobile.contains(&("gruvbox", "gruvbox-material")));
    }

    #[test]
    fn the_parser_refuses_what_it_cannot_read() {
        for bad in [r#"{"themes": [}"#, r#"{"themes": []} x"#, r#"{"a": "\n"}"#] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
