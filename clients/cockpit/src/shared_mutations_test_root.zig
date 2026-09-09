//! Focused root using the native regression graph's provider/SDK imports.
//! Keeps mutation and creation contracts independently runnable during integration.
test {
    _ = @import("cockpit/durable_creation.zig");
    _ = @import("cockpit/shared_mutations_test.zig");
}
