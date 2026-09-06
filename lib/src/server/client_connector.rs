use std::{
  collections::HashMap,
  sync::{Arc, Mutex},
};

use serde_json::Value;
use simple_websockets::{Event, EventHub, Message, Responder};

use crate::{cmd::ActivityCmd, commands, log, url_params::get_url_params};

use super::process::ProcessDetectedEvent;

/// Which wire protocol a connected bridge client speaks.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BridgeProtocol {
  /// JSON text frames (port 1337, the arRPC-compatible bridge).
  Json,
  /// MessagePack binary frames (port 1338).
  MsgPack,
}

impl BridgeProtocol {
  /// The protocol requested by the client's `format=` query parameter,
  /// falling back to the per-port default when absent.
  fn from_query(uri: &str, default: BridgeProtocol) -> BridgeProtocol {
    match get_url_params(uri.to_string())
      .get("format")
      .map(String::as_str)
    {
      Some("msgpack") | Some("messagepack") => BridgeProtocol::MsgPack,
      Some("json") => BridgeProtocol::Json,
      _ => default,
    }
  }
}

#[derive(Clone)]
pub struct ClientConnector {
  pub port: u16,
  pub msgpack_port: u16,

  json_server: Arc<Mutex<Option<EventHub>>>,
  msgpack_server: Arc<Mutex<Option<EventHub>>>,

  pub json_clients: Arc<Mutex<HashMap<u64, Responder>>>,
  pub msgpack_clients: Arc<Mutex<HashMap<u64, Responder>>>,

  data_on_connect: String,

  /// Cache of the last activity payload per socket id, replayed to web
  /// clients that connect after the activity was set (like arRPC).
  last_activities: Arc<Mutex<HashMap<String, commands::CachedActivity>>>,

  /// Shared across the per-loop clones (event/process): which process activity
  /// was last broadcast, so IPC/WS clears can resume process detection.
  pub last_pid: Arc<Mutex<Option<u64>>>,
  pub active_socket: Arc<Mutex<Option<String>>>,
  /// Last process-detected activity id broadcast but not yet cleared.
  /// Consumed (`take`) by the null-scan path to clear exactly once — even
  /// when an IPC/WS clear already reset `active_socket` meanwhile. The two
  /// keyspaces differ (IPC clears are pid-keyed, process clears are
  /// app-id-keyed), so gating the prune on `active_socket` strands the
  /// process entry forever: every later bridge client replays a dead game.
  pub last_process: Arc<Mutex<Option<String>>>,

  pub ipc_event_rec: Arc<Mutex<Option<std::sync::mpsc::Receiver<ActivityCmd>>>>,
  pub proc_event_rec: Arc<Mutex<Option<std::sync::mpsc::Receiver<ProcessDetectedEvent>>>>,
  pub ws_event_rec: Arc<Mutex<Option<std::sync::mpsc::Receiver<ActivityCmd>>>>,
}

impl ClientConnector {
  pub fn new(
    port: u16,
    msgpack_port: u16,
    data_on_connect: String,
    ipc_event_rec: std::sync::mpsc::Receiver<ActivityCmd>,
    proc_event_rec: std::sync::mpsc::Receiver<ProcessDetectedEvent>,
    ws_event_rec: std::sync::mpsc::Receiver<ActivityCmd>,
  ) -> ClientConnector {
    ClientConnector {
      json_server: Arc::new(Mutex::new(Some(launch_server(port, "JSON bridge")))),
      msgpack_server: Arc::new(Mutex::new(Some(launch_server(
        msgpack_port,
        "MessagePack bridge",
      )))),

      json_clients: Arc::new(Mutex::new(HashMap::new())),
      msgpack_clients: Arc::new(Mutex::new(HashMap::new())),
      data_on_connect,
      port,
      msgpack_port,

      last_activities: Arc::new(Mutex::new(HashMap::new())),

      last_pid: Arc::new(Mutex::new(None)),
      active_socket: Arc::new(Mutex::new(None)),
      last_process: Arc::new(Mutex::new(None)),

      ipc_event_rec: Arc::new(Mutex::new(Some(ipc_event_rec))),
      proc_event_rec: Arc::new(Mutex::new(Some(proc_event_rec))),
      ws_event_rec: Arc::new(Mutex::new(Some(ws_event_rec))),
    }
  }

  pub fn start(&mut self) {
    let json_server = self
      .json_server
      .lock()
      .unwrap()
      .take()
      .expect("Client connector already started");
    let msgpack_server = self
      .msgpack_server
      .lock()
      .unwrap()
      .take()
      .expect("Client connector already started");

    let json_clients = self.json_clients.clone();
    let msgpack_clients = self.msgpack_clients.clone();
    let data_on_connect = self.data_on_connect.clone();
    let last_activities = self.last_activities.clone();

    // One poll loop per bridge protocol/port
    std::thread::spawn({
      let last_activities = last_activities.clone();
      let data_on_connect = data_on_connect.clone();
      move || {
        Self::poll_loop(
          json_server,
          json_clients,
          last_activities,
          data_on_connect,
          BridgeProtocol::Json,
        )
      }
    });
    std::thread::spawn(move || {
      Self::poll_loop(
        msgpack_server,
        msgpack_clients,
        last_activities,
        data_on_connect,
        BridgeProtocol::MsgPack,
      )
    });

    // Create a thread for each reciever
    let ipc_event_rec = self.ipc_event_rec.lock().unwrap().take().unwrap();
    let proc_event_rec = self.proc_event_rec.lock().unwrap().take().unwrap();
    let ws_event_rec = self.ws_event_rec.lock().unwrap().take().unwrap();

    let ipc_clone = self.clone();
    let proc_clone = self.clone();
    let ws_clone = self.clone();

    std::thread::spawn(move || Self::event_loop(ipc_event_rec, ipc_clone));
    std::thread::spawn(move || Self::event_loop(ws_event_rec, ws_clone));
    std::thread::spawn(move || Self::process_loop(proc_event_rec, proc_clone));
  }

  fn poll_loop(
    server: EventHub,
    clients: Arc<Mutex<HashMap<u64, Responder>>>,
    last_activities: Arc<Mutex<HashMap<String, commands::CachedActivity>>>,
    data_on_connect: String,
    default_protocol: BridgeProtocol,
  ) {
    loop {
      match server.poll_event() {
        Event::Connect(client_id, responder) => {
          log!("[Client Connector] Client {} connected", client_id);

          let uri = responder.connection_details().uri.clone();
          let protocol = BridgeProtocol::from_query(&uri, default_protocol);

          log!(
            "[Client Connector] Client {} using protocol {:?}",
            client_id,
            protocol
          );

          // Send initial connection data
          send_message(&responder, &data_on_connect, protocol);

          // Send any cached activities so late joiners see current presence
          for payload in last_activities.lock().unwrap().values() {
            send_cached_activity(&responder, payload, protocol);
          }

          clients.lock().unwrap().insert(client_id, responder);
        }
        Event::Disconnect(client_id) => {
          log!("[Client Connector] Client {} disconnected", client_id);
          clients.lock().unwrap().remove(&client_id);
        }
        Event::Message(client_id, message) => {
          log!(
            "[Client Connector] Received message from client {}: {:?}",
            client_id,
            message
          );
          let clients = clients.lock().unwrap();
          if let Some(responder) = clients.get(&client_id) {
            responder.send(message);
          }
        }
      }
    }
  }

  /**
   * Handle activity commands coming from the IPC and WebSocket connectors.
   * `SET_ACTIVITY` commands are translated into bridge payloads, everything
   * else (INVITE_BROWSER, DEEP_LINK, ...) is forwarded as-is.
   */
  fn event_loop(rec: std::sync::mpsc::Receiver<ActivityCmd>, mut connector: ClientConnector) {
    while let Ok(cmd) = rec.recv() {
      if cmd.cmd != "SET_ACTIVITY" {
        // Just send the event as-is, there isn't really anything to go off of here
        connector.broadcast_raw(&cmd);
        continue;
      }

      let mut cmd = cmd;
      match commands::cached_activity(&mut cmd) {
        Some(payload) => {
          let pid = cmd
            .args
            .as_ref()
            .and_then(|args| args.pid)
            .unwrap_or_default();
          // A genuine clear (null activity) from a real connection means the
          // SDK source went away: drop our process-side "already sent" state
          // so the scanner re-asserts the still-running game on the next
          // pass (e.g. How to Fish comes back after Sober/Roblox closes).
          // pid == 0 means no game was ever identified on this connection
          // (e.g. SUBSCRIBE before any SET_ACTIVITY) — ignore those, or
          // every fresh connection would flap the display.
          if is_genuine_clear(&cmd) {
            log!("[Client Connector] IPC/WS source cleared, resuming process detection");
            *connector.active_socket.lock().unwrap() = None;
          }
          connector.broadcast_activity(payload, pid.to_string());
        }
        None => log!("[Client Connector] Invalid activity command, skipping"),
      }
    }
  }

  fn process_loop(
    rec: std::sync::mpsc::Receiver<ProcessDetectedEvent>,
    mut connector: ClientConnector,
  ) {
    while let Ok(proc_event) = rec.recv() {
      let proc_activity = proc_event.activity;

      if proc_activity.id == "null" {
        // Clear exactly once per process publication: consume the
        // outstanding id (if any) and clear it. Gated on the publication,
        // NOT on active_socket — an IPC/WS clear may have reset that flag
        // already while the app-id-keyed entry is still live (dual
        // keyspaces: pid-keyed vs app-id-keyed).
        let Some((pid, socket_id)) = take_process_clear(&connector) else {
          continue;
        };

        // Send an empty payload
        log!("[Client Connector] Sending empty payload");

        let payload = commands::empty_cached(pid, socket_id.clone());

        connector.broadcast_activity(payload, socket_id);

        *connector.active_socket.lock().unwrap() = None;

        continue;
      }

      // If the active socket is different from the current socket, send an empty payload for the old socket
      let active = connector.active_socket.lock().unwrap().clone();
      if active != Some(proc_activity.id.clone()) {
        if let Some(socket_id) = active {
          // Send an empty payload
          log!("[Client Connector] Sending empty payload");

          let pid = connector.last_pid.lock().unwrap().unwrap_or_default();
          let payload = commands::empty_cached(pid, socket_id.clone());

          connector.broadcast_activity(payload, socket_id);
        }
      } else {
        log!(
          "[Client Connector] Already sent payload for activity: {}",
          proc_activity.name
        );
        continue;
      }

      let payload_struct = commands::ProcessPayload {
        activity: commands::ProcessActivity {
          application_id: proc_activity.id.clone(),
          name: proc_activity.name.clone(),
          timestamps: commands::ProcessTimestamps {
            start: proc_activity
              .timestamp
              .as_ref()
              .cloned()
              .unwrap_or_else(|| "0".to_string()),
          },
          r#type: 0,
          metadata: HashMap::new(),
          flags: 0,
        },
        pid: proc_activity.pid.unwrap_or_default(),
        socket_id: proc_activity.id.clone(),
      };

      *connector.last_pid.lock().unwrap() = proc_activity.pid;
      *connector.active_socket.lock().unwrap() = Some(proc_activity.id.clone());
      *connector.last_process.lock().unwrap() = Some(proc_activity.id.clone());

      log!(
        "[Client Connector] Sending payload for activity: {}",
        proc_activity.name
      );

      let payload = commands::CachedActivity {
        json: serde_json::to_string(&payload_struct).unwrap_or_default(),
        msgpack: rmp_serde::to_vec_named(&payload_struct).unwrap_or_default(),
      };

      connector.broadcast_activity(payload, proc_activity.id.clone());
    }
  }

  /**
   * Broadcast an activity payload to all connected clients, updating the
   * replay cache so clients connecting later catch up on the current presence.
   */
  fn broadcast_activity(&mut self, payload: commands::CachedActivity, socket_id: String) {
    // Keep the replay cache in sync, pruning cleared activities
    let is_clear = serde_json::from_str::<Value>(&payload.json)
      .ok()
      .and_then(|value| value.get("activity").cloned())
      .map(|activity| activity.is_null())
      .unwrap_or(false);

    let mut last_activities = self.last_activities.lock().unwrap();
    if is_clear {
      last_activities.remove(&socket_id);
    } else {
      last_activities.insert(socket_id, payload.clone());
    }
    drop(last_activities);

    let json_clients = self.json_clients.lock().unwrap();
    let msgpack_clients = self.msgpack_clients.lock().unwrap();
    if json_clients.is_empty() && msgpack_clients.is_empty() {
      log!("[Client Connector] No clients connected, skipping");
      return;
    }
    drop(json_clients);
    drop(msgpack_clients);

    // Backpressure: Responder::send reports dead clients. Prune them so a
    // stuck bridge client cannot pin memory (its queued frames) forever.
    // One log per pruned client: removal means it never logs again.
    for (clients, payload) in [
      (&self.json_clients, Message::Text(payload.json.clone())),
      (
        &self.msgpack_clients,
        Message::Binary(payload.msgpack.clone()),
      ),
    ] {
      let mut clients = clients.lock().unwrap();
      let dead: Vec<u64> = clients
        .iter()
        .filter_map(|(id, responder)| (!responder.send(payload.clone())).then_some(*id))
        .collect();
      for id in dead {
        log!("[Client Connector] Pruning dead bridge client {id}");
        clients.remove(&id);
      }
    }
  }

  /**
   * Broadcast a non-activity event (e.g. INVITE_BROWSER) as-is to all clients.
   */
  fn broadcast_raw(&mut self, cmd: &ActivityCmd) {
    let json_clients = self.json_clients.lock().unwrap();
    let msgpack_clients = self.msgpack_clients.lock().unwrap();
    if json_clients.is_empty() && msgpack_clients.is_empty() {
      log!("[Client Connector] No clients connected, skipping");
      return;
    }

    for responder in json_clients.values() {
      if let Ok(payload) = serde_json::to_string(cmd) {
        responder.send(Message::Text(payload));
      }
    }
    for responder in msgpack_clients.values() {
      if let Ok(payload) = rmp_serde::to_vec_named(cmd) {
        responder.send(Message::Binary(payload));
      }
    }
  }
}

/**
 * Consume the outstanding process publication for clearing, if any.
 * Returns `(pid, socket_id)` for the clear frame. Single-shot by
 * construction (`take`): repeated null scans clear once and then skip,
 * and a prior IPC/WS clear (which resets `active_socket` but never
 * touches this) cannot disarm it.
 */
pub(crate) fn take_process_clear(connector: &ClientConnector) -> Option<(u64, String)> {
  let socket_id = connector.last_process.lock().unwrap().take()?;
  let pid = connector.last_pid.lock().unwrap().unwrap_or_default();
  Some((pid, socket_id))
}

impl Drop for ClientConnector {
  fn drop(&mut self) {
    if let Ok(mut server) = self.json_server.lock() {
      drop(server.take());
    }
    if let Ok(mut server) = self.msgpack_server.lock() {
      drop(server.take());
    }
  }
}

/**
 * Whether an IPC/WS command is a genuine clear from a real connection
 * (null activity + nonzero pid, e.g. game disconnect/close). SUBSCRIBE-style
 * messages that never identified a game (pid 0) are not clears.
 */
pub(crate) fn is_genuine_clear(cmd: &ActivityCmd) -> bool {
  match cmd.args.as_ref().and_then(|args| args.pid) {
    Some(pid) if pid != 0 => cmd
      .args
      .as_ref()
      .is_some_and(|args| args.activity.is_none()),
    _ => false,
  }
}

fn launch_server(port: u16, name: &str) -> EventHub {
  simple_websockets::launch(port).unwrap_or_else(|_| {
    log!(
      "[Client Connector] Failed to launch {} on port {}, port may already be in use",
      name,
      port
    );
    std::process::exit(1);
  })
}

/// Send a raw JSON string, encoding it to MessagePack when the client speaks
/// MessagePack.
fn send_message(responder: &Responder, data: &str, protocol: BridgeProtocol) {
  match protocol {
    BridgeProtocol::Json => {
      responder.send(Message::Text(data.to_string()));
    }
    BridgeProtocol::MsgPack => {
      if let Ok(value) = serde_json::from_str::<Value>(data)
        && let Ok(bytes) = rmp_serde::to_vec_named(&value)
      {
        responder.send(Message::Binary(bytes));
      }
    }
  }
}

/// Send an already dual-encoded activity payload to a client.
fn send_cached_activity(
  responder: &Responder,
  payload: &commands::CachedActivity,
  protocol: BridgeProtocol,
) {
  match protocol {
    BridgeProtocol::Json => {
      responder.send(Message::Text(payload.json.clone()));
    }
    BridgeProtocol::MsgPack => {
      responder.send(Message::Binary(payload.msgpack.clone()));
    }
  }
}
