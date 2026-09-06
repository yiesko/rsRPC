use crate::cmd::{ActivityCmd, ActivityCmdArgs};
use crate::server::client_connector::{ClientConnector, is_genuine_clear, take_process_clear};

fn cmd_with(pid: Option<u64>, activity: Option<crate::cmd::Activity>) -> ActivityCmd {
  ActivityCmd {
    args: Some(ActivityCmdArgs {
      pid,
      activity,
      code: None,
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
  let a = ClientConnector::new(45971, 45972, String::new(), ipc_rx, proc_rx, ws_rx);
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
  let connector = ClientConnector::new(45973, 45974, String::new(), ipc_rx, proc_rx, ws_rx);

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
  let connector = ClientConnector::new(45975, 45976, String::new(), ipc_rx, proc_rx, ws_rx);

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
