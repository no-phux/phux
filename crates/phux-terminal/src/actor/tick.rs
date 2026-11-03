//! Submodule for terminal actor internals.

pub use phux_resource::RESIZE_RESYNC_DEBOUNCE;
pub use phux_resource::replication::{
    DEFAULT_TICK_INTERVAL, MAX_TICK_INTERVAL, MIN_TICK_INTERVAL, RETRANSMIT_DEFAULT,
    RETRANSMIT_MAX, RETRANSMIT_MIN, RTT_EMA_ALPHA, RttEstimator, TICK_RESET_DEADBAND,
    adaptive_tick_interval, retransmit_timeout,
};
