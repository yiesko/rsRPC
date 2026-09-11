use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static LOGS_ENABLED: AtomicBool = AtomicBool::new(false);
static LOGS_INIT: Once = Once::new();
static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);
static LEVEL_INIT: Once = Once::new();

/// Severity, from chattiest to most urgent. Ordering matters: the
/// configured threshold prints a level if and only if it is at least as
/// severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
  Debug,
  Info,
  Warn,
  Error,
}

/// Parse a level name (`debug|info|warn|error`), case-insensitive.
/// Pure mapping, unit-tested; environment reading stays in [`threshold`].
pub(crate) fn parse_level(name: &str) -> Option<Level> {
  match name.trim().to_ascii_lowercase().as_str() {
    "debug" | "trace" => Some(Level::Debug),
    "info" => Some(Level::Info),
    "warn" | "warning" => Some(Level::Warn),
    "error" | "err" => Some(Level::Error),
    _ => None,
  }
}

/// Boolish env values (`1/true/yes/on`, case-insensitive): mirrors the
/// CLI `BoolishValueParser` so `RSRPC_DEBUG=true` also works for direct
/// library users (previously only the literal `"1"` counted).
pub(crate) fn env_bool(name: &str) -> bool {
  matches!(
    std::env::var(name)
      .unwrap_or_default()
      .trim()
      .to_ascii_lowercase()
      .as_str(),
    "1" | "true" | "yes" | "on"
  )
}
/// Configured minimum severity. `RSRPC_LOG_LEVEL` names it (default
/// `info`); `RSRPC_DEBUG=1` (or `--debug`) forces `debug`, whichever is
/// chattier. Read once; daemon log level never changes at runtime.
fn threshold() -> Level {
  LEVEL_INIT.call_once(|| {
    let mut level = std::env::var("RSRPC_LOG_LEVEL")
      .ok()
      .and_then(|name| parse_level(&name))
      .unwrap_or(Level::Info);
    if env_bool("RSRPC_DEBUG") {
      level = std::cmp::min(level, Level::Debug);
    }
    LEVEL.store(level as u8, Ordering::Relaxed);
  });
  match LEVEL.load(Ordering::Relaxed) {
    0 => Level::Debug,
    2 => Level::Warn,
    3 => Level::Error,
    _ => Level::Info,
  }
}

/// Whether log lines currently print. Checked by the [`log!`] macro
/// *before* formatting, so disabled logging costs one atomic load and no
/// allocation (previously every call formatted eagerly).
pub fn enabled() -> bool {
  LOGS_INIT.call_once(|| {
    if env_bool("RSRPC_LOGS_ENABLED") {
      LOGS_ENABLED.store(true, Ordering::Relaxed);
    }
  });
  LOGS_ENABLED.load(Ordering::Relaxed)
}

/// Whether `level` prints right now: needs the master switch on plus
/// meeting the threshold.
pub fn level_enabled(level: Level) -> bool {
  enabled() && level >= threshold()
}

/// Whether per-tick debug lines print (`RSRPC_DEBUG=1`, also `--debug`).
/// High-frequency internals (scan ticks, repeat sends) live here so the
/// default log stays readable: one line per state change, not per tick.
pub fn debug_enabled() -> bool {
  level_enabled(Level::Debug)
}

fn emit_tagged(tag: &str, message: &str) {
  println!(
    "[{}] [{}] {}",
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
    tag,
    message
  );
}

pub fn log(message: impl AsRef<str>) {
  if enabled() {
    // Historical shape (no level tag) so existing log scrapers keep working.
    println!(
      "[{}] {}",
      chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
      message.as_ref()
    );
  }
}

pub fn debug(message: impl AsRef<str>) {
  if debug_enabled() {
    emit_tagged("DEBUG", message.as_ref());
  }
}

pub fn warn(message: impl AsRef<str>) {
  if level_enabled(Level::Warn) {
    emit_tagged("WARN", message.as_ref());
  }
}

pub fn error(message: impl AsRef<str>) {
  if level_enabled(Level::Error) {
    emit_tagged("ERROR", message.as_ref());
  }
}

// NOTE: INFO keeps the historical `[timestamp] message` shape (no level
// tag) so existing log scrapers keep working; every other level is tagged.

#[macro_export]
macro_rules! log {
  ($($arg:tt)*) => {
    if $crate::logger::enabled() {
      $crate::logger::log(format!($($arg)*))
    }
  };
}

#[macro_export]
macro_rules! debug {
  ($($arg:tt)*) => {
    if $crate::logger::debug_enabled() {
      $crate::logger::debug(format!($($arg)*))
    }
  };
}

#[macro_export]
macro_rules! warn {
  ($($arg:tt)*) => {
    if $crate::logger::level_enabled($crate::logger::Level::Warn) {
      $crate::logger::warn(format!($($arg)*))
    }
  };
}

#[macro_export]
macro_rules! error {
  ($($arg:tt)*) => {
    if $crate::logger::level_enabled($crate::logger::Level::Error) {
      $crate::logger::error(format!($($arg)*))
    }
  };
}
