//! The phux design language, shared by every Rust client.
//!
//! phux-brand authors the tokens and ships them as generated constants
//! ([`tokens`], synced by `brand.config.json`; never edit them here). This
//! crate wraps them with what every port must compute the same way:
//!
//! - [`derive()`]: one terminal theme to the full role set (spec §2.2–2.3),
//!   bit for bit with phux-brand's `tools/derive.ts`;
//! - [`verify`]: the floor checks (§2.5);
//! - [`tone_of`], [`plural`] and [`word`]: the status vocabulary (§1.3);
//! - [`catalog()`]: the unified theme catalog (`tokens/themes.json`).
//!
//! `tests/vectors.rs` holds all of it to `tokens/derive-vectors.json`. The
//! crate has no dependencies and no I/O, so it builds for `wasm32` as is.

#[rustfmt::skip]
mod generated;

pub mod catalog;
mod color;
mod roles;
pub mod solve;
mod status;
mod verify;

/// phux-brand's generated constants: the house look, its variants and every
/// non-colour group (type, radius, space, motion, elevation, materials…).
pub use generated::tokens;

pub use catalog::{Catalog, CatalogTheme, catalog};
pub use color::{Mode, Rgb8, Rgba8};
pub use roles::{Layer, Roles, ToneInk};
pub use solve::{Theme, contrast_more, derive};
pub use status::{Agent, Link, State, Tone, ToneInput, plural, plural_message, tone_of, word};
pub use tokens::Params;
pub use verify::{Context, HouseExtras, Severity, VerifyOptions, Violation, verify};

impl Params {
    /// The solver parameters phux-brand fitted from the house look.
    #[must_use]
    pub const fn house() -> Self {
        tokens::DERIVE
    }
}

/// The phux-brand release these tokens came from.
pub const BRAND_VERSION: &str = tokens::META.brand_version;
