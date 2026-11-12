//! Session colours: `[[sessions]]` entries pairing a session name with the
//! palette colour ([`crate::host_color`]) clients draw it in.
//!
//! This is client config, not server state: the colour follows the file. A
//! session with no entry has no colour; nothing is hashed, unlike a host.

use serde::{Deserialize, Deserializer, Serialize};

use crate::host_color::{PALETTE, index_of};

/// One coloured session in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionConfigEntry {
    /// The session's name, matched exactly. The first entry for a name wins.
    pub name: String,

    /// Palette id the session is drawn in; anything else fails the load.
    #[serde(deserialize_with = "palette_id")]
    pub color: String,
}

fn palette_id<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let id = String::deserialize(de)?;
    if index_of(&id).is_some() {
        return Ok(id);
    }
    let ids: Vec<_> = PALETTE.iter().map(|(id, _)| *id).collect();
    Err(serde::de::Error::custom(format!(
        "{id:?} is not a session colour; use one of {}",
        ids.join(", ")
    )))
}

/// The `0xRRGGBB` configured for the session called `name`, if any.
#[must_use]
pub fn color_of(sessions: &[SessionConfigEntry], name: &str) -> Option<u32> {
    let entry = sessions.iter().find(|entry| entry.name == name)?;
    index_of(&entry.color).map(|index| PALETTE[index].1)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::color_of;
    use crate::loader::parse_client;

    #[test]
    fn a_named_session_gets_its_colour_and_the_rest_get_none() {
        let input = "[[sessions]]\nname = 'api'\ncolor = 'coral'\n\
                     [[sessions]]\nname = 'api'\ncolor = 'navy'\n";
        let sessions = parse_client(input, Path::new("config.toml"))
            .expect("parses")
            .sessions;
        assert_eq!(color_of(&sessions, "api"), Some(0xE8_71_5A));
        assert_eq!(color_of(&sessions, "API"), None);
        assert_eq!(color_of(&sessions, "web"), None);
        assert_eq!(color_of(&[], "api"), None);
    }

    #[test]
    fn an_unknown_colour_or_key_fails_the_load() {
        for (input, needle) in [
            (
                "[[sessions]]\nname = 'api'\ncolor = 'teal'\n",
                "\"teal\" is not a session colour; use one of blue, violet,",
            ),
            ("[[sessions]]\nname = 'api'\ncolor = 'auto'\n", "\"auto\""),
            ("[[sessions]]\nname = 'api'\n", "color"),
            (
                "[[sessions]]\nname = 'api'\ncolor = 'sky'\nicon = 'x'\n",
                "icon",
            ),
        ] {
            let err = parse_client(input, Path::new("config.toml")).expect_err(input);
            assert!(err.to_string().contains(needle), "{err}");
        }
    }
}
