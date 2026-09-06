use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

static LOGS_ENABLED: AtomicBool = AtomicBool::new(false);
static LOGS_INIT: Once = Once::new();

/// Whether log lines currently print. Checked by the [`log!`] macro
/// *before* formatting, so disabled logging costs one atomic load and no
/// allocation (previously every call formatted eagerly).
pub fn enabled() -> bool {
  LOGS_INIT.call_once(|| {
    if std::env::var("RSRPC_LOGS_ENABLED").unwrap_or_else(|_| "0".to_string()) == "1" {
      LOGS_ENABLED.store(true, Ordering::Relaxed);
    }
  });
  LOGS_ENABLED.load(Ordering::Relaxed)
}

pub fn log(message: impl AsRef<str>) {
  if enabled() {
    println!(
      "[{}] {}",
      chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
      message.as_ref()
    );
  }
}

#[macro_export]
macro_rules! log {
  ($($arg:tt)*) => {
    if $crate::logger::enabled() {
      $crate::logger::log(format!($($arg)*))
    }
  };
}
