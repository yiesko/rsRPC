#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;
use rsrpc::RPCConfig;
use rsrpc::detection::{DetectableActivity, trim_detectable};
use std::path::PathBuf;

const DEFAULT_DB_URL: &str = "https://discord.com/api/v9/applications/detectable";
const DEFAULT_EXCLUSIONS_URL: &str = "https://discord.com/api/v9/games/detectable/exclusions";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
  #[arg(short, long, env = "RSRPC_DETECTABLE_FILE")]
  detectable_file: Option<PathBuf>,
  #[arg(
    short,
    long,
    alias = "no-process-scanning",
    env = "RSRPC_NO_PROCESS_SCAN",
    value_parser = clap::builder::BoolishValueParser::new()
  )]
  no_process_scan: bool,
  #[arg(
    long,
    short = 'D',
    env = "RSRPC_DEBUG",
    value_parser = clap::builder::BoolishValueParser::new()
  )]
  /// Print the resolved configuration plus per-tick debug logging
  /// (scan ticks, repeat sends, match details).
  debug: bool,
  #[arg(long, env = "RSRPC_BRIDGE_PORT", default_value_t = 1337)]
  /// Start of the JSON bridge port scan range (see --bridge-port-end).
  bridge_port: u16,
  #[arg(long, env = "RSRPC_BRIDGE_PORT_END", default_value_t = 1347)]
  /// End of the JSON bridge port scan range (arRPC-compatible 1337-1347).
  bridge_port_end: u16,
  #[arg(long, env = "RSRPC_MSGPACK_PORT", default_value_t = 1338)]
  msgpack_port: u16,
  #[arg(long, env = "RSRPC_WS_PORT_START", default_value_t = 6463)]
  ws_port_start: u16,
  #[arg(long, env = "RSRPC_WS_PORT_END", default_value_t = 6472)]
  ws_port_end: u16,
  #[arg(long, env = "RSRPC_SCAN_INTERVAL", default_value_t = 5)]
  /// Base scan cadence in seconds. Idle stretches it (×2 per empty tick
  /// up to 30s) since EXEC events deliver game starts instantly and
  /// tracked exits wake the loop early — polling only backstops the rest.
  scan_interval_secs: u64,
  #[arg(long, env = "RSRPC_DB_URL")]
  db_url: Option<String>,
  #[arg(long, env = "RSRPC_ENABLE_DB_UPDATE", value_parser = clap::builder::BoolishValueParser::new())]
  enable_db_update: bool,
  /// Source URL for Discord's detection exclusions (installer/crash
  /// reporter names + regexes). Defaults to the official endpoint when
  /// --enable-db-update is set (same hourly refresh as the DB).
  #[arg(long, env = "RSRPC_EXCLUSIONS_URL")]
  exclusions_url: Option<String>,
  #[arg(long, env = "RSRPC_OVERRIDES_FILE")]
  overrides_file: Option<PathBuf>,
  /// Directory of override files (`*.json`, each an array or a single
  /// DetectableActivity): MultiMC/Prism/Hydra mappings, Proton-only
  /// titles, anything the main database misses. Merged with
  /// --overrides-file; corrupt files are skipped, never fatal.
  #[arg(long, env = "RSRPC_OVERRIDES_DIR")]
  overrides_dir: Option<PathBuf>,
  /// Application IDs never published (comma-separated): coexistence with
  /// a richer publisher owning those slots (e.g. a companion presence).
  /// Ignored games behave as absent; clears always pass through.
  /// `--list-detected` honors this too (shows what running would publish).
  #[arg(long, env = "RSRPC_IGNORE_IDS")]
  ignore_ids: Option<String>,
  /// Run a single process scan, print detected games and exit (staged
  /// overrides and ignore-list apply, like the daemon would publish)
  #[arg(long, env = "RSRPC_LIST_DETECTED")]
  list_detected: bool,
  /// Print a database summary (entry/executable counts + first entries)
  /// and exit. Runs before any scan, on the same held database.
  #[arg(long, env = "RSRPC_LIST_DATABASE")]
  list_database: bool,
}

fn fetch_detectable(url: &str) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
  let response = rsrpc::http_agent(std::time::Duration::from_secs(30))
    .get(url)
    .call()?;
  let etag = response
    .headers()
    .get("etag")
    .and_then(|value| value.to_str().ok())
    .map(str::to_string);
  let detectable = response
    .into_body()
    .with_config()
    .limit(64 * 1024 * 1024)
    .read_to_string()?;

  Ok((detectable, etag))
}

/// Build the server from a fetched body: parse directly first (serde
/// skips unknown fields — zero DOM transient, ~5x less startup memory),
/// falling back to the trimmed form for entries missing required fields.
/// Mirrors the lib refresh path so boot never pays the DOM pass.
fn server_from_fetched(
  detectable: String,
  config: RPCConfig,
) -> Result<rsrpc::RPCServer, Box<dyn std::error::Error>> {
  let use_trimmed = serde_json::from_str::<Vec<DetectableActivity>>(&detectable).is_err();
  let body = if use_trimmed {
    trim_detectable(&detectable).unwrap_or(detectable)
  } else {
    detectable
  };
  Ok(rsrpc::RPCServer::from_json_str(body, config)?)
}

/// Split a comma-separated id list (`--ignore-ids`): trims, drops blanks.
fn parse_ignore_ids(input: Option<&str>) -> Vec<String> {
  input
    .unwrap_or_default()
    .split(',')
    .map(str::trim)
    .filter(|id| !id.is_empty())
    .map(str::to_string)
    .collect()
}

#[hotpath::main]
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
  // Fail-fast supervision (ADR-1): worker threads dying silently would
  // leave a zombie daemon (systemd green, detection/bridge dead) that
  // Restart=on-failure can never catch. Any panic anywhere exits the
  // process after the default hook logs it, so systemd restarts us into
  // a clean state (stale sockets/IPC are reclaimed on boot by design).
  // Binary-only: the library (and its tests) keep default behavior.
  let default_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    default_hook(info);
    eprintln!("[rsrpc] worker panic, exiting for supervisor restart");
    std::process::exit(1);
  }));
  // When running as a binary, enable logs.
  // SAFETY: called on the main thread at startup, before any other thread
  // exists, so no concurrent environment access can occur.
  unsafe {
    std::env::set_var("RSRPC_LOGS_ENABLED", "1");
  }

  let args = Args::parse();
  if args.debug {
    // SAFETY: same as above, still single-threaded startup.
    unsafe {
      std::env::set_var("RSRPC_DEBUG", "1");
    }
  }
  // Effective db_url for auto-refresh: with --enable-db-update and no --db-url, fall back to DEFAULT_DB_URL
  let effective_db_url = args.db_url.clone().or_else(|| {
    if args.enable_db_update {
      Some(DEFAULT_DB_URL.to_string())
    } else {
      None
    }
  });
  // Same rule for exclusions: official endpoint when refreshing, unless
  // overridden. An explicit empty value disables the fetch.
  let effective_exclusions_url = args.exclusions_url.clone().or_else(|| {
    if args.enable_db_update {
      Some(DEFAULT_EXCLUSIONS_URL.to_string())
    } else {
      None
    }
  });
  let mut config = RPCConfig {
    enable_process_scanner: !args.no_process_scan,
    port: args.bridge_port,
    bridge_port_end: args.bridge_port_end,
    msgpack_port: args.msgpack_port,
    ws_port_start: args.ws_port_start,
    ws_port_end: args.ws_port_end,
    scan_interval_secs: args.scan_interval_secs,
    db_url: effective_db_url.clone(),
    enable_db_update: args.enable_db_update,
    ignored_ids: parse_ignore_ids(args.ignore_ids.as_deref()),
    exclusions_url: effective_exclusions_url.filter(|url| !url.trim().is_empty()),
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
      Ok((detectable, etag)) => {
        config.initial_db_etag = etag;
        server_from_fetched(detectable, config)?
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
      Ok((detectable, etag)) => {
        config.initial_db_etag = etag;
        server_from_fetched(detectable, config)?
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

  // Load local overrides (overrides.json + overrides.d), a feature originating from rsrpc-wrapper (Polaris).
  // Single file resolution: --overrides-file > $RSRPC_OVERRIDES_FILE > $XDG_CONFIG_HOME/rsrpc/overrides.json > ~/.config/rsrpc/overrides.json
  // Directory resolution: --overrides-dir > $RSRPC_OVERRIDES_DIR > $XDG_CONFIG_HOME/rsrpc/overrides.d > ~/.config/rsrpc/overrides.d
  // Both hold Vec<DetectableActivity> (or single objects), staged BEFORE
  // any branch below — so --list-detected sees exactly what the daemon
  // would publish, and start() applies them to the live scanner.
  let overrides_path = args
    .overrides_file
    .clone()
    .unwrap_or_else(rsrpc::overrides::default_file_path);
  let overrides_dir = args
    .overrides_dir
    .clone()
    .unwrap_or_else(rsrpc::overrides::default_dir_path);
  let mut staged = match rsrpc::overrides::load_file(&overrides_path) {
    Ok(overrides) if !overrides.is_empty() => {
      println!(
        "[wrapper] Applying {} override(s) from '{}':",
        overrides.len(),
        overrides_path.display()
      );
      overrides
    }
    Ok(_) => {
      println!(
        "[wrapper] No overrides found in '{}'",
        overrides_path.display()
      );
      Vec::new()
    }
    Err(err) => {
      eprintln!(
        "[wrapper] Could not read overrides from '{}': {}",
        overrides_path.display(),
        err
      );
      Vec::new()
    }
  };
  let dir_overrides = rsrpc::overrides::load_dir(&overrides_dir);
  if !dir_overrides.is_empty() {
    println!(
      "[wrapper] Applying {} override(s) from '{}':",
      dir_overrides.len(),
      overrides_dir.display()
    );
    staged.extend(dir_overrides);
  }
  for o in &staged {
    println!("[wrapper]   -> {} ({})", o.name, o.id);
  }
  client.append_detectables(staged);

  if args.list_detected {
    let found = client.detect_once()?;
    if found.is_empty() {
      println!("No games detected (overrides and ignore-list apply here too).");
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

  if args.list_database {
    let entries = client
      .database_summary()
      .map_err(|err| format!("database unavailable: {err}"))?;
    let executables: usize = entries.iter().map(|entry| entry.executables).sum();
    println!(
      "{} database entries, {} executables",
      entries.len(),
      executables
    );
    for entry in entries.iter().take(10) {
      println!("{} ({})", entry.name, entry.id);
    }
    if entries.len() > 10 {
      println!("... and {} more", entries.len() - 10);
    }
    return Ok(());
  }

  // Starts the other threads (process detector, client connector, etc).
  // Bind failures surface here (no exit inside the library).
  client.start()?;

  let (tx, rx) = std::sync::mpsc::channel();
  ctrlc::set_handler(move || {
    let _ = tx.send(());
  })
  .map_err(|err| format!("error setting Ctrl-C handler: {err}"))?;

  println!("Press Ctrl+C to exit");
  let _ = rx.recv();

  println!("Shutting down...");
  drop(client);

  // giving them a bit so they can clean up (e.g. drop BoundListener)
  std::thread::sleep(std::time::Duration::from_millis(100));

  Ok(())
}
