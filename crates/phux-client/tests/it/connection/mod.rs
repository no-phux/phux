//! Client connection contracts: transport establishment and acknowledged input.
//! Each scenario owns its listener, runtime, and temporary state.

mod agent_prompt_wire;
mod quic_dial;
mod server_protocol;
mod ws_dial;
