//! The phux design language, shared by every Rust client.
//!
//! The tokens are authored in `tokens/src` and generated into constants
//! ([`tokens`]) by `just design-gen`; never edit the generated file. This
//! crate wraps them with what every port must compute the same way:
//!
//! - [`derive()`]: one terminal theme to the full role set,
//!   bit for bit with the generator's `tools/src/design/derive.ts`;
//! - [`tone_of`], [`plural`] and [`word`]: the status vocabulary;
//! - [`catalog()`]: the unified theme catalog (`tokens/themes.json`).
//!
//! `tests/vectors.rs` spot-checks it against the generated subset of the
//! golden vectors (the generator's tests run the full set). The
//! crate has no dependencies and no I/O, so it builds for `wasm32` as is.

#[rustfmt::skip]
mod generated;

pub mod catalog;
mod color;
mod roles;
pub mod solve;
mod status;

/// The generated constants: the house look, its variants and every
/// non-colour group (type, radius, space, motion, elevation, materials…).
pub use generated::tokens;

pub use catalog::{Catalog, CatalogTheme, catalog};
pub use color::{Mode, Rgb8, Rgba8};
pub use roles::{Layer, Roles, ToneInk};
pub use solve::{Theme, contrast_more, derive};
pub use status::{Agent, Link, State, Tone, ToneInput, plural, plural_message, tone_of, word};
pub use tokens::Params;

impl Params {
    /// The solver parameters the generator fitted from the house look.
    #[must_use]
    pub const fn house() -> Self {
        tokens::DERIVE
    }
}

/// The design version these tokens came from (`tokens/src/version.txt`).
pub const DESIGN_VERSION: &str = tokens::META.design_version;
