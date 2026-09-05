use std::sync::mpsc;

use crate::server::ipc_utils::send_empty;

#[test]
fn send_empty_routes_as_set_activity_clear() {
  // Regression: disconnect clears must carry cmd == SET_ACTIVITY, or
  // event_loop misroutes them to broadcast_raw and the presence (and the
  // bridge replay cache) is never cleared — stuck card forever.
  let (mut tx, rx) = mpsc::channel();
  send_empty(&mut tx, 3).unwrap();
  let cmd = rx.try_recv().unwrap();
  assert_eq!(cmd.cmd, "SET_ACTIVITY");
  let args = cmd.args.unwrap();
  assert_eq!(args.pid, Some(3));
  assert!(args.activity.is_none());
}
