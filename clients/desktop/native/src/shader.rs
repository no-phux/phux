//! Backdrop shaders: a WGSL `fn shade(uv: vec2f) -> vec4f` rendered offscreen
//! on the platform's primary GPU API and painted as a GPUI image.
//!
//! | Module | Owns |
//! |---|---|
//! | [`program`] | the contract: the `phux` uniform block, checks, errors with line and column |
//! | [`gpu`] | the shared device and a per-surface renderer with read-back |
//! | [`library`] | the built-in shaders and the user's shader folder |
//!
//! Nothing here knows about windows or the desk: `app::shader_view` puts a
//! program on screen and `app::backdrop` dresses the window in one.

pub mod gpu;
pub mod library;
pub mod program;

pub use program::{Program, ShaderError, Uniforms};
