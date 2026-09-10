use detection::DetectableActivity;
use server::{
  client_connector::ClientConnector,
  ipc::IpcConnector,
  ipc_utils::IpcFacilitator,
  process::{ProcessEventListeners, ProcessScanState, ProcessServer},
  websocket::WebsocketConnector,
};
use std::{
  path::PathBuf,
  sync::{Arc, Mutex, mpsc},
};

use user::RpcUser;

pub mod cmd;
pub mod commands;
pub mod detection;
mod logger;
mod server;
pub mod state;
mod url_params;
pub mod user;

#[cfg(test)]
mod tests;

pub type ProcessCallback = dyn FnMut(ProcessScanState) + Send + Sync;

/// Minimal game info returned by [`RPCServer::detect_once`].
#[derive(Clone, Debug)]
pub struct DetectedGame {
  pub id: String,
  pub name: String,
  pub pid: Option<u64>,
}

/// Minimal database entry for `--list-database` diagnostics.
#[derive(Clone, Debug)]
pub struct DetectableSummary {
  pub id: String,
  pub name: String,
  pub executables: usize,
}

#[derive(Clone, Debug)]
pub struct RPCConfig {
  pub enable_process_scanner: bool,
  pub enable_ipc_connector: bool,
  pub enable_websocket_connector: bool,
  pub enable_secondary_events: bool,
  pub port: u16,
  pub msgpack_port: u16,
  /// End of the JSON bridge port scan range (`port..=bridge_port_end`,
  /// arRPC-compatible `1337-1347` by default).
  pub bridge_port_end: u16,
  pub ws_port_start: u16,
  pub ws_port_end: u16,
  pub scan_interval_secs: u64,
  pub db_url: Option<String>,
  pub enable_db_update: bool,
  /// ETag captured by the startup fetch, so the first hourly refresh is
  /// conditional too (otherwise every restart pays one full redundant
  /// rebuild before learning the tag). `None` means unconditional.
  pub initial_db_etag: Option<String>,
  /// Application IDs the process scanner must never publish (coexistence
  /// with a richer publisher owning those slots). Scan-only by design:
  /// forwarded client frames always pass (a companion and a native frame
  /// are indistinguishable on that path, and the companion must flow).
  /// Ignored games behave as absent: null event, clear.
  pub ignored_ids: Vec<String>,
  /// Source URL for Discord's detection exclusions (installer/crash
  /// reporter names + regexes), refreshed hourly alongside the DB when
  /// `enable_db_update` is set. `None` disables the fetch (empty set =
  /// current behavior).
  pub exclusions_url: Option<String>,
}

impl Default for RPCConfig {
  fn default() -> Self {
    Self {
      enable_process_scanner: true,
      enable_ipc_connector: true,
      enable_websocket_connector: true,
      enable_secondary_events: true,
      port: 1337,
      bridge_port_end: 1347,
      msgpack_port: 1338,
      ws_port_start: 6463,
      ws_port_end: 6472,
      scan_interval_secs: 5,
      db_url: None,
      enable_db_update: false,
      initial_db_etag: None,
      ignored_ids: Vec::new(),
      exclusions_url: None,
    }
  }
}

#[derive(Clone)]
pub struct Connectors {
  process_server: Arc<Mutex<ProcessServer>>,
  client_connector: Arc<Mutex<ClientConnector>>,
  ipc_connector: Arc<Mutex<IpcConnector>>,
  ws_connector: Arc<Mutex<WebsocketConnector>>,
}

pub struct RPCServer {
  /// Parsed database. Ownership moves to the `ProcessServer` on [`start`](RPCServer::start)
  /// (single generation alive, never duplicated); after that this is empty
  /// and [`detect_once`](RPCServer::detect_once) on a started server finds
  /// nothing. One-shot users (CLI `--list-detected`, per-tick scanners that
  /// never start) are unaffected: they read it before any `start()`.
  detectable: Arc<Mutex<Vec<Arc<DetectableActivity>>>>,
  connectors: Option<Connectors>,
  config: RPCConfig,

  on_process_scan_complete: Option<Arc<Mutex<ProcessCallback>>>,
}

impl RPCServer {
  pub fn from_json_str(
    detectable: impl AsRef<str>,
    config: RPCConfig,
  ) -> Result<Self, Box<dyn std::error::Error>> {
    // Parse as DetectableActivity vector; invalid JSON is a caller error,
    // propagated (never panics: this is a library constructor).
    let detectable: Vec<DetectableActivity> = serde_json::from_str(detectable.as_ref())
      .map_err(|err| format!("Invalid JSON provided to RPCServer: {err}"))?;

    let detectable: Vec<Arc<DetectableActivity>> = detectable.into_iter().map(Arc::new).collect();

    Ok(Self {
      detectable: Arc::new(Mutex::new(detectable)),

      // Default to empty servers
      connectors: None,
      config,

      // Event listeners
      on_process_scan_complete: None,
    })
  }

  /**
   * Create a new RPCServer and read the detectable games list from file.
   */
  pub fn from_file(file: PathBuf, config: RPCConfig) -> Result<Self, Box<dyn std::error::Error>> {
    // Read the detectable games list from file.
    let detectable = std::fs::read_to_string(&file)
      .unwrap_or_else(|_| panic!("RPCServer could not find file: {:?}", file.display()));

    Self::from_json_str(detectable.as_str(), config)
  }

  /**
   * Create a new RPCServer using the bundled snapshot of Discord's detectable
   * games database. This works fully offline.
   */
  pub fn from_bundled(config: RPCConfig) -> Result<Self, Box<dyn std::error::Error>> {
    Self::from_json_str(detection::BUNDLED_DETECTABLE, config)
  }

  /**
   * Run a single process scan without starting any threads/connectors.
   * Used by `--list-detected` diagnostics (main DB only; custom overrides
   * require a running server via `append_detectables`). Reads the database
   * held by this server; on a started server that database already moved
   * to the scanner (see the field docs), so call this before `start()`.
   */
  pub fn detect_once(&self) -> Result<Vec<DetectedGame>, Box<dyn std::error::Error>> {
    let (tx, _rx) = mpsc::channel();
    let server = ProcessServer::new(
      self
        .detectable
        .lock()
        .map_err(|e| format!("detectable lock poisoned: {e}"))?
        .to_vec(),
      tx,
      ProcessEventListeners::default(),
      None,
      false,
      None,
      Vec::new(),
      None,
    );

    Ok(
      server
        .scan_for_processes()?
        .iter()
        .map(|a| DetectedGame {
          id: a.id.clone(),
          name: a.name.clone(),
          pid: a.pid,
        })
        .collect(),
    )
  }

  /**
   * Summarize the held database (entry count, executable count, names).
   * Like [`detect_once`](RPCServer::detect_once), call this before
   * [`start`](RPCServer::start): startup moves the database to the
   * scanner, leaving this side empty.
   */
  pub fn database_summary(&self) -> Result<Vec<DetectableSummary>, String> {
    let detectable = self
      .detectable
      .lock()
      .map_err(|err| format!("detectable lock poisoned: {err}"))?;
    Ok(
      detectable
        .iter()
        .map(|entry| DetectableSummary {
          id: entry.id.clone(),
          name: entry.name.clone(),
          executables: entry.executables.as_ref().map(Vec::len).unwrap_or(0),
        })
        .collect(),
    )
  }

  /**
   * Add new detectable processes on-the-fly. This should be run AFTER start().
   */
  pub fn append_detectables(&mut self, detectable: Vec<DetectableActivity>) {
    if self.connectors.is_none() {
      log!("[RPC Server] Cannot append detectables, connectors are not initialized");
      return;
    }

    self
      .connectors
      .as_mut()
      .unwrap()
      .process_server
      .lock()
      .unwrap()
      .append_detectables(detectable);
  }

  /**
   * Remove a detectable process by name.
   */
  pub fn remove_detectable_by_name(&mut self, name: String) {
    if self.connectors.is_none() {
      log!("[RPC Server] Cannot remove detectable, connectors are not initialized");
      return;
    }

    self
      .connectors
      .as_mut()
      .unwrap()
      .process_server
      .lock()
      .unwrap()
      .remove_detectable_by_name(name);
  }

  /**
   * Manually trigger a scan for processes. This should be run AFTER start().
   */
  pub fn scan_for_processes(&mut self) {
    if self.connectors.is_none() {
      log!("[RPC Server] Cannot scan processes, connectors are not initialized");
      return;
    }

    let process_server = self
      .connectors
      .as_mut()
      .unwrap()
      .process_server
      .lock()
      .unwrap();

    match process_server.scan_for_processes() {
      Ok(_) => {}
      Err(err) => {
        log!("[RPC Server] Error while scanning processes: {}", err);
      }
    }
  }

  pub fn on_process_scan_complete(
    &mut self,
    callback: impl FnMut(ProcessScanState) + Send + Sync + 'static,
  ) {
    if self.connectors.is_some() {
      warn!("[RPC Server] Cannot set on_process_scan_complete, connectors are already initialized");
      return;
    }

    self.on_process_scan_complete = Some(Arc::new(Mutex::new(callback)));
  }

  /// Move the parsed database out for the scanner, leaving this server
  /// empty. Single ownership by construction: cloning here would pin a
  /// second live generation beside the scanner's (the retained memory the
  /// hourly rebuilds used to accumulate).
  fn take_detectables(&mut self) -> Vec<Arc<DetectableActivity>> {
    std::mem::take(&mut *self.detectable.lock().unwrap())
  }

  pub fn start(&mut self) {
    let (proc_event_sender, proc_event_receiver) = mpsc::channel();
    let (ipc_event_sender, ipc_event_receiver) = mpsc::channel();
    let (ws_event_sender, ws_event_reciever) = mpsc::channel();

    // Shared READY identity (startup RSRPC_USER_* + runtime SET_USER).
    let user = Arc::new(Mutex::new(RpcUser::from_env()));

    // Bind the edge connectors first: their bound addresses feed the
    // bridge's state snapshot.
    let ipc_connector = IpcConnector::new(ipc_event_sender, user.clone());
    let ws_connector = WebsocketConnector::new(
      ws_event_sender,
      self.config.ws_port_start,
      self.config.ws_port_end,
      user.clone(),
    );
    let mut client_connector = ClientConnector::new(
      self.config.port,
      self.config.bridge_port_end,
      self.config.msgpack_port,
      user,
      ipc_event_receiver,
      proc_event_receiver,
      ws_event_reciever,
    );
    client_connector.set_extra_servers(ws_connector.bound_port, Some(ipc_connector.socket_path()));

    let connectors = Connectors {
      process_server: Arc::new(Mutex::new(ProcessServer::new(
        // Move, never clone: a second live generation here is exactly the
        // retained ~30MB the hourly rebuilds used to pin down.
        self.take_detectables(),
        proc_event_sender,
        ProcessEventListeners {
          on_process_scan_complete: self.on_process_scan_complete.clone(),
        },
        self.config.db_url.clone(),
        self.config.enable_db_update,
        self.config.initial_db_etag.clone(),
        self.config.ignored_ids.clone(),
        self.config.exclusions_url.clone(),
      ))),
      client_connector: Arc::new(Mutex::new(client_connector)),
      ipc_connector: Arc::new(Mutex::new(ipc_connector)),
      ws_connector: Arc::new(Mutex::new(ws_connector)),
    };

    log!(
      "[RPC Server] Starting client connector on port {}...",
      connectors.client_connector.lock().unwrap().port
    );
    log!(
      "[RPC Server] MessagePack bridge on port {}",
      connectors.client_connector.lock().unwrap().msgpack_port
    );
    connectors.client_connector.lock().unwrap().start();

    let config = self.config.clone();

    if config.enable_ipc_connector {
      log!("[RPC Server] Starting IPC connector...");
      connectors.ipc_connector.lock().unwrap().start();
    }

    if config.enable_process_scanner {
      log!("[RPC Server] Starting process server...");
      connectors
        .process_server
        .lock()
        .unwrap()
        .start(std::time::Duration::from_secs(config.scan_interval_secs));
    }

    if config.enable_websocket_connector || config.enable_secondary_events {
      log!("[RPC Server] Starting websocket connector...");
      connectors.ws_connector.lock().unwrap().start(
        config.enable_websocket_connector,
        config.enable_secondary_events,
      );
    }

    log!("[RPC Server] Done! Watching for activity...");
    self.connectors = Some(connectors);
  }
}
