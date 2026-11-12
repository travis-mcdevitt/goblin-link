//! The error type shared by all stores.

use std::path::PathBuf;

/// Failure of a store operation. Messages never contain secret values.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A file system operation failed.
    #[error("i/o error on {path}: {source}")]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A file exists but is not valid JSON of the expected shape.
    #[error("{path} is corrupt: {reason}")]
    Corrupt {
        /// The offending file.
        path: PathBuf,
        /// What is wrong (without values).
        reason: String,
    },
    /// A profile name violates the naming rules.
    #[error("invalid profile name: {0}")]
    InvalidName(String),
    /// A profile with that name (or differing only in case) exists.
    #[error("profile {0:?} already exists")]
    AlreadyExists(String),
    /// No such profile, chat or record.
    #[error("{0} not found")]
    NotFound(String),
    /// The active profile cannot be deleted.
    #[error("profile {0:?} is active")]
    ActiveProfile(String),
    /// The saved pairing is unusable.
    #[error("saved pairing is invalid: {0}")]
    InvalidPairing(&'static str),
    /// An SDK token that gadgets.muse.ai could not have issued.
    #[error("the SDK token is not valid")]
    InvalidSdkToken,
    /// A settings key is unknown.
    #[error("unknown setting {0:?}")]
    UnknownSetting(String),
    /// A settings value is not allowed for its key.
    #[error("invalid value for {key}: {reason}")]
    InvalidSetting {
        /// The setting.
        key: String,
        /// Why the value was rejected.
        reason: String,
    },
    /// A path escapes the directory it must stay in.
    #[error("path {0} is outside the allowed directory")]
    OutsideDirectory(PathBuf),
    /// An identifier is not usable as a file name.
    #[error("invalid id: {0}")]
    InvalidId(String),
}

impl StoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn corrupt(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self::Corrupt {
            path: path.into(),
            reason: reason.into(),
        }
    }
}
