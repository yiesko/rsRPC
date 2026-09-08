use crate::cmd::{ActivityCmd, ActivityCmdArgs};
use crate::server::client_connector::{
  ClientConnector, MAX_CACHED_ACTIVITIES, handle_bridge_control, is_genuine_clear, prune_cache,
  state_activities, take_process_clear,
};
use crate::user::RpcUser;

fn test_user() -> std::sync::Arc<std::sync::Mutex<RpcUser>> {
  std::sync::Arc::new(std::sync::Mutex::new(RpcUser::default()))
}

fn cmd_with(pid: Option<u64>, activity: Option<crate::cmd::Activity>) -> ActivityCmd {
  ActivityCmd {
    args: Some(ActivityCmdArgs {
      pid,
      activity,
      code: None,
      user_id: None,
    }),
    ..ActivityCmd::empty()
  }
}

#[test]
fn genuine_clear_needs_nonzero_pid_and_null_activity() {
  // Sober/Roblox disconnect: null activity, real pid -> genuine
  assert!(is_genuine_clear(&cmd_with(Some(3), None)));
  // SUBSCRIBE before any SET_ACTIVITY: pid 0 -> spurious, ignore
  assert!(!is_genuine_clear(&cmd_with(Some(0), None)));
  // Missing pid entirely -> ignore
  assert!(!is_genuine_clear(&cmd_with(None, None)));
  // No args at all -> ignore
  assert!(!is_genuine_clear(&ActivityCmd::empty()));
}

#[test]
fn clones_share_detection_state() {
  // ClientConnector::new binds bridge ports; use uncommon ones for the test.
  let (_ipc_tx, ipc_rx) = std::sync::mpsc::channel();
  let (_proc_tx, proc_rx) = std::sync::mpsc::channel();
  let (_ws_tx, ws_rx) = std::sync::mpsc::channel();
  let a = ClientConnector::new(45971, 45981, 45972, test_user(), ipc_rx, proc_rx, ws_rx);
  let b = a.clone();

  // A reset on one clone (event_loop) must be visible on the other
  // (process_loop) — plain Option fields would silently diverge here.
  *b.active_socket.lock().unwrap() = Some("123456789012345678".to_string());
  assert_eq!(
    a.active_socket.lock().unwrap().clone(),
    Some("123456789012345678".to_string())
  );
  *a.active_socket.lock().unwrap() = None;
  assert!(b.active_socket.lock().unwrap().is_none());

  *b.last_pid.lock().unwrap() = Some(42);
  assert_eq!(*a.last_pid.lock().unwrap(), Some(42));

  *b.last_process.lock().unwrap() = Some("123456789012345678".to_string());
  assert_eq!(
    a.last_process.lock().unwrap().clone(),
    Some("123456789012345678".to_string())
  );
}

#[test]
fn non_empty_set_activity_is_not_a_clear() {
  // Real Sober/Roblox payload shape (truncated): must not reset detection
  let cmd: ActivityCmd = serde_json::from_str(
    r#"{"nonce":"4","cmd":"SET_ACTIVITY","args":{"pid":3,"activity":{"details":"In the Roblox app"}}}"#,
  )
  .unwrap();
  assert!(!is_genuine_clear(&cmd));
}

#[test]
fn process_clear_consumes_outstanding_publication_once() {
  // ClientConnector::new binds bridge ports; use uncommon ones for the test.
  let (_ipc_tx, ipc_rx) = std::sync::mpsc::channel();
  let (_proc_tx, proc_rx) = std::sync::mpsc::channel();
  let (_ws_tx, ws_rx) = std::sync::mpsc::channel();
  let connector = ClientConnector::new(45973, 45983, 45974, test_user(), ipc_rx, proc_rx, ws_rx);

  // Nothing published: null scans skip.
  assert_eq!(take_process_clear(&connector), None);

  // A process publication arms exactly one clear, with the last pid.
  *connector.last_process.lock().unwrap() = Some("111111111111111111".to_string());
  *connector.last_pid.lock().unwrap() = Some(1234);
  assert_eq!(
    take_process_clear(&connector),
    Some((1234, "111111111111111111".to_string()))
  );
  // Consumed: further null scans skip (no clear spam, no re-clear).
  assert_eq!(take_process_clear(&connector), None);
}

#[test]
fn process_clear_survives_sdk_clear_reset() {
  // Regression: the game SDK disconnect clears active_socket (event_loop
  // path) while the app-id-keyed process entry is still live. The old
  // null-scan gate (active_socket) then skipped the prune forever and
  // every later bridge client replayed the dead game.
  let (_ipc_tx, ipc_rx) = std::sync::mpsc::channel();
  let (_proc_tx, proc_rx) = std::sync::mpsc::channel();
  let (_ws_tx, ws_rx) = std::sync::mpsc::channel();
  let connector = ClientConnector::new(45975, 45985, 45976, test_user(), ipc_rx, proc_rx, ws_rx);

  *connector.last_process.lock().unwrap() = Some("111111111111111111".to_string());
  *connector.last_pid.lock().unwrap() = Some(1234);
  // SDK clear already reset the shared flag...
  *connector.active_socket.lock().unwrap() = None;
  // ...yet the process publication still yields its one clear.
  assert_eq!(
    take_process_clear(&connector),
    Some((1234, "111111111111111111".to_string()))
  );
}

#[test]
fn replay_cache_evicts_oldest_beyond_cap() {
  use crate::commands::empty_cached;

  let mut cache = std::collections::HashMap::new();
  for i in 0..=(MAX_CACHED_ACTIVITIES as u64) {
    cache.insert(format!("pid-{i}"), (empty_cached(1, format!("pid-{i}")), i));
  }
  assert_eq!(cache.len(), MAX_CACHED_ACTIVITIES + 1);

  prune_cache(&mut cache);

  assert_eq!(cache.len(), MAX_CACHED_ACTIVITIES);
  // Oldest (seq 0) evicted, newest kept.
  assert!(!cache.contains_key("pid-0"));
  assert!(cache.contains_key(&format!("pid-{}", MAX_CACHED_ACTIVITIES)));
}

#[test]
fn replay_cache_within_cap_is_untouched() {
  use crate::commands::empty_cached;

  let mut cache = std::collections::HashMap::new();
  cache.insert("a".to_string(), (empty_cached(1, "a".to_string()), 7));
  prune_cache(&mut cache);
  assert!(cache.contains_key("a"));
}

#[test]
fn set_user_patches_identity_and_acks() {
  let user = test_user();
  let (ack_text, changed) = handle_bridge_control(
    &user,
    r#"{"type":"SET_USER","nonce":3,"patch":{"username":"web"}}"#,
  )
  .expect("ack");
  let ack: serde_json::Value = serde_json::from_str(&ack_text).expect("valid json");

  assert_eq!(ack["type"], "SET_USER_ACK");
  assert_eq!(ack["nonce"], 3);
  assert_eq!(ack["data"]["success"], true);
  assert_eq!(ack["data"]["user"]["username"], "web");
  assert_eq!(user.lock().unwrap().username, "web");
  // The changed identity is reported for CURRENT_USER_UPDATE fan-out.
  assert_eq!(changed.expect("changed").username, "web");
}

#[test]
fn set_user_without_changes_reports_no_fanout() {
  let user = test_user();
  let (ack_text, changed) =
    handle_bridge_control(&user, r#"{"type":"SET_USER","patch":{"username":"arRPC"}}"#)
      .expect("ack");

  let ack: serde_json::Value = serde_json::from_str(&ack_text).expect("valid json");
  assert_eq!(ack["type"], "SET_USER_ACK");
  // Same identity: ACKed, but no DISPATCH needed.
  assert!(changed.is_none());
}

#[test]
fn reset_user_restores_startup_identity() {
  let user = test_user();
  user.lock().unwrap().username = "changed".to_string();

  let (ack_text, changed) = handle_bridge_control(&user, r#"{"type":"RESET_USER"}"#).expect("ack");
  let ack: serde_json::Value = serde_json::from_str(&ack_text).expect("valid json");

  assert_eq!(ack["type"], "RESET_USER_ACK");
  assert_eq!(user.lock().unwrap().username, "arRPC");
  assert!(changed.is_some());
}

#[test]
fn non_control_messages_pass_through_untouched() {
  let user = test_user();
  // Activity payloads and unknown types are not control: no ACK.
  assert!(handle_bridge_control(&user, r#"{"activity":null,"pid":1}"#).is_none());
  assert!(handle_bridge_control(&user, r#"{"type":"GET_IGNORED_GAMES"}"#).is_none());
  assert!(handle_bridge_control(&user, "not json").is_none());
  // Identity untouched by pass-through.
  assert_eq!(user.lock().unwrap().username, "arRPC");
}

#[test]
fn state_activities_flatten_replay_cache() {
  use crate::cmd::ActivityCmd;
  use crate::commands::cached_activity;

  let mut cmd: ActivityCmd = serde_json::from_str(
    r#"{"cmd":"SET_ACTIVITY","nonce":"n","application_id":"111","args":{"pid":9,"activity":{"name":"Game","timestamps":{"start":5}}}}"#,
  )
  .expect("parse");
  let payload = cached_activity(&mut cmd).expect("encodes");
  let mut cache = std::collections::HashMap::new();
  cache.insert("9".to_string(), (payload, 1));

  let activities = state_activities(&cache);
  assert_eq!(activities.len(), 1);
  assert_eq!(activities[0].socket_id, "9");
  assert_eq!(activities[0].name.as_deref(), Some("Game"));
  assert_eq!(activities[0].application_id.as_deref(), Some("111"));
  assert_eq!(activities[0].pid, Some(9));
}

#[test]
fn handoff_suppresses_while_ipc_live_and_resumes_on_owner_clear() {
  use crate::server::client_connector::{HandoffState, ScannedGame};

  let game = ScannedGame {
    id: "111111111111111111".to_string(),
    name: "Game".to_string(),
    pid: 1234,
    start: "0".to_string(),
  };
  let mut handoff = HandoffState::default();
  assert!(!handoff.suppresses(&game.id));

  // A live SDK presence takes the slot.
  handoff.note_publish(&game.id, 77);
  assert!(handoff.suppresses(&game.id));

  // A clear from a *different* pid (superseded companion) is ignored.
  handoff.note_scan(Some(game.clone()));
  assert!(!handoff.note_clear(&game.id, 78));
  assert!(handoff.suppresses(&game.id));

  // The owner's clear releases it, and the scan still reports the game.
  assert!(handoff.note_clear(&game.id, 77));
  assert!(!handoff.suppresses(&game.id));
  assert_eq!(handoff.resume_for(&game.id), Some(game));
}

#[test]
fn handoff_takeover_last_publisher_wins() {
  use crate::server::client_connector::HandoffState;

  let mut handoff = HandoffState::default();
  handoff.note_publish("1", 10);
  // Companion B takes over: A's pid is forgotten, no leak.
  handoff.note_publish("1", 20);
  // A's late close must not resume the generic card under B.
  assert!(!handoff.note_clear("1", 10));
  assert!(handoff.suppresses("1"));
  // B's close releases.
  assert!(handoff.note_clear("1", 20));
  assert!(!handoff.suppresses("1"));
}

#[test]
fn handoff_resume_only_matches_scanned_game() {
  use crate::server::client_connector::{HandoffState, ScannedGame};

  let mut handoff = HandoffState::default();
  handoff.note_publish("1", 10);
  handoff.note_scan(None);
  assert!(handoff.note_clear("1", 10));
  // Scanner reports nothing: nothing to resume.
  assert_eq!(handoff.resume_for("1"), None);

  handoff.note_scan(Some(ScannedGame {
    id: "2".to_string(),
    name: "Other".to_string(),
    pid: 9,
    start: "0".to_string(),
  }));
  // A different game on screen: not ours to resume.
  assert_eq!(handoff.resume_for("1"), None);
}

#[test]
fn process_alive_rejects_zero_and_dead_pids() {
  use crate::server::client_connector::process_alive;

  assert!(!process_alive(0));
  assert!(process_alive(std::process::id() as u64));
  assert!(!process_alive(u64::MAX));
}

#[test]
fn generic_payload_carries_scanned_identity() {
  use crate::server::client_connector::{ScannedGame, generic_payload};

  let payload = generic_payload(&ScannedGame {
    id: "111111111111111111".to_string(),
    name: "Game".to_string(),
    pid: 1234,
    start: "7".to_string(),
  });
  let body: serde_json::Value = serde_json::from_str(&payload.json).expect("valid json");
  assert_eq!(body["socketId"], "111111111111111111");
  assert_eq!(body["activity"]["application_id"], "111111111111111111");
  assert_eq!(body["activity"]["name"], "Game");
  assert_eq!(body["activity"]["timestamps"]["start"], "7");
  assert_eq!(body["pid"], 1234);
}
