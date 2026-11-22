//! phux-config integration tests in one binary: the schema, keybindings, and
//! configuration layers and distributions. The loader's tests mutate the
//! process environment, so they keep their own binary.

mod common;

mod configuration;
mod keybind;
mod parse;
