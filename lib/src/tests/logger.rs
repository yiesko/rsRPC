use crate::logger::{Level, parse_level};

#[test]
fn parse_level_accepts_names_case_insensitively() {
  assert_eq!(parse_level("debug"), Some(Level::Debug));
  assert_eq!(parse_level("TRACE"), Some(Level::Debug));
  assert_eq!(parse_level("Info"), Some(Level::Info));
  assert_eq!(parse_level("warning"), Some(Level::Warn));
  assert_eq!(parse_level("ERR"), Some(Level::Error));
  assert_eq!(parse_level("  warn  "), Some(Level::Warn));
  assert_eq!(parse_level("verbose"), None);
  assert_eq!(parse_level(""), None);
}

#[test]
fn severity_orders_debug_below_error() {
  assert!(Level::Debug < Level::Info);
  assert!(Level::Info < Level::Warn);
  assert!(Level::Warn < Level::Error);
}
