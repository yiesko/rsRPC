//! Tests for user override loading (pure file IO under unique temp dirs).

use std::path::PathBuf;

use crate::overrides::{default_dir_path, default_file_path, load_dir, load_file, parse_overrides};

const GAME_ARRAY: &str = r#"[{
  "id": "111", "name": "Array Game", "hook": true,
  "executables": [{"name": "arraygame.exe", "is_launcher": false, "os": "win32"}]
}]"#;

const GAME_OBJECT: &str = r#"{
  "id": "222", "name": "Object Game", "hook": false,
  "executables": [{"name": "objectgame", "is_launcher": false, "os": "linux"}]
}"#;

fn temp_dir(tag: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("rsrpc-overrides-{}-{tag}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

#[test]
fn parse_accepts_array_and_single_object() {
  let array = parse_overrides(GAME_ARRAY).unwrap();
  assert_eq!(array.len(), 1);
  assert_eq!(array[0].id, "111");
  assert_eq!(array[0].executables.as_ref().unwrap().len(), 1);

  let single = parse_overrides(GAME_OBJECT).unwrap();
  assert_eq!(single.len(), 1);
  assert_eq!(single[0].id, "222");

  // Neither shape: error, never panic.
  assert!(parse_overrides("{\"id\": }").is_err());
  assert!(parse_overrides("[1, 2]").is_err());
}

#[test]
fn load_dir_merges_sorted_and_skips_bad_files() {
  let dir = temp_dir("dir");
  std::fs::write(dir.join("b-second.json"), GAME_ARRAY).unwrap();
  std::fs::write(dir.join("a-first.json"), GAME_OBJECT).unwrap();
  std::fs::write(dir.join("c-broken.json"), "{nope").unwrap();
  std::fs::write(dir.join("notes.txt"), "not json, ignored").unwrap();

  let merged = load_dir(&dir);
  // Sorted by file name (a before b), broken + non-json skipped.
  let ids: Vec<_> = merged.iter().map(|game| game.id.clone()).collect();
  assert_eq!(ids, vec!["222".to_string(), "111".to_string()]);

  // Missing dir/file degrade to empty, never error.
  assert!(load_dir(&dir.join("absent")).is_empty());
  assert!(load_file(&dir.join("absent.json")).unwrap().is_empty());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_paths_honor_env() {
  // Save, override, restore: env is process-global (serialized with the
  // Steam cache test via crate::tests::lock_env).
  let _guard = crate::tests::lock_env();
  let previous_file = std::env::var("RSRPC_OVERRIDES_FILE").ok();
  let previous_dir = std::env::var("RSRPC_OVERRIDES_DIR").ok();
  unsafe {
    std::env::set_var("RSRPC_OVERRIDES_FILE", "/tmp/custom-overrides.json");
    std::env::set_var("RSRPC_OVERRIDES_DIR", "/tmp/custom-overrides.d");
  }
  assert_eq!(
    default_file_path(),
    PathBuf::from("/tmp/custom-overrides.json")
  );
  assert_eq!(default_dir_path(), PathBuf::from("/tmp/custom-overrides.d"));
  unsafe {
    match previous_file {
      Some(value) => std::env::set_var("RSRPC_OVERRIDES_FILE", value),
      None => std::env::remove_var("RSRPC_OVERRIDES_FILE"),
    }
    match previous_dir {
      Some(value) => std::env::set_var("RSRPC_OVERRIDES_DIR", value),
      None => std::env::remove_var("RSRPC_OVERRIDES_DIR"),
    }
  }
  // Without env: XDG-shaped defaults (values depend on the machine, shape
  // is what matters).
  assert!(default_file_path().ends_with("rsrpc/overrides.json"));
  assert!(default_dir_path().ends_with("rsrpc/overrides.d"));
}
