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
