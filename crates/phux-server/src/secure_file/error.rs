//! The one error every secure-file operation returns, and its mapping into
//! each store's own error type.

use std::io;

use crate::auth::AuthError;
use crate::workload::WorkloadError;

/// Why a secure file could not be read, locked, or replaced. Each store
/// maps it into its own error type.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SecureFileError {
    /// Filesystem operation failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// A file or directory failed its ownership, type, or mode check.
    #[error("{file} {reason}")]
    Insecure {
        /// The role of the file (never its path).
        file: &'static str,
        /// The rule it broke.
        reason: &'static str,
    },
    /// The file (or its directory) changed during every bounded attempt.
    #[error("the file kept changing while it was read")]
    Unstable,
    /// A development build aimed at production state
    /// ([`phux_config::production::refuse_dev_on_production_state`]).
    #[error("{0}")]
    ProductionState(String),
}

impl From<SecureFileError> for WorkloadError {
    fn from(error: SecureFileError) -> Self {
        match error {
            SecureFileError::Io(error) => Self::Io(error),
            SecureFileError::Insecure { file, reason } => Self::Insecure { file, reason },
            SecureFileError::Unstable => Self::Unstable,
            SecureFileError::ProductionState(refusal) => Self::ProductionState(refusal),
        }
    }
}

/// Keeps the token store's split: an integrity failure is insecure, an I/O
/// or unstable read is unreadable (`StoreCondition::of`).
impl From<SecureFileError> for AuthError {
    fn from(error: SecureFileError) -> Self {
        match error {
            SecureFileError::Io(error) => Self::Io(error),
            insecure @ SecureFileError::Insecure { .. } => {
                Self::InsecureStore(insecure.to_string())
            }
            SecureFileError::Unstable => Self::UnstableStore,
            SecureFileError::ProductionState(refusal) => Self::ProductionStore(refusal),
        }
    }
}
