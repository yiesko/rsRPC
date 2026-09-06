use serde_json::json;

use crate::cmd::ActivityCmd;

fn parse_cmd(json: &str) -> ActivityCmd {
  serde_json::from_str(json).expect("failed to parse test command")
}

#[test]
fn fix_buttons_maps_objects_to_labels_and_urls() {
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": {
            "name": "Test",
            "buttons": [
              { "label": "Join", "url": "https://example.com/join" },
              { "label": "Watch", "url": "https://example.com/watch" }
            ]
          }
        },
        "nonce": "n"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  assert_eq!(
    activity.buttons.unwrap(),
    vec![json!("Join"), json!("Watch")]
  );
  assert_eq!(
    activity.metadata.unwrap().button_urls.unwrap(),
    vec![
      "https://example.com/join".to_string(),
      "https://example.com/watch".to_string()
    ]
  );
}

#[test]
fn fix_buttons_keeps_plain_labels() {
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": {
            "name": "Test",
            "buttons": ["Play", "Details"]
          }
        },
        "nonce": "n"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  assert_eq!(
    activity.buttons.unwrap(),
    vec![json!("Play"), json!("Details")]
  );
  assert!(activity.metadata.is_none());
}

#[test]
fn fix_buttons_no_urls_leaves_metadata_untouched() {
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": {
            "name": "Test",
            "buttons": [{ "label": "NoUrl" }]
          }
        },
        "nonce": "n"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  assert_eq!(activity.buttons.unwrap(), vec![json!("NoUrl")]);
  assert!(activity.metadata.is_none());
}

#[test]
fn fix_buttons_no_buttons_is_noop() {
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": { "name": "Test" }
        },
        "nonce": "n"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  assert!(activity.buttons.is_none());
  assert!(activity.metadata.is_none());
}

#[test]
fn fix_timestamps_converts_seconds_to_millis() {
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": {
            "name": "Test",
            "timestamps": { "start": 1000000000, "end": 2000000000 }
          }
        },
        "nonce": "n"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  let timestamps = activity.timestamps.unwrap();
  assert_eq!(timestamps.start.unwrap().0, 1_000_000_000_000);
  assert_eq!(timestamps.end.unwrap().0, 2_000_000_000_000);
}

#[test]
fn fix_timestamps_keeps_millis_untouched() {
  let future_ms = chrono::Utc::now().timestamp() + (100 * 365 * 24 * 3600) + 10_000;
  let json = format!(
    r#"{{
        "cmd": "SET_ACTIVITY",
        "args": {{
          "pid": 42,
          "activity": {{
            "name": "Test",
            "timestamps": {{ "start": {} }}
          }}
        }},
        "nonce": "n"
      }}"#,
    future_ms
  );
  let mut cmd = parse_cmd(&json);

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  let timestamps = activity.timestamps.unwrap();
  assert_eq!(timestamps.start.unwrap().0, future_ms);
}

#[test]
fn fix_buttons_preserves_sober_roblox_payload() {
  // Shape inspired by a real Sober payload: both buttons must survive
  // fix() as labels + metadata.button_urls, including roblox:// URLs —
  // what Discord renders from there is Discord's decision, not ours.
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 3,
          "activity": {
            "state": "by ExamplePlayer",
            "details": "Playing Example Experience",
            "assets": {"large_image": "roblox_big", "large_text": "Roblox"},
            "buttons": [
              {"label": "Join server", "url": "roblox://experiences/start?placeId=1234567890&gameInstanceId=00000000-0000-0000-0000-000000000000"},
              {"label": "See game page", "url": "https://roblox.com/games/1234567890"}
            ],
            "instance": false
          }
        },
        "nonce": "7"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  assert_eq!(
    activity.buttons.unwrap(),
    vec![json!("Join server"), json!("See game page")]
  );
  assert_eq!(
    activity.metadata.unwrap().button_urls.unwrap(),
    vec![
      "roblox://experiences/start?placeId=1234567890&gameInstanceId=00000000-0000-0000-0000-000000000000".to_string(),
      "https://roblox.com/games/1234567890".to_string()
    ]
  );
}

#[test]
fn fix_flags_sets_instance_flag() {
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": {
            "name": "Test",
            "instance": true
          }
        },
        "nonce": "n"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  assert_eq!(activity.flags, Some(1));
}

#[test]
fn fix_flags_keeps_existing_flags() {
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": {
            "name": "Test",
            "instance": true,
            "flags": 8
          }
        },
        "nonce": "n"
      }"#,
  );

  cmd.fix();

  let activity = cmd.args.unwrap().activity.unwrap();
  assert_eq!(activity.flags, Some(8));
}

#[test]
fn bridge_payload_preserves_buttons_and_metadata() {
  // End-to-end through the bridge encoder (arrpc #141): what a game
  // sends must reach bridge clients untouched.
  let mut cmd = parse_cmd(
    r#"{
        "cmd": "SET_ACTIVITY",
        "args": {
          "pid": 42,
          "activity": {
            "name": "Test",
            "details": "Racing",
            "buttons": [
              { "label": "Join", "url": "https://example.com/join" },
              { "label": "Watch", "url": "https://example.com/watch" }
            ]
          }
        },
        "nonce": "n"
      }"#,
  );
  let cached = crate::commands::cached_activity(&mut cmd).expect("encodes");
  let payload: serde_json::Value = serde_json::from_str(&cached.json).expect("valid json");
  let activity = &payload["activity"];
  assert_eq!(activity["details"], "Racing");
  assert_eq!(activity["buttons"], json!(["Join", "Watch"]));
  assert_eq!(
    activity["metadata"]["button_urls"],
    json!(["https://example.com/join", "https://example.com/watch"])
  );
  // MessagePack twin carries the same activity.
  let decoded: serde_json::Value = rmp_serde::from_slice(&cached.msgpack).expect("valid msgpack");
  assert_eq!(decoded["activity"]["buttons"], json!(["Join", "Watch"]));
}
