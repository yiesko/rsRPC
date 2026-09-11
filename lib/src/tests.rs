//! Unit tests for the library, one module per area.
//!
//! Kept in their own folder (instead of inline `#[cfg(test)]` modules) so
//! the implementation files stay focused on implementation.
//! They run with `cargo test --lib`.

mod client_connector;
mod cmd;
mod commands;
mod ipc_utils;
mod logger;
mod overrides;
mod process;
mod rpc_server;
mod state;
mod user;

/// Serializes every test that mutates process-global env: the variables
/// are process-wide, so exactly one env borrower runs at a time (a
/// concurrent reader during `set_var` is a data race). Poison-proof: a
/// panicking holder must not wedge the rest of the suite.
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Lock [`ENV_LOCK`], tolerating a poisoned predecessor.
pub(crate) fn lock_env() -> std::sync::MutexGuard<'static, ()> {
  ENV_LOCK
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}
