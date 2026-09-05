//! Integration tests for the rsrpc library: configuration defaults, protocol
//! round-trips (JSON and MessagePack), payload equivalence, and response
//! shapes. Mirrors the coverage of pog5-rsrpc's tests.

use rsrpc::RPCConfig;
use rsrpc::cmd::ActivityCmd;
use rsrpc::commands::{cached_activity, set_activity_response};

const SET_ACTIVITY_WITH_BUTTONS: &str = r#"{
  "cmd": "SET_ACTIVITY",
  "args": {
    "pid": 42,
    "activity": {
      "name": "Test",
      "details": "Playing",
      "buttons": [
        { "label": "Join", "url": "https://example.com/join" },
        { "label": "Watch", "url": "https://example.com/watch" }
      ],
      "timestamps": { "start": 1000000000 }
    }
  },
  "nonce": "n1"
}"#;

fn parse_cmd(json: &str) -> ActivityCmd {
  serde_json::from_str(json).expect("failed to parse test command")
}

#[test]
fn config_defaults_match_reference_servers() {
  let config = RPCConfig::default();

  assert_eq!(config.port, 1337, "bridge port");
  assert_eq!(config.msgpack_port, 1338, "MessagePack bridge port");
  assert_eq!(config.ws_port_start, 6463, "websocket range start");
  assert_eq!(config.ws_port_end, 6472, "websocket range end (inclusive)");
  assert_eq!(config.scan_interval_secs, 5, "process scan interval");
  assert!(config.enable_process_scanner);
  assert!(config.enable_ipc_connector);
  assert!(config.enable_websocket_connector);
  assert!(config.enable_secondary_events);
  assert_eq!(config.db_url, None);
  assert!(!config.enable_db_update);
}

#[test]
fn cached_activity_roundtrips_via_json() {
  let mut cmd = parse_cmd(SET_ACTIVITY_WITH_BUTTONS);
  let cached = cached_activity(&mut cmd).expect("activity payload");

  let payload: rsrpc::cmd::ActivityPayload = serde_json::from_str(&cached.json).unwrap();

  assert_eq!(payload.pid, Some(42));
  assert_eq!(payload.socket_id.as_deref(), Some("42"));
  let activity = payload.activity.expect("activity present");
  assert_eq!(activity.name.as_deref(), Some("Test"));
  assert_eq!(activity.details.as_deref(), Some("Playing"));
  assert_eq!(
    activity.buttons,
    Some(vec![serde_json::json!("Join"), serde_json::json!("Watch")])
  );
  assert_eq!(
    activity.metadata.unwrap().button_urls,
    Some(vec![
      "https://example.com/join".to_string(),
      "https://example.com/watch".to_string()
    ])
  );
}

#[test]
fn cached_activity_roundtrips_via_msgpack() {
  let mut cmd = parse_cmd(SET_ACTIVITY_WITH_BUTTONS);
  let cached = cached_activity(&mut cmd).expect("activity payload");

  let payload: rsrpc::cmd::ActivityPayload = rmp_serde::from_slice(&cached.msgpack).unwrap();

  assert_eq!(payload.pid, Some(42));
  assert_eq!(payload.socket_id.as_deref(), Some("42"));
  let activity = payload.activity.expect("activity present");
  assert_eq!(activity.name.as_deref(), Some("Test"));
  assert_eq!(
    activity.buttons,
    Some(vec![serde_json::json!("Join"), serde_json::json!("Watch")])
  );
}

#[test]
fn json_and_msgpack_payloads_are_equivalent() {
  let mut cmd = parse_cmd(SET_ACTIVITY_WITH_BUTTONS);
  let cached = cached_activity(&mut cmd).expect("activity payload");

  let json_payload: serde_json::Value = serde_json::from_str(&cached.json).unwrap();
  let msgpack_payload: serde_json::Value = rmp_serde::from_slice(&cached.msgpack).unwrap();

  assert_eq!(json_payload, msgpack_payload, "both protocols agree");
}

#[test]
fn msgpack_is_never_larger_than_json() {
  let mut cmd = parse_cmd(SET_ACTIVITY_WITH_BUTTONS);
  let cached = cached_activity(&mut cmd).expect("activity payload");

  assert!(
    cached.msgpack.len() <= cached.json.len(),
    "msgpack ({}) should be <= json ({})",
    cached.msgpack.len(),
    cached.json.len()
  );
}

#[test]
fn clear_activity_payload_is_null() {
  let mut cmd = parse_cmd(
    r#"{
      "cmd": "SET_ACTIVITY",
      "args": { "pid": 7, "activity": null },
      "nonce": "clear"
    }"#,
  );
  let cached = cached_activity(&mut cmd).expect("clear payload");

  let json_payload: serde_json::Value = serde_json::from_str(&cached.json).unwrap();
  assert!(json_payload["activity"].is_null());
  assert_eq!(json_payload["pid"], 7);
  assert_eq!(json_payload["socketId"], "7");

  let msgpack_payload: serde_json::Value = rmp_serde::from_slice(&cached.msgpack).unwrap();
  assert_eq!(json_payload, msgpack_payload);
}

#[test]
fn set_activity_response_has_arrpc_shape() {
  let mut cmd = parse_cmd(SET_ACTIVITY_WITH_BUTTONS);
  cmd.fix();

  let response = set_activity_response(&cmd).expect("response");
  let value: serde_json::Value = serde_json::from_str(&response).unwrap();

  assert_eq!(value["cmd"], "SET_ACTIVITY");
  assert!(value["evt"].is_null());
  assert_eq!(value["nonce"], "n1");
  assert_eq!(value["data"]["name"], "");
  assert_eq!(value["data"]["type"], 0);
  assert_eq!(
    value["data"]["buttons"],
    serde_json::json!(["Join", "Watch"])
  );
  assert_eq!(value["data"]["details"], "Playing");
  assert_eq!(value["data"]["timestamps"]["start"], 1_000_000_000_000i64);
}

#[test]
fn set_activity_response_clear_has_null_data() {
  let mut cmd = parse_cmd(
    r#"{
      "cmd": "SET_ACTIVITY",
      "args": { "pid": 7, "activity": null },
      "nonce": "clear"
    }"#,
  );
  cmd.fix();

  let response = set_activity_response(&cmd).expect("response");
  let value: serde_json::Value = serde_json::from_str(&response).unwrap();

  assert_eq!(value["cmd"], "SET_ACTIVITY");
  assert!(value["data"].is_null());
  assert!(value["evt"].is_null());
}

#[test]
fn missing_args_yields_no_activity_payload() {
  let mut cmd = parse_cmd(
    r#"{
      "cmd": "SET_ACTIVITY",
      "nonce": "nope"
    }"#,
  );

  assert!(cached_activity(&mut cmd).is_none());
  assert!(set_activity_response(&cmd).is_none());
}
