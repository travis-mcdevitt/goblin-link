//! CLI error type and exit codes.

use goblin_platform::PlatformError;

/// Anything a command can fail with.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The platform adapter reported an error.
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// The arguments were well-formed but not acceptable.
    #[error("{0}")]
    Usage(String),
    /// The daemon could not be reached or refused the request.
    #[error("Muse service: {0}")]
    Daemon(String),
    /// IPC transport failure.
    #[error(transparent)]
    Ipc(#[from] goblin_ipc::ClientError),
    /// Output could not be produced.
    #[error("could not encode output: {0}")]
    Encode(#[from] serde_json::Error),
}

impl CliError {
    /// Process exit status: 4 when the daemon is unreachable or refuses, 3 when a capability is
    /// unavailable, 2 for usage or out-of-range arguments, else 1.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Platform(e) if e.is_unavailable() => 3,
            Self::Usage(_) | Self::Platform(PlatformError::OutOfRange { .. }) => 2,
            Self::Daemon(_) | Self::Ipc(_) => 4,
            _ => 1,
        }
    }
}

/// Result alias for commands.
pub type Result<T> = std::result::Result<T, CliError>;
