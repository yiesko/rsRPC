use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use serde_with::skip_serializing_none;

use crate::cmd::{ActivityCmd, ActivityPayload};

#[skip_serializing_none]
#[derive(Serialize)]
pub struct ProcessActivity {
  pub application_id: String,
  pub name: String,
  pub timestamps: ProcessTimestamps,
  pub r#type: u32,
  pub metadata: HashMap<String, String>,
  pub flags: u32,
}

#[derive(Serialize)]
pub struct ProcessTimestamps {
  pub start: String,
}

#[derive(Serialize)]
pub struct ProcessPayload {
  pub activity: ProcessActivity,
  pub pid: u64,
  #[serde(rename = "socketId")]
  pub socket_id: String,
}

pub fn empty_activity(pid: u64, socket_id: String) -> String {
  format!(
    r#"
    {{
      "activity": null,
      "pid": {pid},
      "socketId": "{socket_id}"
    }}
  "#
  )
}

/**
 * An activity payload serialized for both bridge protocols (JSON text frames
 * for the 1337 port, MessagePack binary frames for the 1338 port).
 */
#[derive(Clone, Debug)]
pub struct CachedActivity {
  pub json: String,
  pub msgpack: Vec<u8>,
}

/**
 * Build the empty (clear) payload in both protocols.
 */
pub fn empty_cached(pid: u64, socket_id: String) -> CachedActivity {
  let payload = ActivityPayload {
    activity: None,
    pid: Some(pid),
    socket_id: Some(socket_id.clone()),
  };

  CachedActivity {
    json: empty_activity(pid, socket_id),
    msgpack: rmp_serde::to_vec_named(&payload).unwrap_or_default(),
  }
}

/**
 * Turn a `SET_ACTIVITY` command into the bridge payload in both protocols.
 *
 * Returns `None` when the command cannot be converted into a valid payload
 * (e.g. it is missing its arguments entirely).
 */
pub fn cached_activity(cmd: &mut ActivityCmd) -> Option<CachedActivity> {
  cmd.fix();

  let args = cmd.args.as_mut()?;

  if args.activity.is_none() {
    let pid = args.pid.unwrap_or_default();
    return Some(empty_cached(pid, pid.to_string()));
  }

  let activity = args.activity.as_mut()?;
  activity.application_id = cmd.application_id.clone();

  let payload = ActivityPayload {
    activity: Some(activity.clone()),
    pid: args.pid,
    socket_id: Some(args.pid.unwrap_or(0).to_string()),
  };

  Some(CachedActivity {
    json: serde_json::to_string(&payload).ok()?,
    msgpack: rmp_serde::to_vec_named(&payload).ok()?,
  })
}

/**
 * Build the arRPC-shaped acknowledgement for a `SUBSCRIBE`/`UNSUBSCRIBE`
 * command: echoes `cmd`/`nonce`, reports the subscribed event name in
 * `data.evt` (arRPC blind-ACKs subscriptions the same way).
 */
pub fn subscribe_ack(cmd: &ActivityCmd) -> String {
  serde_json::to_string(&serde_json::json!({
    "cmd": cmd.cmd,
    "data": { "evt": cmd.evt },
    "evt": null,
    "nonce": cmd.nonce,
  }))
  .unwrap_or_else(|_| format!(r#"{{"cmd":"{}","evt":"ERROR"}}"#, cmd.cmd))
}

/**
 * Build the reply for a `GET_USER` command: the current identity, or
 * `null` when the requested id names somebody else (the official
 * response is "an RPC user object or null").
 */
pub fn get_user_response(cmd: &ActivityCmd, user: Option<&crate::user::RpcUser>) -> String {
  let data = user
    .and_then(|user| serde_json::to_value(user).ok())
    .unwrap_or(Value::Null);
  serde_json::to_string(&serde_json::json!({
    "cmd": cmd.cmd,
    "data": data,
    "evt": null,
    "nonce": cmd.nonce,
  }))
  .unwrap_or_else(|_| format!(r#"{{"cmd":"{}","evt":"ERROR"}}"#, cmd.cmd))
}

/**
 * Build the `CURRENT_USER_UPDATE` dispatch emitted when the local
 * identity changes (`SET_USER`/`RESET_USER`). The inner payload is the
 * user object itself, per the official event shape.
 */
pub fn current_user_update(user: &crate::user::RpcUser) -> String {
  let data = serde_json::to_value(user).unwrap_or(Value::Null);
  serde_json::json!({
    "cmd": "DISPATCH",
    "evt": "CURRENT_USER_UPDATE",
    "data": data,
    "nonce": null,
  })
  .to_string()
}

/**
 * Official error for a known command that has no backend here (OAuth,
 * voice, guilds, overlay, store...). Returns `(code, message)` so the IPC
 * and WebSocket dispatches share one table instead of drifting apart;
 * `None` means "not a known-unbacked command" (handled elsewhere, or
 * genuinely unknown).
 */
pub fn unsupported_command(cmd: &str) -> Option<(u16, &'static str)> {
  const NEEDS_CLIENT: &str = "requires the real Discord client";
  match cmd {
    // OAuth needs the in-client modal plus token exchange with the app's
    // secret: 5000 is the official OAuth2 error bucket.
    "AUTHORIZE" | "AUTHENTICATE" => Some((5000, "Authorization requires the real Discord client")),
    // Activity invites target live sessions on the real client.
    "SEND_ACTIVITY_JOIN_INVITE"
    | "CLOSE_ACTIVITY_REQUEST"
    | "ACCEPT_ACTIVITY_INVITE"
    | "ACTIVITY_INVITE_USER" => Some((
      5006,
      "No eligible activity: invites require the real Discord client",
    )),
    // Voice, guilds, channels, overlay, store, capture, certs: real-client
    // state only. Grouped under the generic code with an honest message.
    "GET_GUILD"
    | "GET_GUILDS"
    | "GET_CHANNEL"
    | "GET_CHANNELS"
    | "CREATE_CHANNEL_INVITE"
    | "GET_RELATIONSHIPS"
    | "SET_USER_VOICE_SETTINGS"
    | "SET_USER_VOICE_SETTINGS_2"
    | "PUSH_TO_TALK"
    | "SELECT_VOICE_CHANNEL"
    | "GET_SELECTED_VOICE_CHANNEL"
    | "SELECT_TEXT_CHANNEL"
    | "GET_VOICE_SETTINGS"
    | "SET_VOICE_SETTINGS"
    | "SET_VOICE_SETTINGS_2"
    | "SET_CERTIFIED_DEVICES"
    | "CAPTURE_SHORTCUT"
    | "GET_IMAGE"
    | "OVERLAY"
    | "SET_OVERLAY_LOCKED"
    | "OPEN_OVERLAY_ACTIVITY_INVITE"
    | "OPEN_OVERLAY_GUILD_INVITE"
    | "OPEN_OVERLAY_VOICE_SETTINGS"
    | "GET_SKUS"
    | "GET_ENTITLEMENTS"
    | "START_PURCHASE"
    | "VALIDATE_APPLICATION" => Some((1000, NEEDS_CLIENT)),
    _ => None,
  }
}

/**
 * Build an `evt: "ERROR"` reply for a command the server refuses
 * (unknown command, invalid invite code, unsupported callback, ...),
 * mirroring arRPC's `{cmd, data: {code, message}, evt: "ERROR", nonce}`.
 */
pub fn rpc_error(cmd: &str, nonce: &Value, code: u16, message: &str) -> String {
  serde_json::to_string(&serde_json::json!({
    "cmd": cmd,
    "data": { "code": code, "message": message },
    "evt": "ERROR",
    "nonce": nonce,
  }))
  .unwrap_or_else(|_| format!(r#"{{"cmd":"{cmd}","evt":"ERROR"}}"#))
}

/**
 * Build a neutral acknowledgement for known secondary commands
 * (`INVITE_BROWSER`, `DEEP_LINK`, ...) that are forwarded to bridge
 * clients: the outcome lives downstream, so the reply only confirms
 * receipt (arRPC answers these from the bridge round-trip; without
 * bridge clients there is nothing more to report).
 */
pub fn generic_ack(cmd: &ActivityCmd) -> String {
  serde_json::to_string(&serde_json::json!({
    "cmd": cmd.cmd,
    "data": null,
    "evt": null,
    "nonce": cmd.nonce,
  }))
  .unwrap_or_else(|_| format!(r#"{{"cmd":"{}","evt":null}}"#, cmd.cmd))
}
/**
 * Build the arRPC-shaped confirmation reply for a `SET_ACTIVITY` command.
 *
 * The reply echoes `cmd`/`nonce` and carries `data` with the (fixed) activity,
 * with `name` forced to an empty string and `type` forced to 0, matching what
 * arrpc/pog5-rsrpc return so RPC libraries that require a response (e.g.
 * pypresence) do not hang. Returns `None` when the command has no arguments.
 */
pub fn set_activity_response(cmd: &ActivityCmd) -> Option<String> {
  let args = cmd.args.as_ref()?;

  let data = match args.activity.as_ref() {
    Some(activity) => {
      let mut data = serde_json::to_value(activity).ok()?;
      if let Some(obj) = data.as_object_mut() {
        obj.insert("name".to_string(), Value::String(String::new()));
        obj.insert("type".to_string(), Value::Number(0.into()));
        obj.insert(
          "application_id".to_string(),
          cmd
            .application_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
        );
      }
      data
    }
    None => Value::Null,
  };

  serde_json::to_string(&serde_json::json!({
    "cmd": cmd.cmd,
    "data": data,
    "evt": null,
    "nonce": cmd.nonce,
  }))
  .ok()
}
