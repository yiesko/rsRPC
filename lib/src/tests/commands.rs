use serde_json::Value;

use crate::cmd::ActivityCmd;
use crate::commands::{
  current_user_update, generic_ack, rpc_error, subscribe_ack, unsupported_command, user_response,
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
