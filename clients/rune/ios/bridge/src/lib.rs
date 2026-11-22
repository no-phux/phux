//! The iOS-only C ABI. Pointer validity, main-thread and lifetime contracts
//! are documented in Bridge.h; all app editing/transport policy stays safe
//! Rust in phux-rune. No unsafe permission is added to the application crate.
#[cfg(any(all(target_os = "ios", target_abi = "sim"), test))]
mod diagnostics;
#[cfg(target_os = "ios")]
mod host;
#[cfg(target_os = "ios")]
mod identity;
