//! Focused root using the native regression graph's provider/SDK imports.
//! Keeps mutation and creation contracts independently runnable during integration.
test {
    _ = @import("cockpit/durable_creation.zig");
    _ = @import("cockpit/shared_mutations_test.zig");
}

const creation = @import("cockpit/durable_creation_tests.zig");

test "cutover tab publication waits for shared confirmation" {
    try creation.tabPublication();
}

test "cutover split retains its captured shared destination" {
    try creation.splitDestination();
}

test "cutover native destination epoch fences creation" {
    try creation.windowEpoch();
}

test "cutover unknown spawn outcome never retries" {
    try creation.unknownOutcome();
}

test "cutover refused spawn retires its empty window" {
    try creation.windowRefusal();
}

test "cutover tab reservations use shared capacity" {
    try creation.destinationReservations();
}

test "cutover split reservations use shared identity" {
    try creation.splitReservations();
}

test "cutover shared snapshot leaf subscribes before becoming live" {
    try creation.restoredSubscription();
}

test "cutover incarnation recovery fences old input authority" {
    try creation.incarnationRecovery();
}

test "cutover direct reconnect cancels creation" {
    try creation.directReconnectFences();
}

test "cutover coalesced terminal death retires creation" {
    try creation.earlyTerminalDeath();
}

test "cutover reconnect close publishes destination removal" {
    try creation.reconnectClosePublishes();
}

test "cutover persisted empty view adopts the server workspace" {
    try creation.restoredEmptyWorkspace();
}

test "cutover presentation close and explicit catalog reattachment" {
    try creation.sharedCloseAndCatalogAdmission();
}

test "cutover title metadata announces a new revision" {
    try creation.titleAnnouncement();
}

test "cutover reconnect replaces prior title with empty metadata" {
    try creation.emptyTitleReconnect();
}

test "cutover frozen paint recovers through provider publication" {
    try creation.frozenPaintRecovery();
}

test "cutover native window close rehomes during pending refresh" {
    try creation.nativeCloseRehomesWhileBusy();
}

test "cutover disconnected shared topology refuses edits" {
    try creation.offlineSharedCloseRefuses();
}
