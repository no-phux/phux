//! Bounded opaque replay captures. Their bytes are interpreted only by engines.

#[derive(Debug, Clone)]
pub struct SnapshotBytes {
    pub cols: u16,
    pub rows: u16,
    pub bytes: Vec<u8>,
    pub scrollback: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("snapshot byte limit exceeded")]
    LimitExceeded,
    #[error("snapshot allocation failed")]
    OutOfMemory,
    #[error("canonical terminal is on loan to a snapshot capture")]
    TerminalUnavailable,
    #[error("rendered capture would be {required} bytes, over the {budget}-byte budget")]
    RenderBudgetExceeded { required: usize, budget: usize },
    #[error("{0}")]
    Engine(String),
}
