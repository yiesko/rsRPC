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

/// Unique scratch dir under the temp dir, removed on drop — even when
/// the test panics mid-body (plain trailing `remove_dir_all` never runs
/// then, littering `/tmp` and risking cross-run collisions).
pub(crate) struct TempDir(std::path::PathBuf);

impl TempDir {
  pub(crate) fn new(tag: &str) -> Self {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
      "rsrpc-test-{}-{}-{}",
      std::process::id(),
      COUNTER.fetch_add(1, Ordering::Relaxed),
      tag
    ));
    std::fs::create_dir_all(&dir).expect("test scratch dir");
    Self(dir)
  }
}

impl std::ops::Deref for TempDir {
  type Target = std::path::PathBuf;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl Drop for TempDir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

/// Process-global env var set for a test body, restored on drop — even
/// on panic (a trailing restore never runs then, poisoning parallel
/// tests). Hold a [`lock_env`] guard alongside while using it: env
/// mutation is only sound single-threaded, which the lock guarantees.
///
/// # Safety
///
/// Constructing this touches process-global state; the caller must hold
/// [`lock_env`] for the whole lifetime (all current callers do).
pub(crate) struct EnvRestore {
  key: &'static str,
  previous: Option<String>,
}

impl EnvRestore {
  pub(crate) fn set(key: &'static str, value: &str) -> Self {
    let previous = std::env::var(key).ok();
    // SAFETY: by contract the caller holds `lock_env`, so no other
    // thread observes the environment concurrently.
    unsafe { std::env::set_var(key, value) };
    Self { key, previous }
  }
}

impl Drop for EnvRestore {
  fn drop(&mut self) {
    // SAFETY: same contract as construction (drop runs on the same
    // thread while the test's `lock_env` guard is still alive: guards
    // are declared before us, so they drop after us).
    unsafe {
      match &self.previous {
        Some(value) => std::env::set_var(self.key, value),
        None => std::env::remove_var(self.key),
      }
    }
  }
}
