use std::sync::mpsc;

use crate::server::ipc_utils::{MAX_IPC_PAYLOAD, PacketType, close_frame, send_empty};

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

#[test]
fn unknown_packet_types_are_refused_not_misread_as_frames() {
  assert!(PacketType::try_from_u32(0).is_some());
  assert!(PacketType::try_from_u32(4).is_some());
  assert!(PacketType::try_from_u32(5).is_none());
  assert!(PacketType::try_from_u32(u32::MAX).is_none());
}

#[test]
fn payload_limit_matches_discord_1mib() {
  assert_eq!(MAX_IPC_PAYLOAD, 1024 * 1024);
}

#[test]
fn close_frame_carries_code_and_message() {
  let frame = close_frame(1003, "Payload too large");
  let r_type = u32::from_le_bytes(frame[0..4].try_into().expect("header"));
  let len = u32::from_le_bytes(frame[4..8].try_into().expect("header")) as usize;
  assert_eq!(r_type, 2);
  assert_eq!(len, frame.len() - 8);
  let body: serde_json::Value = serde_json::from_slice(&frame[8..]).expect("json close body");
  assert_eq!(body["code"], 1003);
  assert_eq!(body["message"], "Payload too large");
}
