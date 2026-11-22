//! phux-client integration tests in one binary: connection establishment,
//! command isolation, poll reuse, QUIC spawn streams, and uploads, each
//! driving a production server runtime.

mod support;

mod command_isolation;
mod connection;
mod native_controller;
mod polling_reuse;
mod quic_spawn_stream;
mod upload;
