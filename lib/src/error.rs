//! Library error type: every fallible public API returns
//! [`Result`] with [`RsrpcError`] instead of a boxed opaque error, so
//! callers can match on failure kinds instead of parsing strings.
//!
//! Wire-visible messages keep their historical text.

use thiserror::Error;

/// Alias for fallible library operations.
pub type Result<T> = std::result::Result<T, RsrpcError>;

/// Every failure mode of this library.
#[derive(Debug, Error)]
pub enum RsrpcError {
  /// Caller-supplied JSON (database, overrides) failed to parse.
  #[error("invalid JSON provided to RPCServer: {0}")]
  InvalidJson(String),

  /// Database file missing or unreadable.
  #[error("rpcserver could not find file {0:?}: {1}", path, source)]
  UnreadableFile {
    path: std::path::PathBuf,
    source: std::io::Error,
  },

  /// A poisoned mutex was encountered (a previous panic while holding
  /// the lock). Carries which lock, for diagnostics.
  #[error("{0} lock poisoned: {1}")]
  Poisoned(&'static str, String),

  /// Catch-all preserving historical ad-hoc messages.
  #[error("{0}")]
  Message(String),

  /// I/O failures (procfs reads, file writes).
  #[error(transparent)]
  Io(#[from] std::io::Error),

  /// JSON (de)serialization failures.
  #[error(transparent)]
  Json(#[from] serde_json::Error),

  /// HTTP fetch failures (database, exclusions).
  #[error(transparent)]
  Http(#[from] ureq::Error),
}
