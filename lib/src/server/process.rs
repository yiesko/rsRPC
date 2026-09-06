use aho_corasick::{AhoCorasick, PatternID};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;
use std::vec;

#[cfg(not(target_os = "linux"))]
use sysinfo::System;

use crate::ProcessCallback;
use crate::log;

use super::super::DetectableActivity;

#[derive(Default, Clone)]
pub struct ProcessScanState {
  pub obs_open: bool,
}

#[derive(Default)]
pub struct ProcessEventListeners {
  pub on_process_scan_complete: Option<Arc<Mutex<ProcessCallback>>>,
}

#[derive(Clone)]
pub struct Exec {
  pid: u64,
  path: String,
  arguments: Option<String>,
}

#[derive(Clone)]
pub struct ProcessDetectedEvent {
  pub activity: Arc<DetectableActivity>,
}

#[derive(Clone)]
pub struct ProcessServer {
  detected_list: Arc<Mutex<Vec<Arc<DetectableActivity>>>>,
  custom_detectables: Arc<Mutex<Vec<Arc<DetectableActivity>>>>,
  scanning: Arc<AtomicBool>,

  detectable_indexes: Arc<Mutex<Vec<[usize; 2]>>>,
  detectable_ac: Arc<Mutex<AhoCorasick>>,

  custom_detectable_indexes: Arc<Mutex<Vec<[usize; 2]>>>,
  custom_detectable_ac: Arc<Mutex<Option<AhoCorasick>>>,

  pub detectable_list: Arc<Mutex<Vec<Arc<DetectableActivity>>>>,
  /// Steam AppId (`third_party_skus` distributor `steam`) -> activity index.
  /// Lets us detect store games whose DB entry ships empty `executables`.
  steam_map: Arc<Mutex<HashMap<String, usize>>>,
  /// Normalized game name -> activity index, for the conservative exe-stem
  /// fallback (exact, multi-word names only, e.g. `how to fish`).
  name_map: Arc<Mutex<HashMap<String, usize>>>,
  pub event_sender: mpsc::Sender<ProcessDetectedEvent>,

  event_listeners: Arc<Mutex<ProcessEventListeners>>,

  /// Source URL for the detectable games database (auto-refresh).
  db_url: Option<String>,
  /// Refresh the detectable games database periodically when set.
  enable_db_update: bool,

  #[cfg(not(target_os = "linux"))]
  sysinfo: Arc<Mutex<System>>,
}

unsafe impl Sync for ProcessServer {}

impl ProcessServer {
  pub fn new(
    detectable: Vec<Arc<DetectableActivity>>,
    event_sender: mpsc::Sender<ProcessDetectedEvent>,
    event_listeners: ProcessEventListeners,
    db_url: Option<String>,
    enable_db_update: bool,
  ) -> Self {
    log!("[Process Scanner] Building Aho-Corasick patterns for main detectable activities...");
    let (ac, idx) = build_ac_patterns(&detectable);
    let (steam_map, name_map) = build_aux_maps(&detectable);
    log!("[Process Scanner] Done!");

    let server = ProcessServer {
      scanning: Arc::new(AtomicBool::new(false)),
      detected_list: Arc::new(Mutex::new(vec![])),
      custom_detectables: Arc::new(Mutex::new(vec![])),
      detectable_list: Arc::new(Mutex::new(detectable)),
      steam_map: Arc::new(Mutex::new(steam_map)),
      name_map: Arc::new(Mutex::new(name_map)),
      event_sender,

      // Aho-Corasick matching with detectables mapping
      detectable_indexes: Arc::new(Mutex::new(idx)),
      detectable_ac: Arc::new(Mutex::new(ac)),
      custom_detectable_indexes: Arc::new(Mutex::new(vec![])),
      custom_detectable_ac: Arc::new(Mutex::new(None)),

      // Event listeners
      event_listeners: Arc::new(Mutex::new(event_listeners)),

      // Detectable database auto-refresh
      db_url,
      enable_db_update,

      // sysinfo System
      #[cfg(not(target_os = "linux"))]
      sysinfo: Arc::new(Mutex::new(System::new())),
    };

    // One-time parse arenas are now garbage: steady state is the lean
    // structures just built.
    release_parse_arenas();

    server
  }

  fn update_custom_detectables(&self) {
    log!("[Process Scanner] Updating Aho-Corasick patterns for custom detectable activities...");
    let (ac, idx) = build_ac_patterns_allow_all_os(&self.custom_detectables.lock().unwrap());
    if !idx.is_empty() {
      *self.custom_detectable_ac.lock().unwrap() = Some(ac);
    } else {
      *self.custom_detectable_ac.lock().unwrap() = None;
    }
    *self.custom_detectable_indexes.lock().unwrap() = idx;
    log!("[Process Scanner] Done!");
  }

  /**
   * Replace the main detectable games database at runtime (used by the
   * periodic refresh), rebuilding the Aho-Corasick automaton.
   */
  fn update_main_detectables(&self, detectable: Vec<DetectableActivity>) {
    log!("[Process Scanner] Rebuilding Aho-Corasick patterns for main detectable activities...");
    let detectable: Vec<Arc<DetectableActivity>> = detectable.into_iter().map(Arc::new).collect();
    let (ac, idx) = build_ac_patterns(&detectable);
    let (steam_map, name_map) = build_aux_maps(&detectable);

    *self.detectable_list.lock().unwrap() = detectable;
    *self.detectable_ac.lock().unwrap() = ac;
    *self.detectable_indexes.lock().unwrap() = idx;
    *self.steam_map.lock().unwrap() = steam_map;
    *self.name_map.lock().unwrap() = name_map;
    log!("[Process Scanner] Done!");
    // Fetch string, JSON DOM and trimmed copy are now garbage: hand the
    // hourly spike back (refresh cadence itself is unchanged).
    release_parse_arenas();
  }

  pub fn append_detectables(&mut self, detectable: Vec<DetectableActivity>) {
    // Append to detectable chunks, since that's what is actually scanned
    self
      .custom_detectables
      .lock()
      .unwrap()
      .extend(detectable.into_iter().map(Arc::new));
    self.update_custom_detectables();
  }

  pub fn remove_detectable_by_name(&mut self, name: String) {
    self
      .custom_detectables
      .lock()
      .unwrap()
      .retain(|x| x.name != name);
    self.update_custom_detectables();
  }

  pub fn start(&self, scan_interval: Duration) {
    let wait_time = scan_interval;
    let clone = self.clone();

    self.update_custom_detectables();

    // Periodically refresh the detectable games database (like pog5-rsrpc).
    // Sleep first: startup already fetched synchronously, so an immediate
    // refetch would parse the whole DB twice for the same data (double
    // transient memory + startup time for zero new information).
    if clone.enable_db_update && clone.db_url.is_some() {
      let db_clone = clone.clone();
      std::thread::spawn(move || {
        loop {
          std::thread::sleep(Duration::from_secs(3600));
          let url = db_clone.db_url.clone().unwrap();
          match fetch_detectable(&url) {
            Ok(detectable) => db_clone.update_main_detectables(detectable),
            Err(err) => {
              log!(
                "[Process Scanner] Error updating detectable database: {}",
                err
              );
            }
          }
        }
      });
    }

    std::thread::spawn(move || {
      // Run the process scan repeatedly (every 3 seconds)
      loop {
        let detected = match clone.scan_for_processes() {
          Ok(detected) => detected,
          Err(err) => {
            log!("[Process Scanner] Error while scanning processes: {}", err);
            std::thread::sleep(wait_time);
            continue;
          }
        };
        let mut new_game_detected = false;

        // If the detected list has changed, send only the first element
        if !detected.is_empty() {
          let detected_list = clone.detected_list.lock().unwrap();

          // If the detected list is empty, send the first element
          if detected_list.is_empty() {
            new_game_detected = true;
            clone
              .event_sender
              .send(ProcessDetectedEvent {
                activity: detected[0].clone(),
              })
              .unwrap();
          } else {
            // If the detected list is not empty, check if the first element is different
            if detected[0].id != detected_list[0].id {
              new_game_detected = true;
            }

            clone
              .event_sender
              .send(ProcessDetectedEvent {
                activity: detected[0].clone(),
              })
              .unwrap();
          }
        }

        // If there are no detected processes, send an empty message
        if detected.is_empty() {
          clone
            .event_sender
            .send(ProcessDetectedEvent {
              activity: Arc::new(DetectableActivity {
                bot_public: None,
                bot_require_code_grant: None,
                cover_image: None,
                description: None,
                developers: None,
                executables: None,
                flags: None,
                guild_id: None,
                hook: false,
                icon: None,
                id: "null".to_string(),
                name: "".to_string(),
                publishers: None,
                rpc_origins: None,
                splash: None,
                third_party_skus: None,
                type_field: None,
                verify_key: None,
                primary_sku_id: None,
                slug: None,
                aliases: None,
                overlay: None,
                overlay_compatibility_hook: None,
                privacy_policy_url: None,
                terms_of_service_url: None,
                eula_id: None,
                deeplink_uri: None,
                tags: None,
                pid: None,
                timestamp: None,
              }),
            })
            .unwrap();
        }

        if new_game_detected {
          // Set the detected list to the new list
          *clone.detected_list.lock().unwrap() = detected;
        }

        std::thread::sleep(wait_time);
      }
    });
  }

  #[cfg(not(target_os = "linux"))]
  pub fn process_list(&self) -> Result<Vec<Exec>, Box<dyn std::error::Error>> {
    use std::path::Path;
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};

    let mut processes = Vec::new();
    let mut sys = self.sysinfo.lock().unwrap();
    sys.refresh_processes_specifics(
      ProcessesToUpdate::All,
      true,
      ProcessRefreshKind::nothing()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet),
    );

    for proc in sys.processes() {
      let mut cmd = proc.1.cmd().iter();
      processes.push(Exec {
        pid: proc.0.to_string().parse::<u64>()?,
        path: proc.1.exe().unwrap_or(Path::new("")).display().to_string(),
        arguments: cmd.next().map(|_| {
          cmd
            .map(|x| x.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
        }),
      });
    }

    Ok(processes)
  }

  #[cfg(target_os = "linux")]
  pub fn process_list() -> Result<Vec<Exec>, Box<dyn std::error::Error>> {
    use std::fs;

    let proc_list = fs::read_dir("/proc")?.filter(|e| {
      if let Ok(entry) = e {
        // Only if we can parse this as a number
        return entry.file_name().to_str().unwrap().parse::<u64>().is_ok();
      }

      false
    });
    let mut processes = Vec::new();

    for entry in proc_list {
      let entry = entry?;
      let path = entry.path();

      if let Ok(cmdline) = fs::read_to_string(path.join("cmdline"))
        && !cmdline.is_empty()
      {
        let mut cmd_iter = cmdline.split('\0');
        let (cmd_path, cmd_args) = (
          cmd_iter.next().unwrap_or("").to_string(),
          cmd_iter.collect::<Vec<_>>().join(" "),
        );
        processes.push(Exec {
          pid: path
            .file_name()
            .ok_or("Invalid path")?
            .to_str()
            .ok_or("Invalid path")?
            .parse::<u64>()?,
          path: cmd_path,
          arguments: if cmd_args.is_empty() {
            None
          } else {
            Some(cmd_args)
          },
        });
      }
    }

    Ok(processes)
  }

  pub fn scan_for_processes(
    &self,
  ) -> Result<Vec<Arc<DetectableActivity>>, Box<dyn std::error::Error>> {
    #[cfg(not(target_os = "linux"))]
    let processes = self.process_list()?;
    #[cfg(target_os = "linux")]
    let processes = ProcessServer::process_list()?;

    log!("[Process Scanner] Process scan triggered");

    if self.scanning.load(std::sync::atomic::Ordering::Relaxed) {
      log!("[Process Scanner] Scanning already in progress");
      return Err("Scanning already in progress".into());
    }

    let mut obs_open = false;

    let ac = self
      .detectable_ac
      .lock()
      .map_err(|e| format!("detectable_ac lock poisoned: {e}"))?;
    let custom_ac = self
      .custom_detectable_ac
      .lock()
      .map_err(|e| format!("custom_detectable_ac lock poisoned: {e}"))?;
    let detectable_list = self
      .detectable_list
      .lock()
      .map_err(|e| format!("detectable_list lock poisoned: {e}"))?;

    let mut reversed_path = String::with_capacity(256);
    // Variant scratch space, reused for every process: the scan allocates
    // nothing per process at steady state (see path_variants_into).
    let mut variant_bufs: [String; 5] = Default::default();

    let mut detected_list: Vec<Arc<DetectableActivity>> = processes
      .iter()
      .filter_map(|process| {
        // Process path (but consistent slashes, so we can compare properly)
        let mut process_path = process.path.to_ascii_lowercase();

        if process_path.contains('\\') {
          process_path = process_path.replace('\\', "/");
        }

        if !process_path.starts_with('/') {
          process_path.insert(0, '/');
        }

        if !obs_open && (process_path.contains("obs64") || process_path.contains("streamlabs")) {
          obs_open = true;
        }

        // Aho-Corasick matching against the path and its 64-bit-stripped
        // variants (so `wow64.exe` also matches a `wow.exe` pattern, like
        // arrpc/pog5-rsrpc). First hit in variant order wins, exactly as
        // before — only the allocations are gone.
        let mut found: Option<(Arc<DetectableActivity>, usize)> = None;
        let variant_count = path_variants_into(&process_path, &mut variant_bufs);
        'variants: for variant in &variant_bufs[..variant_count] {
          reversed_path.clear();
          reversed_path.extend(variant.chars().rev());

          if let Some(mat) = ac.find(&reversed_path) {
            let pattern_id: PatternID = mat.pattern();
            let exe_index = self.detectable_indexes.lock().unwrap()[pattern_id.as_usize()];
            found = Some((detectable_list[exe_index[0]].clone(), exe_index[1]));
          } else if let Some(custom_ac) = custom_ac.as_ref()
            && let Some(mat) = custom_ac.find(&reversed_path)
          {
            let pattern_id: PatternID = mat.pattern();
            let exe_index = self.custom_detectable_indexes.lock().unwrap()[pattern_id.as_usize()];
            found = Some((
              self.custom_detectables.lock().unwrap()[exe_index[0]].clone(),
              exe_index[1],
            ));
          }

          if found.is_some() {
            break 'variants;
          }
        }

        // No executable-name hit: try Steam AppId, then the conservative
        // exe-stem == game-name fallback. Both cover DB entries that ship
        // empty `executables` (e.g. How to Fish) with no overrides.json.
        // The AppId lives in environ (kilobytes per process), so it is
        // read here — only for the few misses — never for the whole table.
        let (obj, exe_index) = match found {
          Some(found) => found,
          None => {
            let app_id = read_steam_app_id(process.pid);
            return match_aux_process(
              &process_path,
              app_id.as_deref(),
              process.pid,
              &self.steam_map,
              &self.name_map,
              &detectable_list,
            );
          }
        };

        // Argument checks: when the database declares `arguments` for an
        // executable, the process command line must contain them (parity with
        // arrpc/pog5-rsrpc, e.g. TF2 `-game tf`, Garry's Mod `-game garrysmod`).
        let executable = &obj.executables.as_ref().unwrap()[exe_index];

        if let Some(exec_args) = &executable.arguments {
          let has_args = process
            .arguments
            .as_ref()
            .is_some_and(|args| args.contains(exec_args));
          if !has_args {
            return None;
          }
        }

        Some(stamp_activity(&obj, process.pid))
      })
      .collect();

    let callback = self
      .event_listeners
      .lock()
      .map_err(|e| format!("event_listeners lock poisoned: {e}"))?
      .on_process_scan_complete
      .clone();

    if let Some(callback) = callback.as_ref() {
      callback
        .lock()
        .map_err(|e| format!("process callback lock poisoned: {e}"))?(ProcessScanState {
        obs_open,
      });
    }

    detected_list.shrink_to_fit();

    log!("[Process Scanner] Process scan complete");

    Ok(detected_list)
  }
}

fn os_matches(os: &str) -> bool {
  match std::env::consts::OS {
    "windows" => os == "win32",
    "macos" => os == "darwin",
    "linux" => os == "linux",
    _ => true,
  }
}

/// Return one-time parse arenas (fetch body, JSON DOM, trimmed copy,
/// automaton build scratch) to the OS. Steady state is the lean
/// structures only; without this, the allocator holds the startup spike
/// as RSS indefinitely. Called after the initial build and every hourly
/// rebuild (refresh cadence itself is unchanged).
fn release_parse_arenas() {
  // SAFETY: malloc_trim only advises the allocator to release free pages;
  // it cannot invalidate live allocations, so it is always safe to call.
  unsafe {
    libc::malloc_trim(0);
  }
}

/// Read `SteamAppId` from `/proc/<pid>/environ` (set by Steam/Proton for
/// every game process it launches, including non-Steam shortcuts).
/// Callers read this lazily: environ is the most expensive read of the
/// scan, so only misses pay for it.
#[cfg(target_os = "linux")]
fn read_steam_app_id(pid: u64) -> Option<String> {
  let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
  for entry in env.split(|b| *b == 0) {
    if let Some(id) = entry.strip_prefix(b"SteamAppId=")
      && let Ok(id) = std::str::from_utf8(id)
    {
      let id = id.trim();
      if !id.is_empty() {
        return Some(id.to_string());
      }
    }
  }
  None
}

fn normalize_name(name: &str) -> String {
  name.to_lowercase().trim().to_string()
}

/// Conservative gate for the exe-stem fallback: exact, multi-word names with
/// a minimum length. Keeps generic stems (`fish`, `steam`, `game`, `reaper`)
/// from ever matching same-named DB entries.
pub(crate) fn name_matchable(normalized: &str) -> bool {
  normalized.contains(' ') && normalized.chars().count() >= 6
}

/// Executable stem of an already-normalized (`/`-separated, lowercase) path,
/// without extension: `/games/how to fish.exe` -> `how to fish`.
pub(crate) fn exe_stem(normalized_path: &str) -> String {
  let base = normalized_path
    .rsplit('/')
    .next()
    .unwrap_or(normalized_path);
  match base.rfind('.') {
    Some(dot) if dot > 0 => base[..dot].to_string(),
    _ => base.to_string(),
  }
}

fn stamp_activity(obj: &Arc<DetectableActivity>, pid: u64) -> Arc<DetectableActivity> {
  let mut new_activity = (**obj).clone();
  new_activity.pid = Some(pid);
  new_activity.timestamp = Some(format!(
    "{:?}",
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_millis()
  ));
  Arc::new(new_activity)
}

/// Auxiliary lookup maps over the main DB:
/// Steam store id -> activity index, normalized game name -> activity index.
pub(crate) fn build_aux_maps(
  detectables: &[Arc<DetectableActivity>],
) -> (HashMap<String, usize>, HashMap<String, usize>) {
  let mut steam_map = HashMap::new();
  let mut name_map = HashMap::new();

  for (index, activity) in detectables.iter().enumerate() {
    if let Some(skus) = activity.third_party_skus.as_ref() {
      for sku in skus {
        if sku.distributor == "steam"
          && let Some(id) = sku.id.as_ref()
          && !id.is_empty()
        {
          steam_map.entry(id.clone()).or_insert(index);
        }
      }
    }

    // Only matchable names participate in the exe-stem fallback, so generic
    // stems (`fish`, `steam`, `game`) can never collide with `Fish`/`Steam`.
    let normalized = normalize_name(&activity.name);
    if name_matchable(&normalized) {
      name_map.entry(normalized).or_insert(index);
    }
  }

  log!(
    "[Process Scanner] Aux maps: {} steam ids, {} matchable names",
    steam_map.len(),
    name_map.len()
  );

  (steam_map, name_map)
}

/// Last-resort matching for processes no executable pattern hit.
/// 1. `SteamAppId` (store games with empty `executables`, legit Steam).
/// 2. Exact exe-stem == multi-word game name (shortcuts/renamed exes).
///
/// Custom overrides (`append_detectables`) always win: they run first via AC.
#[allow(clippy::too_many_arguments)]
pub(crate) fn match_aux_process(
  process_path: &str,
  steam_app_id: Option<&str>,
  pid: u64,
  steam_map: &Mutex<HashMap<String, usize>>,
  name_map: &Mutex<HashMap<String, usize>>,
  detectable_list: &[Arc<DetectableActivity>],
) -> Option<Arc<DetectableActivity>> {
  if let Some(appid) = steam_app_id
    && let Some(&idx) = steam_map.lock().unwrap().get(appid)
    && let Some(obj) = detectable_list.get(idx)
  {
    log!(
      "[Process Scanner] Steam match: {} (appid {})",
      obj.name,
      appid
    );
    return Some(stamp_activity(obj, pid));
  }

  let stem = exe_stem(process_path);
  if name_matchable(&stem)
    && let Some(&idx) = name_map.lock().unwrap().get(&stem)
    && let Some(obj) = detectable_list.get(idx)
  {
    log!(
      "[Process Scanner] Name match: {} (exe stem `{}`)",
      obj.name,
      stem
    );
    return Some(stamp_activity(obj, pid));
  }

  // Install-folder fallback (Hydra / non-Steam shortcuts / renamed exes):
  // the folder often carries the title when the exe doesn't, e.g.
  // `.../Meccha Chameleon/MECCHA CHAMELEON/Chameleon/Binaries/Win64/
  // PenguinHotel-Win64-Shipping.exe`. Nearest ancestor wins; the map only
  // holds multi-word names, so generic folders (`binaries`, `win64`) and
  // single-word ones (`chameleon`) can never hit. Dotted components are
  // versions/hidden dirs, never titles.
  for component in process_path.rsplit('/').skip(1) {
    if component.contains('.') {
      continue;
    }
    let folder = normalize_name(component);
    if name_matchable(&folder)
      && let Some(&idx) = name_map.lock().unwrap().get(&folder)
      && let Some(obj) = detectable_list.get(idx)
    {
      log!(
        "[Process Scanner] Folder match: {} (folder `{}`)",
        obj.name,
        folder
      );
      return Some(stamp_activity(obj, pid));
    }
  }

  None
}

/**
 * Fetch the detectable games database from a URL, trimming it with
 * `detection::trim_detectable` before parsing.
 */
fn fetch_detectable(url: &str) -> Result<Vec<DetectableActivity>, Box<dyn std::error::Error>> {
  let body = ureq::get(url)
    .call()?
    .into_body()
    .with_config()
    .limit(64 * 1024 * 1024)
    .read_to_string()?;

  // Direct parse first: serde skips unknown fields, so the full body
  // parses with zero DOM overhead (~5x less transient memory than the
  // trimmed-Value pass). The trimming pass stays as fallback for entries
  // missing required fields (it defaults them); raw last.
  if let Ok(parsed) = serde_json::from_str::<Vec<DetectableActivity>>(&body) {
    return Ok(parsed);
  }
  if let Ok(trimmed) = super::super::detection::trim_detectable_value(&body)
    && let Ok(parsed) = serde_json::from_value::<Vec<DetectableActivity>>(trimmed)
  {
    return Ok(parsed);
  }
  Ok(serde_json::from_str(&body)?)
}

/**
 * Generate matching variants of a process path, removing 64-bit markers
 * (parity with arrpc/pog5-rsrpc). E.g. `/games/wow64.exe` produces
 * `/games/wow.exe` which matches a `wow.exe` database entry.
 *
 * Writes into caller-owned buffers and returns how many are filled, so the
 * per-process scan allocates nothing at steady state (buffers are reused
 * across processes and scans; only marker hits allocate one temp string).
 */
pub(crate) fn path_variants_into(path: &str, out: &mut [String; 5]) -> usize {
  out[0].clear();
  out[0].push_str(path);
  let mut count = 1;
  for marker in ["64", ".x64", "x64", "_64"] {
    if !path.contains(marker) {
      continue;
    }
    let variant = path.replace(marker, "");
    if variant != path && !out[..count].contains(&variant) && count < out.len() {
      out[count].clear();
      out[count].push_str(&variant);
      count += 1;
    }
  }
  count
}

fn build_ac_patterns(detectables: &[Arc<DetectableActivity>]) -> (AhoCorasick, Vec<[usize; 2]>) {
  build_ac_patterns_with_os_filter(detectables, true)
}

fn build_ac_patterns_allow_all_os(
  detectables: &[Arc<DetectableActivity>],
) -> (AhoCorasick, Vec<[usize; 2]>) {
  build_ac_patterns_with_os_filter(detectables, false)
}

fn build_ac_patterns_with_os_filter(
  detectables: &[Arc<DetectableActivity>],
  enforce_os: bool,
) -> (AhoCorasick, Vec<[usize; 2]>) {
  let mut exe_patterns: Vec<String> = Vec::new();
  let mut exe_indexes: Vec<[usize; 2]> = Vec::new();

  for (activity_index, activity) in detectables.iter().enumerate() {
    if let Some(executables) = &activity.executables {
      for (exe_index, executable) in executables.iter().enumerate() {
        if executable.is_launcher {
          continue;
        }

        // Only build patterns for executables that could run on this platform
        // For custom overrides (enforce_os=false) we skip the OS filter entirely
        // so that win32 executables can be detected on Linux via Proton/Wine
        // — this is the fix for NFS HP Remastered etc that only ships win32 entries.
        if enforce_os && !executable.os.is_empty() && !os_matches(&executable.os) {
          continue;
        }

        // Make paths consistent, and fix some additional checks
        let mut exec_name = executable.name.replace('\\', "/").to_lowercase();

        // Checks adapted from arrpc, remain the '>' in DetectableActivity for later argument checks
        if exec_name.starts_with(">") {
          exec_name.replace_range(0..1, "/");
        } else if !exec_name.starts_with("/") {
          exec_name.insert(0, '/');
        }

        exe_patterns.push(exec_name.chars().rev().collect::<String>());
        exe_indexes.push([activity_index, exe_index]);
      }
    }
  }

  (AhoCorasick::new(exe_patterns).unwrap(), exe_indexes)
}
