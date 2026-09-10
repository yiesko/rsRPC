use std::{
  collections::HashMap,
  sync::{Arc, Mutex},
};

use serde_json::Value;
use simple_websockets::{Event, EventHub, Message, Responder};

use crate::{
  cmd::ActivityCmd,
  commands, debug, error, log,
  state::{self, StateActivity, StateServer, StateServers, StateSnapshot},
  url_params::get_url_params,
  user::RpcUser,
  warn,
};

use super::process::ProcessDetectedEvent;

/// How many bridge ports to scan when the first choice is taken
/// (`port..=port_end`, arRPC scans `1337-1347` the same way).
pub const BRIDGE_PORT_SCAN_SPAN: u16 = 10;
/// Cap on replayed activities (arRPC keeps 50): bounds memory when many
/// distinct pids publish without clearing.
pub const MAX_CACHED_ACTIVITIES: usize = 50;
/// How often cached activities are rebroadcast so bridge clients that
/// missed a frame converge (arRPC refreshes every 30s).
pub const BRIDGE_REFRESH_INTERVAL_SECS: u64 = 30;

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

/// One process-detected game, remembered so an IPC clear can hand the
/// slot back to generic detection (the scanner only emits on *changes*,
/// so without this the slot would stay dark until the next game switch).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScannedGame {
  pub(crate) id: String,
  pub(crate) name: String,
  pub(crate) pid: u64,
  pub(crate) start: u64,
}

/// IPC-wins handoff: generic process detection yields its slot to a live
/// game-SDK presence and reclaims it when that source clears. One lock for
/// the whole state (short critical sections, no I/O under it), shared by
/// the event (IPC/WS) and process loops.
///
/// Retro-compat rule (several companions may target one app, e.g. wwrpc
/// plus a community fork): **last publisher wins**. Each app maps to the
/// pid of its current owner; a publish replaces the owner, and a clear
/// only releases the slot when it comes from that same owner — a stale
/// close from a superseded companion is ignored instead of wrongly
/// resuming the generic card.
#[derive(Clone, Debug, Default)]
pub(crate) struct HandoffState {
  /// App id → pid of its current IPC/WS owner (`SET_ACTIVITY` with activity).
  live_ipc: HashMap<String, u64>,
  /// Last games the scanner reported, by app id (a null/clear event wipes
  /// the table).
  last_scans: HashMap<String, ScannedGame>,
}

impl HandoffState {
  pub(crate) fn note_publish(&mut self, app_id: &str, pid: u64) {
    self.live_ipc.insert(app_id.to_string(), pid);
  }

  /// Returns true when the slot was actually released (owner cleared).
  pub(crate) fn note_clear(&mut self, app_id: &str, pid: u64) -> bool {
    if self.live_ipc.get(app_id).is_some_and(|owner| *owner == pid) {
      self.live_ipc.remove(app_id);
      true
    } else {
      false
    }
  }

  pub(crate) fn note_scan(&mut self, game: Option<ScannedGame>) {
    match game {
      Some(game) => {
        self.last_scans.insert(game.id.clone(), game);
      }
      // Null scan: the table is empty, forget every game.
      None => self.last_scans.clear(),
    }
  }

  /// Release every slot owned by `pid` (abrupt close without CLEAR) and
  /// return the released app ids. Without this, a dead owner suppresses
  /// its slots' generics forever.
  pub(crate) fn note_clear_pid(&mut self, pid: u64) -> Vec<String> {
    self
      .live_ipc
      .extract_if(|_, owner| *owner == pid)
      .map(|(app, _)| app)
      .collect()
  }

  /// Whether generic detection must stay out of this slot right now.
  pub(crate) fn suppresses(&self, app_id: &str) -> bool {
    self.live_ipc.contains_key(app_id)
  }

  /// The game to re-assert when `app_id`'s IPC source cleared, if the
  /// scanner still reports that same game.
  pub(crate) fn resume_for(&self, app_id: &str) -> Option<ScannedGame> {
    self.last_scans.get(app_id).cloned()
  }
}

/// Best-effort liveness probe so an IPC clear for an already-dead game
/// doesn't flash the generic card on the way out (the scanner's null
/// event clears the slot anyway).
pub(crate) fn process_alive(pid: u64) -> bool {
  if pid == 0 {
    return false;
  }
  if cfg!(target_os = "linux") {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
  } else {
    true
  }
}

/// Build the generic process-detection payload for a scanned game (the
/// shape `process_loop` broadcasts; reused by the handoff resume path).
pub(crate) fn generic_payload(game: &ScannedGame) -> commands::CachedActivity {
  let payload_struct = commands::ProcessPayload {
    activity: commands::ProcessActivity {
      application_id: game.id.clone(),
      name: game.name.clone(),
      timestamps: commands::ProcessTimestamps { start: game.start },
      r#type: 0,
      metadata: HashMap::new(),
      flags: 0,
    },
    pid: game.pid,
    socket_id: game.id.clone(),
  };
  commands::CachedActivity {
    json: serde_json::to_string(&payload_struct).unwrap_or_default(),
    msgpack: rmp_serde::to_vec_named(&payload_struct).unwrap_or_default(),
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

  /// Shared identity for READY frames (startup `RSRPC_USER_*`, runtime
  /// `SET_USER`/`RESET_USER` from bridge clients).
  user: Arc<Mutex<RpcUser>>,
  /// State-snapshot slot (`RSRPC_STATE_FILE` set), written on broadcasts.
  state_path: Option<std::path::PathBuf>,
  /// Extra servers for the snapshot, filled via [`set_extra_servers`]
  /// before [`start`](Self::start) (the connectors bind in their own
  /// constructors, so this struct learns them after the fact).
  ws_port: Option<u16>,
  ipc_path: Option<String>,

  /// Cache of the last activity payload per socket id, replayed to web
  /// clients that connect after the activity was set (like arRPC). Each
  /// entry carries a sequence number so the cache can evict the oldest
  /// first when it hits [`MAX_CACHED_ACTIVITIES`].
  last_activities: Arc<Mutex<HashMap<String, (commands::CachedActivity, u64)>>>,
  /// Monotonic sequence for replay-cache recency (LRU eviction order).
  activity_seq: Arc<Mutex<u64>>,

  /// Last process-detected activities broadcast but not yet cleared, by
  /// app id (each with its pid for the clear frame). Consumed (`drain`)
  /// by the null-scan path to clear exactly once. The two keyspaces
  /// differ (IPC clears are pid-keyed, process clears are app-id-keyed),
  /// so IPC clears never touch this map: an entry lives exactly from its
  /// generic publication to its process clear, and every later bridge
  /// client replays only live games.
  pub last_process: Arc<Mutex<HashMap<String, u64>>>,
  /// IPC-wins handoff state (see [`HandoffState`]).
  handoff: Arc<Mutex<HandoffState>>,

  pub ipc_event_rec: Arc<Mutex<Option<std::sync::mpsc::Receiver<ActivityCmd>>>>,
  pub proc_event_rec: Arc<Mutex<Option<std::sync::mpsc::Receiver<ProcessDetectedEvent>>>>,
  pub ws_event_rec: Arc<Mutex<Option<std::sync::mpsc::Receiver<ActivityCmd>>>>,
}

impl ClientConnector {
  pub fn new(
    port_start: u16,
    port_end: u16,
    msgpack_port: u16,
    user: Arc<Mutex<RpcUser>>,
    ipc_event_rec: std::sync::mpsc::Receiver<ActivityCmd>,
    proc_event_rec: std::sync::mpsc::Receiver<ProcessDetectedEvent>,
    ws_event_rec: std::sync::mpsc::Receiver<ActivityCmd>,
  ) -> ClientConnector {
    let (json_server, port) = launch_in_range(port_start, port_end, None, "JSON bridge");
    // The MessagePack port keeps its configured value unless it collides
    // with the claimed JSON port (e.g. defaults 1337/1338 are adjacent, or
    // a custom --bridge-port lands on 1338): then scan forward instead of
    // dying.
    let skip = (msgpack_port == port).then_some(port);
    let (msgpack_server, msgpack_port) = launch_in_range(
      msgpack_port,
      msgpack_port.saturating_add(BRIDGE_PORT_SCAN_SPAN),
      skip,
      "MessagePack bridge",
    );

    // Optional presence snapshot for external tooling (arRPC-compatible
    // layout, `rsrpc-` prefix so both daemons coexist).
    let state_path = std::env::var("RSRPC_STATE_FILE").ok().and_then(|_| {
      let path = state::select_slot(&std::env::temp_dir(), state::now_secs());
      match &path {
        Some(path) => log!("[Client Connector] State snapshot: {}", path.display()),
        None => warn!("[Client Connector] RSRPC_STATE_FILE set but no free state slot"),
      }
      path
    });

    ClientConnector {
      json_server: Arc::new(Mutex::new(Some(json_server))),
      msgpack_server: Arc::new(Mutex::new(Some(msgpack_server))),

      json_clients: Arc::new(Mutex::new(HashMap::new())),
      msgpack_clients: Arc::new(Mutex::new(HashMap::new())),
      user,
      state_path,
      ws_port: None,
      ipc_path: None,
      port,
      msgpack_port,

      last_activities: Arc::new(Mutex::new(HashMap::new())),
      activity_seq: Arc::new(Mutex::new(0)),

      last_process: Arc::new(Mutex::new(HashMap::new())),
      handoff: Arc::new(Mutex::new(HandoffState::default())),

      ipc_event_rec: Arc::new(Mutex::new(Some(ipc_event_rec))),
      proc_event_rec: Arc::new(Mutex::new(Some(proc_event_rec))),
      ws_event_rec: Arc::new(Mutex::new(Some(ws_event_rec))),
    }
  }

  /// Fill in the servers this struct does not bind itself (called once,
  /// before [`start`](Self::start)).
  pub fn set_extra_servers(&mut self, ws_port: Option<u16>, ipc_path: Option<String>) {
    self.ws_port = ws_port;
    self.ipc_path = ipc_path;
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
    let user = self.user.clone();
    let last_activities = self.last_activities.clone();

    // One poll loop per bridge protocol/port
    std::thread::spawn({
      let last_activities = last_activities.clone();
      let user = user.clone();
      move || {
        Self::poll_loop(
          json_server,
          json_clients,
          last_activities,
          user,
          BridgeProtocol::Json,
        )
      }
    });
    std::thread::spawn(move || {
      Self::poll_loop(
        msgpack_server,
        msgpack_clients,
        last_activities,
        user,
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
    let refresh_clone = self.clone();

    std::thread::spawn(move || Self::event_loop(ipc_event_rec, ipc_clone));
    std::thread::spawn(move || Self::event_loop(ws_event_rec, ws_clone));
    std::thread::spawn(move || Self::process_loop(proc_event_rec, proc_clone));
    // Periodic rebroadcast: bridge clients that missed a frame (or
    // connected between frames) converge on the cached presence.
    std::thread::spawn(move || {
      loop {
        std::thread::sleep(std::time::Duration::from_secs(BRIDGE_REFRESH_INTERVAL_SECS));
        refresh_clone.refresh_clients();
      }
    });

    // Snapshot the (empty) presence + bound servers immediately, so
    // external tooling sees us before the first game appears.
    self.persist_state();
  }

  fn poll_loop(
    server: EventHub,
    clients: Arc<Mutex<HashMap<u64, Responder>>>,
    last_activities: Arc<Mutex<HashMap<String, (commands::CachedActivity, u64)>>>,
    user: Arc<Mutex<RpcUser>>,
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
          send_message(&responder, &user.lock().unwrap().ready_payload(), protocol);

          // Send any cached activities so late joiners see current presence
          for (payload, _) in last_activities.lock().unwrap().values() {
            send_cached_activity(&responder, payload, protocol);
          }

          clients.lock().unwrap().insert(client_id, responder);
        }
        Event::Disconnect(client_id) => {
          log!("[Client Connector] Client {} disconnected", client_id);
          clients.lock().unwrap().remove(&client_id);
        }
        Event::Message(client_id, message) => {
          debug!(
            "[Client Connector] Received message from client {}: {:?}",
            client_id, message
          );
          // Bridge control messages (JSON text) are answered, everything
          // else echoes to the sender as before. Identity changes also
          // fan out as CURRENT_USER_UPDATE (official event) to every
          // client on THIS port loop (JSON and MessagePack loops are
          // independent; control traffic practically only arrives on the
          // JSON port). IPC/WS game clients learn it on their next
          // handshake (no reverse channel exists there by design).
          match &message {
            Message::Text(text) => match handle_bridge_control(&user, text) {
              Some((ack, changed)) => {
                let mut clients = clients.lock().unwrap();
                if let Some(responder) = clients.get(&client_id) {
                  responder.send(Message::Text(ack));
                }
                if let Some(user) = changed {
                  let dispatch = commands::current_user_update(&user);
                  let dead: Vec<u64> = clients
                    .iter()
                    .filter_map(|(id, responder)| {
                      (!responder.send(Message::Text(dispatch.clone()))).then_some(*id)
                    })
                    .collect();
                  for id in dead {
                    warn!("[Client Connector] Pruning dead bridge client {id}");
                    clients.remove(&id);
                  }
                }
              }
              None => {
                if let Some(responder) = clients.lock().unwrap().get(&client_id) {
                  responder.send(message);
                }
              }
            },
            _ => {
              if let Some(responder) = clients.lock().unwrap().get(&client_id) {
                responder.send(message);
              }
            }
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
  fn event_loop(rec: std::sync::mpsc::Receiver<ActivityCmd>, connector: ClientConnector) {
    while let Ok(cmd) = rec.recv() {
      if cmd.cmd != "SET_ACTIVITY" {
        // Just send the event as-is, there isn't really anything to go off of here
        connector.broadcast_raw(&cmd);
        continue;
      }

      let mut cmd = cmd;
      match commands::cached_activity(&mut cmd) {
        Some(payload) => {
          let args = cmd.args.as_ref();
          let pid = args.and_then(|args| args.pid).unwrap_or_default();
          let activity = args.and_then(|args| args.activity.as_ref());
          // IPC-wins handoff: a live SDK presence takes over this app slot
          // from generic detection (last publisher wins across companions).
          if let Some(app) = activity.and_then(|activity| activity.application_id.clone()) {
            connector.handoff.lock().unwrap().note_publish(&app, pid);
          }
          // NOTE: no ignore-list filtering here by design. Forwarded client
          // frames (companions, native game presences) are indistinguishable
          // on this path — filtering would kill the companion this bridge
          // exists to carry. `--ignore-ids` applies to process detection
          // only (see apply_ignore_list).
          // Identical republishes (apps re-send every few minutes) and
          // duplicate clears stay in debug: compare against the cached
          // activity for this pid, so first publishes, real changes and
          // effective clears still log once.
          let changed = {
            let cached = connector
              .last_activities
              .lock()
              .unwrap()
              .get(&pid.to_string())
              .and_then(|(cached, _)| serde_json::from_str::<Value>(&cached.json).ok())
              .and_then(|body| body.get("activity").cloned());
            let current = activity
              .and_then(|activity| serde_json::to_value(activity).ok())
              .unwrap_or(Value::Null);
            match cached {
              None => !current.is_null(),
              Some(old) => old != current,
            }
          };
          // Who published what, in one line: best available label (name,
          // else details/state) + app id + pid. Labels and pids only.
          match activity {
            Some(activity) => {
              let line = format!(
                "[Client Connector] Published: {} (app {}, pid {})",
                activity.display_name(),
                activity.application_id.as_deref().unwrap_or("?"),
                pid
              );
              if changed {
                log!("{}", line);
              } else {
                debug!("{}", line);
              }
            }
            None => {
              let line = format!("[Client Connector] Published clear (pid {})", pid);
              if changed {
                log!("{}", line);
              } else {
                debug!("{}", line);
              }
            }
          }
          // A genuine clear (null activity) from a real connection means the
          // SDK source went away: the slot below is handed back to generic
          // detection (resume), so the scanner re-asserts the still-running
          // game (e.g. How to Fish comes back after Sober/Roblox closes).
          // pid == 0 means no game was ever identified on this connection
          // (e.g. SUBSCRIBE before any SET_ACTIVITY) — ignore those, or
          // every fresh connection would flap the display.
          if is_genuine_clear(&cmd) {
            // Hand the slot(s) back to generic detection when the owning
            // SDK source cleared (stale closes from superseded companions
            // are ignored); the game must still be alive, or the card
            // would flash on the way out (the scanner's null event clears
            // anyway).
            let resume: Vec<ScannedGame> = {
              let mut handoff = connector.handoff.lock().unwrap();
              match cmd.application_id.clone() {
                Some(app) if handoff.note_clear(&app, pid) => {
                  handoff.resume_for(&app).into_iter().collect()
                }
                Some(_) => Vec::new(),
                // No app id: abrupt close (socket died without CLEAR).
                // Release every slot this pid owned, or their generics
                // stay suppressed by a dead owner forever.
                None => handoff
                  .note_clear_pid(pid)
                  .into_iter()
                  .filter_map(|app| handoff.resume_for(&app))
                  .collect(),
              }
            };
            for game in resume.into_iter().filter(|game| process_alive(game.pid)) {
              connector.resume_generic(&game);
            }
            if changed {
              log!("[Client Connector] IPC/WS source cleared, resuming process detection");
            } else {
              debug!("[Client Connector] Duplicate clear ignored (pid {})", pid);
            }
          }
          connector.broadcast_activity(payload, pid.to_string());
        }
        None => warn!("[Client Connector] Invalid activity command, skipping"),
      }
    }
  }

  fn process_loop(
    rec: std::sync::mpsc::Receiver<ProcessDetectedEvent>,
    connector: ClientConnector,
  ) {
    while let Ok(proc_event) = rec.recv() {
      let proc_activity = proc_event.activity;

      if proc_activity.id == "null" {
        connector.handoff.lock().unwrap().note_scan(None);
        // Clear every outstanding process publication (multi-game scans
        // publish per slot; a lone null means the table is empty). The
        // two keyspaces differ (IPC clears are pid-keyed, process clears
        // are app-id-keyed), so IPC clears never disarm these.
        let outstanding = take_process_clear(&connector);
        if outstanding.is_empty() {
          continue;
        }

        for (pid, socket_id) in outstanding {
          // Send an empty payload
          log!("[Client Connector] Sending empty payload");

          let payload = commands::empty_cached(pid, socket_id.clone());

          connector.broadcast_activity(payload, socket_id);
        }

        continue;
      }

      // Remember the scan for the handoff: an IPC clear hands the slot
      // back to exactly this game (the scanner won't re-emit it).
      let game = ScannedGame {
        id: proc_activity.id.clone(),
        name: proc_activity.name.clone(),
        pid: proc_activity.pid.unwrap_or_default(),
        start: proc_activity.timestamp.unwrap_or(0),
      };
      connector
        .handoff
        .lock()
        .unwrap()
        .note_scan(Some(game.clone()));

      // IPC-wins handoff: a live SDK presence owns this slot — withdraw
      // our generic card if shown and stay out until that source clears.
      // Strictly per-slot: other games' cards are untouched, so co-running
      // games each keep theirs.
      if connector.handoff.lock().unwrap().suppresses(&game.id) {
        // The armed entry carries its own pid for the clear frame.
        if let Some(pid) = connector.last_process.lock().unwrap().remove(&game.id) {
          connector.broadcast_activity(
            commands::empty_cached(pid, game.id.clone()),
            game.id.clone(),
          );
          debug!(
            "[Client Connector] Yielding {} to live IPC presence",
            game.name
          );
        } else {
          debug!(
            "[Client Connector] Deferring to live IPC presence for: {}",
            game.name
          );
        }
        continue;
      }

      // Already showing this slot: the scanner emits every pass, so
      // repeats dedup here instead of flapping the display.
      if connector
        .last_process
        .lock()
        .unwrap()
        .contains_key(&game.id)
      {
        debug!(
          "[Client Connector] Already sent payload for activity: {}",
          proc_activity.name
        );
        continue;
      }

      connector
        .last_process
        .lock()
        .unwrap()
        .insert(game.id.clone(), proc_activity.pid.unwrap_or_default());

      debug!(
        "[Client Connector] Publishing generic presence for activity: {}",
        proc_activity.name
      );

      // Same bytes as the handoff resume path (one construction site).
      connector.broadcast_activity(generic_payload(&game), game.id.clone());
    }
  }

  /**
   * Re-assert generic process presence for `game` after its IPC source
   * cleared (handoff back). The scanner only emits on *changes*, so
   * without this the slot would stay dark until the next game switch.
   */
  fn resume_generic(&self, game: &ScannedGame) {
    self
      .last_process
      .lock()
      .unwrap()
      .insert(game.id.clone(), game.pid);
    debug!(
      "[Client Connector] Resuming generic presence for {} ({})",
      game.name, game.id
    );
    self.broadcast_activity(generic_payload(game), game.id.clone());
  }

  /**
   * Broadcast an activity payload to all connected clients, updating the
   * replay cache so clients connecting later catch up on the current presence.
   */
  #[hotpath::measure]
  fn broadcast_activity(&self, payload: commands::CachedActivity, socket_id: String) {
    // Keep the replay cache in sync, pruning cleared activities
    let is_clear = serde_json::from_str::<Value>(&payload.json)
      .ok()
      .and_then(|value| value.get("activity").cloned())
      .map(|activity| activity.is_null())
      .unwrap_or(false);

    {
      let mut last_activities = self.last_activities.lock().unwrap();
      if is_clear {
        last_activities.remove(&socket_id);
      } else {
        let mut seq = self.activity_seq.lock().unwrap();
        *seq = seq.saturating_add(1);
        last_activities.insert(socket_id, (payload.clone(), *seq));
        prune_cache(&mut last_activities);
      }
    }

    self.send_to_all(&payload);
    self.persist_state();
  }

  /**
   * Rebroadcast every cached activity (refresh tick): no cache bookkeeping,
   * just convergence for clients that missed a frame.
   */
  fn refresh_clients(&self) {
    let payloads: Vec<commands::CachedActivity> = self
      .last_activities
      .lock()
      .unwrap()
      .values()
      .map(|(payload, _)| payload.clone())
      .collect();
    if payloads.is_empty() {
      // No presence to rebroadcast, but still refresh the snapshot mtime
      // so a live-but-idle daemon never looks stale to slot reuse.
      self.persist_state();
      return;
    }
    debug!(
      "[Client Connector] Refreshing {} cached activities",
      payloads.len()
    );
    for payload in &payloads {
      self.send_to_all(payload);
    }
    self.persist_state();
  }

  /**
   * Persist the state snapshot when enabled (`RSRPC_STATE_FILE`).
   * Best-effort: failures stay in debug so a full tmpfs never breaks
   * presence.
   */
  fn persist_state(&self) {
    let Some(path) = self.state_path.clone() else {
      return;
    };
    let servers = StateServers {
      bridge: Some(StateServer {
        host: "127.0.0.1".to_string(),
        port: self.port,
      }),
      msgpack: Some(StateServer {
        host: "127.0.0.1".to_string(),
        port: self.msgpack_port,
      }),
      websocket: self.ws_port.map(|port| StateServer {
        host: "127.0.0.1".to_string(),
        port,
      }),
      ipc: self.ipc_path.clone(),
    };
    let activities = state_activities(&self.last_activities.lock().unwrap());
    let snapshot = StateSnapshot::new(servers, activities);
    if let Err(err) = state::write_snapshot(&path, &snapshot) {
      debug!("[Client Connector] State snapshot failed: {}", err);
    }
  }

  /**
   * Send one payload to every connected bridge client, pruning dead ones.
   */
  fn send_to_all(&self, payload: &commands::CachedActivity) {
    let json_clients = self.json_clients.lock().unwrap();
    let msgpack_clients = self.msgpack_clients.lock().unwrap();
    if json_clients.is_empty() && msgpack_clients.is_empty() {
      debug!("[Client Connector] No clients connected, skipping");
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
        warn!("[Client Connector] Pruning dead bridge client {id}");
        clients.remove(&id);
      }
    }
  }

  /**
   * Broadcast a non-activity event (e.g. INVITE_BROWSER) as-is to all clients.
   */
  fn broadcast_raw(&self, cmd: &ActivityCmd) {
    let json_clients = self.json_clients.lock().unwrap();
    let msgpack_clients = self.msgpack_clients.lock().unwrap();
    if json_clients.is_empty() && msgpack_clients.is_empty() {
      debug!("[Client Connector] No clients connected, skipping");
      return;
    }

    // Serialize once per encoding, not once per client: clones are
    // orders of magnitude cheaper than re-serializing the same frame.
    let json_payload = if json_clients.is_empty() {
      None
    } else {
      serde_json::to_string(cmd).ok()
    };
    let msgpack_payload = if msgpack_clients.is_empty() {
      None
    } else {
      rmp_serde::to_vec_named(cmd).ok()
    };
    if let Some(payload) = json_payload {
      for responder in json_clients.values() {
        responder.send(Message::Text(payload.clone()));
      }
    }
    if let Some(payload) = msgpack_payload {
      for responder in msgpack_clients.values() {
        responder.send(Message::Binary(payload.clone()));
      }
    }
  }
}

/**
 * Consume the outstanding process publications for clearing, if any.
 * Returns `(pid, socket_id)` pairs, sorted for deterministic clears.
 * Single-shot by construction (`drain`): repeated null scans clear once
 * and then skip. IPC/WS clears never touch this map (pid-keyed vs
 * app-id-keyed keyspaces), so they cannot disarm it either.
 */
pub(crate) fn take_process_clear(connector: &ClientConnector) -> Vec<(u64, String)> {
  let mut outstanding: Vec<(u64, String)> = connector
    .last_process
    .lock()
    .unwrap()
    .drain()
    .map(|(socket_id, pid)| (pid, socket_id))
    .collect();
  outstanding.sort();
  outstanding
}

impl Drop for ClientConnector {
  fn drop(&mut self) {
    if let Ok(mut server) = self.json_server.lock() {
      drop(server.take());
    }
    if let Ok(mut server) = self.msgpack_server.lock() {
      drop(server.take());
    }
    // State slots are owned while alive (fresh mtime blocks reuse):
    // remove ours so a later daemon reuses the slot immediately.
    if let Some(path) = self.state_path.clone() {
      let _ = std::fs::remove_file(path);
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

/**
 * Handle a bridge control message (`SET_USER`/`RESET_USER`, arRPC parity).
 * Returns the ACK text plus the new identity when it changed, `None` for
 * anything else (the caller echoes those to the sender untouched).
 */
pub(crate) fn handle_bridge_control(
  user: &Arc<Mutex<RpcUser>>,
  text: &str,
) -> Option<(String, Option<RpcUser>)> {
  let body: Value = serde_json::from_str(text).ok()?;
  let msg_type = body.get("type")?.as_str()?;
  if !matches!(msg_type, "SET_USER" | "RESET_USER") {
    return None;
  }
  let nonce = body.get("nonce").cloned().unwrap_or(Value::Null);
  let before = user.lock().unwrap().clone();
  if msg_type == "SET_USER" {
    // `patch` (arRPC shape) or `data` (defensive alias) carry the patch.
    if let Some(patch) = body.get("patch").or_else(|| body.get("data")) {
      user.lock().unwrap().patch(patch);
    }
  } else {
    // Reset to the startup identity (defaults + `RSRPC_USER_*`).
    *user.lock().unwrap() = RpcUser::from_env();
  }
  let after = user.lock().unwrap().clone();
  let changed = (before != after).then_some(after.clone());
  let user_value = serde_json::to_value(after).unwrap_or(Value::Null);
  let ack_type = format!("{msg_type}_ACK");
  let ack = serde_json::json!({
    "type": ack_type,
    "nonce": nonce,
    "data": { "success": true, "user": user_value },
  })
  .to_string();
  Some((ack, changed))
}

/**
 * Flatten the replay cache into state-snapshot activities (best-effort:
 * unparseable entries contribute their socket id only).
 */
pub(crate) fn state_activities(
  cache: &HashMap<String, (commands::CachedActivity, u64)>,
) -> Vec<StateActivity> {
  cache
    .iter()
    .map(|(socket_id, (payload, _))| {
      let body: Value = serde_json::from_str(&payload.json).unwrap_or(Value::Null);
      let activity = body.get("activity");
      StateActivity {
        socket_id: socket_id.clone(),
        name: activity
          .and_then(|item| item.get("name"))
          .and_then(Value::as_str)
          .map(str::to_string),
        application_id: activity
          .and_then(|item| item.get("application_id"))
          .and_then(Value::as_str)
          .map(str::to_string),
        pid: body.get("pid").and_then(Value::as_u64),
        start_time: activity
          .and_then(|item| item.get("timestamps"))
          .and_then(|item| item.get("start"))
          .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
          }),
      }
    })
    .collect()
}

/**
 * Evict the oldest entries while the replay cache exceeds
 * [`MAX_CACHED_ACTIVITIES`]. Pure map operation (no locks taken here).
 */
pub(crate) fn prune_cache(cache: &mut HashMap<String, (commands::CachedActivity, u64)>) {
  while cache.len() > MAX_CACHED_ACTIVITIES {
    let oldest = cache
      .iter()
      .min_by_key(|(_, (_, seq))| *seq)
      .map(|(key, _)| key.clone());
    match oldest {
      Some(key) => {
        cache.remove(&key);
      }
      None => break,
    }
  }
}

fn launch_in_range(start: u16, end: u16, skip: Option<u16>, name: &str) -> (EventHub, u16) {
  let end = end.max(start);
  for port in start..=end {
    if Some(port) == skip {
      continue;
    }
    match simple_websockets::launch(port) {
      Ok(server) => {
        log!("[Client Connector] {} on port {}", name, port);
        return (server, port);
      }
      Err(err) => {
        warn!(
          "[Client Connector] Failed to launch {} on port {}, trying next: {:?}",
          name, port, err
        );
      }
    }
  }

  error!(
    "[Client Connector] Failed to launch {} on ports {}-{}, exiting",
    name, start, end
  );
  std::process::exit(1);
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
