//! Configuration layers, distributions, plugins, and schema round trips.
//! The environment-mutating loader and snapshot roots retain their boundaries.

#[path = "../common/mod.rs"]
mod common;

mod agent_roundtrip;
mod distro;
mod herdr_distro;
mod layers;
mod plugin;
mod provenance;
mod satellite;
mod scaffold;
