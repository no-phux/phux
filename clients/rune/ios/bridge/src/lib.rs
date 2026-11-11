//! The iOS-only C ABI. Pointer validity, main-thread and lifetime contracts
//! are documented in Bridge.h; all app editing/transport policy stays safe
//! Rust in phux-rune. No unsafe permission is added to the application crate.
#[cfg(target_os = "ios")]
mod host;
#[cfg(target_os = "ios")]
mod identity;
