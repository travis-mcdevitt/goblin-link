//! Error types for the platform adapter.

use std::path::PathBuf;
use std::time::Duration;

/// Everything that can go wrong while loading or using a platform adapter.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The platform does not declare (or cannot provide) the named capability.
    #[error("capability unavailable on this platform: {0}")]
    Unavailable(&'static str),
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// `platform.toml` is not valid TOML or does not match the schema.
    #[error("{path}: {message}")]
    Parse {
        /// File involved.
        path: PathBuf,
        /// Parser message.
        message: String,
    },
    /// `platform.toml` parsed but violates a rule.
    #[error("invalid platform config: {0}")]
    Invalid(String),
    /// No platform under the search directory matched this device.
    #[error("no platform matched this device (searched {0})")]
    NoMatch(PathBuf),
    /// The platforms directory could not be located.
    #[error("could not locate a platforms directory; set GOBLIN_PLATFORMS_DIR")]
    NoPlatformsDir,
    /// A requested value is out of its legal range.
    #[error("{what} out of range: {detail}")]
    OutOfRange {
        /// Which value.
        what: &'static str,
        /// Explanation.
        detail: String,
    },
    /// A value read from the system could not be understood.
    #[error("unexpected value from {source_name}: {value:?}")]
    BadValue {
        /// Where the value came from.
        source_name: String,
        /// The offending text.
        value: String,
    },
    /// A hook failed.
    #[error("hook {hook}: {kind}")]
    Hook {
        /// Hook file name.
        hook: String,
        /// What happened.
        kind: HookFailure,
    },
}

/// How a hook invocation failed.
#[derive(Debug, thiserror::Error)]
pub enum HookFailure {
    /// The script could not be started.
    #[error("could not start: {0}")]
    Spawn(std::io::Error),
    /// The script ran past its time limit and was killed.
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// The script exited with the "unsupported" status (2).
    #[error("reported unsupported: {0}")]
    Unsupported(String),
    /// The script exited with a failing status.
    #[error("exited with status {code}: {stderr}")]
    Failed {
        /// Exit status (-1 when killed by a signal).
        code: i32,
        /// Trimmed standard error.
        stderr: String,
    },
}

impl PlatformError {
    /// Wrap an I/O error with the path it concerns.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// True when the capability simply is not there (not a runtime failure).
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Unavailable(_)
                | Self::Hook {
                    kind: HookFailure::Unsupported(_),
                    ..
                }
        )
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, PlatformError>;
