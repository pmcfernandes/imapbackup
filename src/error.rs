use thiserror::Error;

/// Indicates aborting processing of the current folder while continuing with the next folder.
/// Direct equivalent of Python's `SkipFolderException`.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct SkipFolderError(pub String);

/// Top-level error hierarchy exit codes.
#[derive(Debug, Error)]
pub enum BackupError {
    /// Exit code 2: Command-line usage or configuration validation errors.
    #[error("{0}")]
    #[allow(dead_code)]
    Config(String),

    /// Exit code 3: DNS / address resolution failure (`socket.gaierror`).
    #[error("problem looking up server '{server}' ({details})")]
    DnsLookup { server: String, details: String },

    /// Exit code 4: Socket, TLS certificate/key, or local filesystem I/O error (`socket.error`).
    #[error("{0}")]
    SocketOrIo(String),

    /// Exit code 5: IMAP4 protocol error (`imaplib.IMAP4.error`).
    #[error("{0}")]
    Imap(String),
}

impl BackupError {
    pub fn exit_code(&self) -> i32 {
        match self {
            BackupError::Config(_) => 2,
            BackupError::DnsLookup { .. } => 3,
            BackupError::SocketOrIo(_) => 4,
            BackupError::Imap(_) => 5,
        }
    }
}

impl From<std::io::Error> for BackupError {
    fn from(err: std::io::Error) -> Self {
        BackupError::SocketOrIo(err.to_string())
    }
}
