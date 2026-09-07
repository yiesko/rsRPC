use crate::{RPCConfig, RPCServer};

const TINY_DB: &str = r#"[
  {"id": "111111111111111111", "name": "First Test Game", "hook": true},
  {"id": "222222222222222222", "name": "Second Test Game", "hook": true}
]"#;

#[test]
fn take_detectables_moves_without_duplicating() {
  let mut server = RPCServer::from_json_str(TINY_DB, RPCConfig::default()).unwrap();
  assert_eq!(server.detectable.lock().unwrap().len(), 2);

  let moved = server.take_detectables();
  assert_eq!(moved.len(), 2);
  assert_eq!(moved[0].id, "111111111111111111");
  // Single ownership: nothing stays behind to pin a second generation.
  assert!(server.detectable.lock().unwrap().is_empty());

  // Second take finds nothing (idempotent, never duplicates).
  assert!(server.take_detectables().is_empty());
}
