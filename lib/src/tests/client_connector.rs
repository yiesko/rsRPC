use crate::cmd::{ActivityCmd, ActivityCmdArgs};
use crate::server::client_connector::{ClientConnector, is_genuine_clear};

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
