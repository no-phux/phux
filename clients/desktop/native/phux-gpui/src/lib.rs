//! A phux terminal for GPUI applications.
//!
//! [`terminal::Terminal`] is a `gpui::Render` entity bound to one runtime
//! publication: give it an `Arc<phux_client_runtime::Client>`, a `ViewId` and
//! a terminal `ResourceId`, and it paints that terminal with native GPUI
//! glyphs and sends keys, mouse, paste and resizes back through the runtime.
//! The desktop app (`phux-desktop`) is built on it.
//!
//! - [`terminal`]: the view, its settings and its cell geometry.
//! - [`input`]: the `EntityInputHandler` behind the view: keys, IME, pointer,
//!   selection, clipboard and resize proposals.
//! - [`attach`]: dropped files and clipboard images. A local terminal pastes
//!   shell-quoted paths. A remote terminal uploads, then pastes the server path.
//! - [`fonts`]: the bundled terminal faces, registered before the view measures.
//! - [`perf`]: always-on paint and input metrics, one relaxed atomic per sample.
//!
//! Without the `patched-gpui` feature the crate builds on the published
//! gpui-fast; see `README.md` for what that feature adds.

pub mod attach;
pub mod fonts;
pub mod input;
pub mod perf;
mod platform;
mod presentation;
pub mod terminal;
