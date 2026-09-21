//! Where a phux provider connects, and the modules that serve that.
//!
//! This was the Zig socket worker: its own DNS, connect, length framing, poll
//! loop and write deadlines, feeding frames across a `Bridge`. All of it now
//! lives in `phux-client-runtime` behind `phux_client_connect` (ADR-0133), so
//! the provider names a destination and the runtime owns the socket, the
//! reconnect ladder and the framing.
//!
//! What remains is the destination itself and the two modules that belong to
//! this build module so their `@cImport` of `phux/client.h` is shared.

const std = @import("std");

/// Local coordinator supervision. The runtime dials; it does not start
/// anything, so a local server is still this machine's to supervise.
pub const startup = @import("startup.zig");

/// phux-client-ffi's remote-host registry and resolved-tunnel handles. The
/// provider reaches these through this re-export so the file belongs to
/// exactly one build module.
pub const remote = @import("remote_tunnel.zig");

test {
    _ = remote;
    _ = startup;
}

/// Where a provider connects. Slices are borrowed and must outlive the
/// provider that holds them.
pub const Endpoint = union(enum) {
    /// A bare address. It carries neither a certificate pin nor a token, and
    /// the registry is what supplies both, so there is no runtime lane for
    /// it; opening refuses.
    tcp: struct { host: []const u8, port: u16 },
    /// This machine's phux server, by absolute socket path.
    unix: []const u8,
    /// A host in the phux CLI's `[[remote]]` registry, resolved and dialed by
    /// the runtime under the CLI's own trust rules.
    remote: Remote,

    pub const Remote = struct {
        /// Registry name or `[USER@]HOST[:PORT]`.
        target: []const u8,
        /// Absolute phux `config.toml`, or empty for the CLI's own path.
        config_path: []const u8 = "",
        /// Provider-owned failure record; outlives every connection.
        status: ?*remote.Status = null,
    };
};
