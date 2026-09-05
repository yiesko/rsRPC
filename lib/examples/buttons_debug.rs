//! Temporary debug helper: print the full broadcast payload for a
//! reconstructed Roblox SET_ACTIVITY (buttons + assets), mirroring what the
//! bridge sends to web clients. Compare against arrpc's emitted payload.

use rsrpc::cmd::ActivityCmd;
use rsrpc::commands::cached_activity;

fn main() {
  let raw = r#"{
    "nonce": "6",
    "cmd": "SET_ACTIVITY",
    "args": {
      "pid": 3,
      "activity": {
        "state": "by ExamplePlayer",
        "details": "Playing Example Experience",
        "timestamps": { "start": 1787096769 },
        "assets": {
          "large_image": "img_example-icon--hash",
          "large_text": "Example Experience",
          "small_image": "img_example-icon--hash2",
          "small_text": "Online"
        },
        "party": { "id": "abc", "size": [1, 10] },
        "secrets": { "join": "xyz" },
        "buttons": [
          { "label": "Join server", "url": "https://example.com/join" },
          { "label": "See game page", "url": "https://example.com/game" }
        ]
      }
    }
  }"#;

  let mut cmd: ActivityCmd = serde_json::from_str(raw).expect("parse");
  cmd.application_id = Some("123456789012345678".to_string());

  let cached = cached_activity(&mut cmd).expect("payload");
  println!("JSON:\n{}", cached.json);
  println!("\nMSGPACK bytes: {}", cached.msgpack.len());
}
