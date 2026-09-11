use serde_json::Value;

use crate::cmd::ActivityCmd;
use crate::commands::{
  cached_activity, current_user_update, generic_ack, rpc_error, subscribe_ack, unsupported_command,
  user_response,
};
use crate::user::RpcUser;

fn parse_cmd(json: &str) -> ActivityCmd {
  serde_json::from_str(json).expect("failed to parse test command")
}

#[test]
fn subscribe_ack_echoes_cmd_evt_and_nonce() {
  let cmd =
    parse_cmd(r#"{"cmd":"SUBSCRIBE","evt":"VOICE_STATE_UPDATE","nonce":"abc","args":null}"#);

  let reply: Value = serde_json::from_str(&subscribe_ack(&cmd)).expect("valid json");

  assert_eq!(reply["cmd"], "SUBSCRIBE");
  assert_eq!(reply["data"]["evt"], "VOICE_STATE_UPDATE");
  assert!(reply["evt"].is_null());
  assert_eq!(reply["nonce"], "abc");
}

#[test]
fn rpc_error_carries_code_and_message() {
  let reply: Value = serde_json::from_str(&rpc_error(
    "INVITE_BROWSER",
    &Value::String("n".to_string()),
    4011,
    "Invalid invite id",
  ))
  .expect("valid json");

  assert_eq!(reply["cmd"], "INVITE_BROWSER");
  assert_eq!(reply["data"]["code"], 4011);
  assert_eq!(reply["data"]["message"], "Invalid invite id");
  assert_eq!(reply["evt"], "ERROR");
  assert_eq!(reply["nonce"], "n");
}

#[test]
fn generic_ack_confirms_receipt_without_claiming_success() {
  let cmd = parse_cmd(r#"{"cmd":"DEEP_LINK","nonce":"7","args":null}"#);

  let reply: Value = serde_json::from_str(&generic_ack(&cmd)).expect("valid json");

  assert_eq!(reply["cmd"], "DEEP_LINK");
  assert!(reply["data"].is_null());
  assert!(reply["evt"].is_null());
  assert_eq!(reply["nonce"], "7");
}

#[test]
fn user_response_returns_identity_or_null() {
  let cmd = parse_cmd(r#"{"cmd":"GET_USER","nonce":"g1","args":null}"#);
  let user = RpcUser::default();

  let hit: Value = serde_json::from_str(&user_response(&cmd, Some(&user))).expect("valid json");
  assert_eq!(hit["cmd"], "GET_USER");
  assert_eq!(hit["data"]["id"], "1045800378228281345");
  assert!(hit["evt"].is_null());
  assert_eq!(hit["nonce"], "g1");

  // Unknown id: official "user object or null".
  let miss: Value = serde_json::from_str(&user_response(&cmd, None)).expect("valid json");
  assert!(miss["data"].is_null());
}

#[test]
fn current_user_update_is_a_dispatch_with_the_user_inside() {
  let frame: Value =
    serde_json::from_str(&current_user_update(&RpcUser::default())).expect("valid json");

  assert_eq!(frame["cmd"], "DISPATCH");
  assert_eq!(frame["evt"], "CURRENT_USER_UPDATE");
  assert_eq!(frame["data"]["username"], "arRPC");
  assert!(frame["nonce"].is_null());
}

#[test]
fn unsupported_commands_map_to_official_codes() {
  // OAuth needs the in-client flow + app secret.
  assert_eq!(
    unsupported_command("AUTHORIZE"),
    Some((5000, "Authorization requires the real Discord client"))
  );
  assert_eq!(
    unsupported_command("AUTHENTICATE"),
    Some((5000, "Authorization requires the real Discord client"))
  );
  // Invites target live sessions on the real client.
  assert_eq!(
    unsupported_command("SEND_ACTIVITY_JOIN_INVITE"),
    Some((
      5006,
      "No eligible activity: invites require the real Discord client"
    ))
  );
  // Voice/guilds/overlay/store: real-client state only.
  let (code, _) = unsupported_command("SELECT_VOICE_CHANNEL").expect("known");
  assert_eq!(code, 1000);
  let (code, _) = unsupported_command("GET_GUILDS").expect("known");
  assert_eq!(code, 1000);
  // Handled elsewhere or genuinely unknown: no entry.
  assert!(unsupported_command("SET_ACTIVITY").is_none());
  assert!(unsupported_command("GET_USER").is_none());
  assert!(unsupported_command("FROBNICATE").is_none());
}

#[test]
fn rich_activity_roundtrips_every_field_through_cached_activity() {
  // Full-rich SET_ACTIVITY as a real game SDK sends it (details, state,
  // timestamps, assets, party, secrets, buttons, flags, emoji, display
  // type, plus one unknown future key): the bridge payload must carry
  // every one of them to consumers, byte-equivalent modulo key order.
  let mut cmd = parse_cmd(
    r#"{"cmd":"SET_ACTIVITY","nonce":"rich-1","args":{"pid":424242,"activity":{
      "name":"Rook R.E.P.O. Detail","type":0,"details":"Battle Creek",
      "state":"In Competitive Match","timestamps":{"start":1789148307000},
      "assets":{"large_image":"canary-large","large_text":"Canary","small_image":"ptb-small","small_text":"PTB"},
      "party":{"id":"party_aac0ffee","size":[2,4],"privacy":1},
      "secrets":{"join":"L33tJoin","spectate":"L33tSpec","match":"L33tMatch"},
      "buttons":[{"label":"Play","url":"https://example.com/play"}],
      "instance":true,"flags":1,"status_display_type":1,
      "emoji":{"name":" 잠수","id":"123","animated":false},
      "mystery_field_xyz":"must-survive"}}}"#,
  );
  let payload = cached_activity(&mut cmd).expect("rich activity encodes");
  let body: Value = serde_json::from_str(&payload.json).expect("valid json");
  let activity = &body["activity"];
  assert_eq!(activity["details"], "Battle Creek");
  assert_eq!(activity["state"], "In Competitive Match");
  assert_eq!(activity["timestamps"]["start"], 1789148307000i64);
  assert_eq!(activity["assets"]["large_image"], "canary-large");
  assert_eq!(activity["party"]["size"], serde_json::json!([2, 4]));
  assert_eq!(activity["party"]["privacy"], 1);
  assert_eq!(activity["secrets"]["join"], "L33tJoin");
  assert_eq!(activity["secrets"]["spectate"], "L33tSpec");
  assert_eq!(activity["secrets"]["match"], "L33tMatch");
  assert_eq!(activity["buttons"], serde_json::json!(["Play"]));
  assert_eq!(activity["instance"], true);
  assert_eq!(activity["status_display_type"], 1);
  assert_eq!(activity["mystery_field_xyz"], "must-survive");
  // Same bytes on the MessagePack leg.
  let decoded: Value = rmp_serde::from_slice(&payload.msgpack).expect("valid msgpack");
  assert_eq!(
    decoded["activity"]["party"]["size"],
    serde_json::json!([2, 4])
  );
  assert_eq!(decoded["activity"]["mystery_field_xyz"], "must-survive");
}
