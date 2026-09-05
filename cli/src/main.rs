#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;
use rsrpc::RPCConfig;
use rsrpc::detection::{DetectableActivity, trim_detectable};
use std::path::PathBuf;

const DEFAULT_DB_URL: &str = "https://discord.com/api/v9/applications/detectable";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
  #[arg(short, long, env = "RSRPC_DETECTABLE_FILE")]
  detectable_file: Option<PathBuf>,
  #[arg(
    short,
    long,
    alias = "no-process-scanning",
    env = "RSRPC_NO_PROCESS_SCAN"
  )]
  no_process_scan: bool,
  #[arg(long, short = 'D', env = "RSRPC_DEBUG")]
  debug: bool,
  #[arg(long, env = "RSRPC_BRIDGE_PORT", default_value_t = 1337)]
  bridge_port: u16,
  #[arg(long, env = "RSRPC_MSGPACK_PORT", default_value_t = 1338)]
  msgpack_port: u16,
  #[arg(long, env = "RSRPC_WS_PORT_START", default_value_t = 6463)]
  ws_port_start: u16,
  #[arg(long, env = "RSRPC_WS_PORT_END", default_value_t = 6472)]
  ws_port_end: u16,
  #[arg(long, env = "RSRPC_SCAN_INTERVAL", default_value_t = 5)]
  scan_interval_secs: u64,
  #[arg(long, env = "RSRPC_DB_URL")]
  db_url: Option<String>,
  #[arg(long, env = "RSRPC_ENABLE_DB_UPDATE")]
  enable_db_update: bool,
  #[arg(long, env = "RSRPC_OVERRIDES_FILE")]
  overrides_file: Option<PathBuf>,
  /// Run a single process scan, print detected games and exit (main DB only)
  #[arg(long, env = "RSRPC_LIST_DETECTED")]
  list_detected: bool,
}

fn fetch_detectable(url: &str) -> Result<String, Box<dyn std::error::Error>> {
  let detectable = ureq::get(url)
    .call()?
    .into_body()
    .with_config()
    .limit(64 * 1024 * 1024)
    .read_to_string()?;

  Ok(detectable)
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
  // When running as a binary, enable logs.
  // SAFETY: called on the main thread at startup, before any other thread
  // exists, so no concurrent environment access can occur.
  unsafe {
    std::env::set_var("RSRPC_LOGS_ENABLED", "1");
  }

  let args = Args::parse();
  // Effective db_url for auto-refresh: with --enable-db-update and no --db-url, fall back to DEFAULT_DB_URL
  let effective_db_url = args.db_url.clone().or_else(|| {
    if args.enable_db_update {
      Some(DEFAULT_DB_URL.to_string())
    } else {
      None
    }
  });
  let config = RPCConfig {
    enable_process_scanner: !args.no_process_scan,
    port: args.bridge_port,
    msgpack_port: args.msgpack_port,
    ws_port_start: args.ws_port_start,
    ws_port_end: args.ws_port_end,
    scan_interval_secs: args.scan_interval_secs,
    db_url: effective_db_url.clone(),
    enable_db_update: args.enable_db_update,
    ..Default::default()
  };

  if args.debug {
    println!("[Debug] Resolved configuration: {:#?}", config);
  }

  let mut client = if args.no_process_scan {
    rsrpc::RPCServer::from_json_str("[]", config)?
  } else if let Some(file) = args.detectable_file {
    rsrpc::RPCServer::from_file(file, config)?
  } else if let Some(url) = args.db_url {
    // A custom database URL was provided; fetch it with offline fallback
    match fetch_detectable(&url) {
      Ok(detectable) => {
        let trimmed = trim_detectable(&detectable).unwrap_or(detectable);
        rsrpc::RPCServer::from_json_str(trimmed, config)?
      }
      Err(err) => {
        eprintln!(
          "[rsrpc] Failed to fetch DB from '{}': {} - using offline bundled snapshot",
          url, err
        );
        rsrpc::RPCServer::from_bundled(config)?
      }
    }
  } else if args.enable_db_update {
    // Fetch the official DB with trim + offline fallback; keep db_url in config for hourly refresh
    match fetch_detectable(DEFAULT_DB_URL) {
      Ok(detectable) => {
        let trimmed = trim_detectable(&detectable).unwrap_or(detectable);
        rsrpc::RPCServer::from_json_str(trimmed, config)?
      }
      Err(err) => {
        eprintln!(
          "[rsrpc] Failed to fetch official DB '{}': {} - using offline bundled snapshot (background refresh continues)",
          DEFAULT_DB_URL, err
        );
        rsrpc::RPCServer::from_bundled(config)?
      }
    }
  } else {
    // Fall back to the bundled snapshot (works offline)
    rsrpc::RPCServer::from_bundled(config)?
  };

  if args.list_detected {
    let found = client.detect_once()?;
    if found.is_empty() {
      println!("No games detected (main DB only; overrides.json requires a running server).");
    } else {
      for game in &found {
        match game.pid {
          Some(pid) => println!("{} (id {}) pid {}", game.name, game.id, pid),
          None => println!("{} (id {})", game.name, game.id),
        }
      }
    }
    return Ok(());
  }

  // Starts the other threads (process detector, client connector, etc)
  client.start();

  // Load local overrides (overrides.json), a feature originating from rsrpc-wrapper (Polaris)
  // Resolution order: --overrides-file > $RSRPC_OVERRIDES_FILE > $XDG_CONFIG_HOME/rsrpc/overrides.json > ~/.config/rsrpc/overrides.json
  // The file holds a Vec<DetectableActivity>, applied via append_detectables (bypasses the OS filter)
  let overrides_path = args
    .overrides_file
    .clone()
    .unwrap_or_else(overrides_file_path);
  match load_overrides(&overrides_path) {
    Ok(overrides) if !overrides.is_empty() => {
      println!(
        "[wrapper] Applying {} override(s) from '{}':",
        overrides.len(),
        overrides_path.display()
      );
      for o in &overrides {
        println!("[wrapper]   -> {} ({})", o.name, o.id);
      }
      client.append_detectables(overrides);
    }
    Ok(_) => {
      println!(
        "[wrapper] No overrides found in '{}'",
        overrides_path.display()
      );
    }
    Err(err) => {
      eprintln!(
        "[wrapper] Could not read overrides from '{}': {}",
        overrides_path.display(),
        err
      );
    }
  }

  let (tx, rx) = std::sync::mpsc::channel();
  ctrlc::set_handler(move || {
    let _ = tx.send(());
  })
  .expect("Error setting Ctrl-C handler");

  println!("Press Ctrl+C to exit");
  let _ = rx.recv();

  println!("Shutting down...");
  drop(client);

  // giving them a bit so they can clean up (e.g. drop BoundListener)
  std::thread::sleep(std::time::Duration::from_millis(100));

  Ok(())
}

/// Where to look for the overrides file (compatible with rsrpc-wrapper):
/// $RSRPC_OVERRIDES_FILE, else $XDG_CONFIG_HOME/rsrpc/overrides.json,
/// else ~/.config/rsrpc/overrides.json.
fn overrides_file_path() -> PathBuf {
  if let Ok(custom) = std::env::var("RSRPC_OVERRIDES_FILE") {
    return PathBuf::from(custom);
  }

  let base = std::env::var("XDG_CONFIG_HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|_| {
      let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
      PathBuf::from(home).join(".config")
    });

  base.join("rsrpc").join("overrides.json")
}

fn load_overrides(path: &PathBuf) -> Result<Vec<DetectableActivity>, Box<dyn std::error::Error>> {
  if !path.exists() {
    return Ok(vec![]);
  }

  let content = std::fs::read_to_string(path)?;
  let overrides: Vec<DetectableActivity> = serde_json::from_str(&content)?;
  Ok(overrides)
}
