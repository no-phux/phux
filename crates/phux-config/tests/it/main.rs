//! phux-config integration tests in one binary: the schema, keybindings,
//! configuration layers and distributions, and the status bar. The loader's
//! tests mutate the process environment, so they keep their own binary.

mod common;

mod configuration;
mod keybind;
mod parse;
mod status_bar;
