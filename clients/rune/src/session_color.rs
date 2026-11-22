//! Each session's colour, from `config.toml`'s `[[sessions]]`: drawn with
//! the host colours' own marks, and absent for a session the file leaves out.

use std::sync::{LazyLock, PoisonError, RwLock};

use phux_config_core::SessionConfigEntry;
use phux_config_core::session_color::color_of;

// ponytail: one process-wide table read once at launch, like the host
// colours'; reload it with the file if Rune ever watches `config.toml`.
static KNOWN: LazyLock<RwLock<Vec<SessionConfigEntry>>> = LazyLock::new(RwLock::default);

/// Take in the file's `[[sessions]]`.
pub fn learn(sessions: Vec<SessionConfigEntry>) {
    *KNOWN.write().unwrap_or_else(PoisonError::into_inner) = sessions;
}

/// The `0xRRGGBB` configured for the session called `name`, if any.
pub fn of(name: &str) -> Option<u32> {
    color_of(&KNOWN.read().unwrap_or_else(PoisonError::into_inner), name)
}
