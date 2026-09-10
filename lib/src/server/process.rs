use aho_corasick::{AhoCorasick, PatternID};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;
use std::vec;

#[cfg(not(target_os = "linux"))]
use sysinfo::System;

use crate::ProcessCallback;
use crate::{debug, log, warn};

use super::super::DetectableActivity;
use super::super::detection::{Exclusions, parse_exclusions};
use super::steam::SteamLibraries;

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
  pub(crate) pid: u64,
  pub(crate) path: String,
  pub(crate) arguments: Option<String>,
}

#[derive(Clone)]
pub struct ProcessDetectedEvent {
  pub activity: Arc<DetectableActivity>,
}

#[derive(Clone)]
pub struct ProcessServer {
  custom_detectables: Arc<Mutex<Vec<Arc<DetectableActivity>>>>,
  scanning: Arc<AtomicBool>,

  detectable_indexes: Arc<Mutex<Vec<[usize; 2]>>>,
  detectable_ac: Arc<Mutex<AhoCorasick>>,

  custom_detectable_indexes: Arc<Mutex<Vec<[usize; 2]>>>,
  custom_detectable_ac: Arc<Mutex<Option<AhoCorasick>>>,

  /// Proton fallback automaton (Linux only): `win32` executables from the
  /// main DB, probed after the native patterns and the authoritative Steam
  /// AppId, but before the stem/folder heuristics. Empty on other platforms.
  proton_detectable_indexes: Arc<Mutex<Vec<[usize; 2]>>>,
  proton_detectable_ac: Arc<Mutex<Option<AhoCorasick>>>,

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
  /// Pids of the currently detected games (refreshed every scan tick,
  /// plus event-driven EXEC hits). Lets the proc-events watcher wake the
  /// scan loop the moment a TRACKED game exits — untracked exits never
  /// cause a scan.
  detected_pids: Arc<Mutex<HashSet<u64>>>,
  /// Memoized SteamAppId per pid: environ never changes after exec, so
  /// one read per process lifetime suffices (environ is kilobytes — the
  /// biggest per-process cost in the profiler). Invalidated by EXEC (the
  /// watcher drops the entry before reclassifying) and by death (swept
  /// every tick against the live pid set). Bounded by live process count.
  appid_cache: Arc<Mutex<HashMap<u64, Option<String>>>>,
  /// Scan thread handle for early wakeups (proc-events EXIT of a tracked
  /// game). Registered by the scan thread itself on startup.
  scan_wake: Arc<Mutex<Option<std::thread::Thread>>>,
  /// Discord detection exclusions (installer/crash-reporter basenames +
  /// regexes): excluded processes are dropped before any matching.
  /// Empty until [`ProcessServer::set_exclusions`] (startup fetch) or the
  /// hourly refresh fills it; empty behaves exactly like no exclusions.
  exclusions: Arc<Mutex<Exclusions>>,
  /// Source URL for the exclusions list (same hourly refresh as the DB).
  exclusions_url: Option<String>,
  /// Steam install-dir -> AppId (VDF provider): refreshed once per scan
  /// tick when a `libraryfolders.vdf` changed, consulted on path misses.
  steam_libraries: Arc<Mutex<SteamLibraries>>,
  /// Refresh the detectable games database periodically when set.
  enable_db_update: bool,
  /// ETag captured by the startup fetch: seeds the refresh thread so its
  /// first hourly check is conditional instead of a redundant full rebuild.
  initial_db_etag: Option<String>,
  /// Application IDs never published by the scan thread (coexistence with
  /// a richer publisher elsewhere). Filtered right after the scan, so an
  /// ignored-only result behaves exactly like no game: null event, clear.
  ignored_ids: Vec<String>,

  #[cfg(not(target_os = "linux"))]
  sysinfo: Arc<Mutex<System>>,
}

unsafe impl Sync for ProcessServer {}

impl ProcessServer {
  // Eight discovery sources (DB, refresh, ignore-list, exclusions) thread
  // through here; bundling them would churn the public constructor for no
  // runtime gain.
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    detectable: Vec<Arc<DetectableActivity>>,
    event_sender: mpsc::Sender<ProcessDetectedEvent>,
    event_listeners: ProcessEventListeners,
    db_url: Option<String>,
    enable_db_update: bool,
    initial_db_etag: Option<String>,
    ignored_ids: Vec<String>,
    exclusions_url: Option<String>,
  ) -> Self {
    log!("[Process Scanner] Building Aho-Corasick patterns for main detectable activities...");
    let (ac, idx) = build_ac_patterns(&detectable);
    let (proton_ac, proton_idx) = build_proton_ac_patterns(&detectable);
    let (steam_map, name_map) = build_aux_maps(&detectable);
    log!("[Process Scanner] Done!");

    let server = ProcessServer {
      scanning: Arc::new(AtomicBool::new(false)),
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
      proton_detectable_indexes: Arc::new(Mutex::new(proton_idx)),
      proton_detectable_ac: Arc::new(Mutex::new(proton_ac)),

      // Event listeners
      event_listeners: Arc::new(Mutex::new(event_listeners)),

      // Detectable database auto-refresh
      db_url,
      enable_db_update,
      initial_db_etag,
      ignored_ids,
      exclusions: Arc::new(Mutex::new(Exclusions::default())),
      exclusions_url,
      steam_libraries: Arc::new(Mutex::new(SteamLibraries::discover())),
      detected_pids: Arc::new(Mutex::new(HashSet::new())),
      appid_cache: Arc::new(Mutex::new(HashMap::new())),
      scan_wake: Arc::new(Mutex::new(None)),

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
    // Never swap in an empty database (outage returning `[]`, corrupt
    // fetch): it would build a failing automaton and blind detection.
    // Keep serving the current data instead.
    if detectable.is_empty() {
      warn!("[Process Scanner] Refusing empty detectable database update, keeping current");
      return;
    }
    log!("[Process Scanner] Rebuilding Aho-Corasick patterns for main detectable activities...");
    let detectable: Vec<Arc<DetectableActivity>> = detectable.into_iter().map(Arc::new).collect();
    let (ac, idx) = build_ac_patterns(&detectable);
    let (proton_ac, proton_idx) = build_proton_ac_patterns(&detectable);
    let (steam_map, name_map) = build_aux_maps(&detectable);

    *self.detectable_list.lock().unwrap() = detectable;
    *self.detectable_ac.lock().unwrap() = ac;
    *self.detectable_indexes.lock().unwrap() = idx;
    *self.proton_detectable_ac.lock().unwrap() = proton_ac;
    *self.proton_detectable_indexes.lock().unwrap() = proton_idx;
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

  /// Replace the exclusions set (startup fetch, tests). The hourly refresh
  /// thread overwrites it on the same cadence when `exclusions_url` is set.
  pub fn set_exclusions(&self, exclusions: Exclusions) {
    *self.exclusions.lock().unwrap() = exclusions;
  }

  /// AppId whose Steam install dir prefixes `normalized_path` (already
  /// lowercased `/`-separated). Cloned out of the lock; tiny strings.
  pub(crate) fn steam_prefix_app_id(&self, normalized_path: &str) -> Option<String> {
    self
      .steam_libraries
      .lock()
      .unwrap()
      .match_prefix(normalized_path)
      .map(str::to_string)
  }

  /// SteamAppId for one pid, memoized: environ is kilobytes and never
  /// changes after exec, so re-reading it every 5s per process was pure
  /// waste (profiler: ~5KB of the ~5KB per-process cost). IO happens
  /// outside the lock; EXEC invalidates via [`ProcessServer::drop_appid`].
  fn cached_app_id(&self, pid: u64) -> Option<String> {
    if let Some(cached) = self.appid_cache.lock().unwrap().get(&pid) {
      return cached.clone();
    }
    let id = read_steam_app_id(pid);
    self.appid_cache.lock().unwrap().insert(pid, id.clone());
    id
  }

  /// Drop one pid's memoized AppId (EXEC: same pid, new image, possibly
  /// new environ). Called by the proc-events watcher before reclassifying.
  pub(crate) fn drop_appid(&self, pid: u64) {
    self.appid_cache.lock().unwrap().remove(&pid);
  }

  /// Revalidate the Steam libraries (stats only unless something changed).
  /// Called once per scan tick; the scan loop goes through here so tests
  /// can drive the same path.
  pub(crate) fn refresh_steam_libraries(&self) {
    self.steam_libraries.lock().unwrap().refresh_if_stale();
  }

  /// Refresh the exclusions list once, best-effort: failures keep the
  /// previous set (empty at first boot = current behavior).
  fn refresh_exclusions(&self) {
    let Some(url) = self.exclusions_url.clone() else {
      return;
    };
    match fetch_exclusions(&url) {
      Ok(exclusions) => {
        log!(
          "[Process Scanner] Exclusions updated: {} names, {} patterns",
          exclusions.executables.len(),
          exclusions.patterns.len()
        );
        self.set_exclusions(exclusions);
      }
      Err(err) => {
        warn!("[Process Scanner] Error updating exclusions, retrying in 1h: {err}");
      }
    }
  }

  pub fn start(&self, scan_interval: Duration) {
    let wait_time = scan_interval;
    let clone = self.clone();

    self.update_custom_detectables();

    // Periodically refresh the detectable games database (like pog5-rsrpc).
    // Sleep first: startup already fetched synchronously, so an immediate
    // refetch would parse the whole DB twice for the same data (double
    // transient memory + startup time for zero new information). Refreshes
    // are conditional (ETag): an unchanged database costs one header round
    // trip and zero parsing, so steady-state RSS never ratchets.
    if clone.enable_db_update && clone.db_url.is_some() {
      let db_clone = clone.clone();
      std::thread::spawn(move || {
        // Seeded from the startup fetch when available: the first check
        // is conditional like every other, instead of one guaranteed
        // redundant full rebuild per daemon lifetime.
        let mut etag = db_clone.initial_db_etag.clone();
        // Content hash of the last built database: guards against CDN etag
        // flaps (new tag, identical bytes), which a tag-only check would
        // rebuild pointlessly.
        let mut content_hash: Option<u64> = None;
        // Unlike the DB, exclusions are NOT fetched synchronously at
        // startup (tiny payload, empty = current behavior), so prime them
        // here instead of waiting an hour for the first set.
        db_clone.refresh_exclusions();
        loop {
          std::thread::sleep(Duration::from_secs(3600));
          db_clone.refresh_exclusions();
          let url = db_clone.db_url.clone().unwrap();
          match fetch_detectable_etag(&url, etag.as_deref(), content_hash) {
            Ok(FetchOutcome::Unchanged) => {
              log!(
                "[Process Scanner] DB check: unchanged (etag {})",
                etag.as_deref().unwrap_or("none")
              );
            }
            Ok(FetchOutcome::SameContent { etag: new_tag }) => {
              log!(
                "[Process Scanner] DB check: same bytes, new tag (etag {} -> {})",
                etag.as_deref().unwrap_or("none"),
                new_tag.as_deref().unwrap_or("none")
              );
              etag = new_tag;
            }
            Ok(FetchOutcome::Updated {
              etag: new_tag,
              content_hash: new_hash,
              detectable,
            }) => {
              log!(
                "[Process Scanner] DB updated: {} entries (etag {} -> {})",
                detectable.len(),
                etag.as_deref().unwrap_or("none"),
                new_tag.as_deref().unwrap_or("none")
              );
              etag = new_tag;
              content_hash = Some(new_hash);
              db_clone.update_main_detectables(detectable);
            }
            Err(err) => {
              warn!(
                "[Process Scanner] Error updating detectable database, retrying in 1h: {}",
                err
              );
            }
          }
        }
      });
    }

    std::thread::spawn(move || {
      // Register for early wakeups: the proc-events watcher unparks us
      // the moment a tracked game exits (Linux only; elsewhere None and
      // the cadence below is a plain sleep).
      *clone.scan_wake.lock().unwrap() = Some(std::thread::current());
      // Idle backoff state: consecutive ticks with no games detected.
      let mut idle_ticks: u32 = 0;
      // Run the process scan repeatedly (base cadence, stretched while idle)
      loop {
        let mut detected = match clone.scan_for_processes() {
          Ok(detected) => detected,
          Err(err) => {
            warn!(
              "[Process Scanner] Error while scanning processes, retrying: {}",
              err
            );
            wait_scan(wait_time);
            continue;
          }
        };
        // Coexistence filter: ignored app IDs behave as absent, so a richer
        // publisher elsewhere owns the slot (clears flow normally).
        let before = detected.len();
        detected = apply_ignore_list(detected, &clone.ignored_ids);
        if detected.len() != before {
          debug!(
            "[Process Scanner] Ignored {} detected game(s)",
            before - detected.len()
          );
        }
        // Track live game pids for the proc-events watcher: only THEIR
        // exits wake us early (a build storm's exits never cause a scan).
        *clone.detected_pids.lock().unwrap() =
          detected.iter().filter_map(|game| game.pid).collect();
        // Forward EVERY detected game, one event per slot. Downstream
        // publishes per app id and dedups repeats, so co-running games
        // each own their card instead of only the first.
        if !detected.is_empty() {
          for game in &detected {
            if clone
              .event_sender
              .send(ProcessDetectedEvent {
                activity: game.clone(),
              })
              .is_err()
            {
              warn!("[Process Scanner] Event receiver gone, retrying scan");
              wait_scan(wait_time);
              continue;
            }
          }
        }

        // If there are no detected processes, send an empty message.
        // Fail-soft like above: never panic the scan loop on send.
        if detected.is_empty() {
          let cleared = clone.event_sender.send(ProcessDetectedEvent {
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
          });
          if cleared.is_err() {
            warn!("[Process Scanner] Event receiver gone, retrying scan");
            wait_scan(wait_time);
            continue;
          }
        }

        // Idle backoff: consecutive empty ticks stretch the cadence
        // (base → 30s cap). Safe because game START arrives via EXEC
        // events instantly and EXITs of tracked games unpark us early —
        // polling only backstops what the watcher cannot (untracked
        // exits, DB refreshes). Any detection or early wake resets.
        if detected.is_empty() {
          idle_ticks = idle_ticks.saturating_add(1);
        } else {
          idle_ticks = 0;
        }
        let cadence = idle_wait(wait_time, idle_ticks);
        let wait_start = std::time::Instant::now();
        wait_scan(cadence);
        if wait_start.elapsed() < cadence.mul_f32(0.9) {
          idle_ticks = 0;
        }
      }
    });

    // Event-driven fast path (Linux): EXEC classifies one process at once,
    // EXIT of a tracked game wakes the scan above. Best-effort — setup
    // failure keeps pure polling, silently.
    #[cfg(target_os = "linux")]
    spawn_proc_watcher(self);
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

      let Ok(pid) = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .parse::<u64>()
      else {
        continue;
      };
      // Same single-pid reader as the EXEC fast path: unreadable pids
      // (kernel threads, zombies, races) are skipped, never fatal.
      if let Some(exec) = read_exec(pid) {
        processes.push(exec);
      }
    }

    Ok(processes)
  }

  /// Single reversed-path AC probe (main DB, then custom overrides).
  /// Shared lookup half of the scan: direct-path and cwd-joined probes
  /// match identically through here.
  pub(crate) fn ac_probe(
    &self,
    reversed_path: &str,
    detectable_list: &[Arc<DetectableActivity>],
  ) -> Option<(Arc<DetectableActivity>, usize)> {
    let ac = self.detectable_ac.lock().unwrap();
    if let Some(mat) = ac.find(reversed_path) {
      let pattern_id: PatternID = mat.pattern();
      let exe_index = self.detectable_indexes.lock().unwrap()[pattern_id.as_usize()];
      return Some((detectable_list[exe_index[0]].clone(), exe_index[1]));
    }
    let custom = self.custom_detectable_ac.lock().unwrap();
    if let Some(custom_ac) = custom.as_ref()
      && let Some(mat) = custom_ac.find(reversed_path)
    {
      let pattern_id: PatternID = mat.pattern();
      let exe_index = self.custom_detectable_indexes.lock().unwrap()[pattern_id.as_usize()];
      return Some((
        self.custom_detectables.lock().unwrap()[exe_index[0]].clone(),
        exe_index[1],
      ));
    }
    None
  }

  /// Proton fallback probe (main DB `win32` entries on Linux): same shape
  /// as [`ProcessServer::ac_probe`], consulted only after the native
  /// patterns, user overrides and the authoritative Steam AppId all miss.
  /// Empty automaton off-Linux, so this is a cheap `None` there.
  pub(crate) fn proton_probe(
    &self,
    reversed_path: &str,
    detectable_list: &[Arc<DetectableActivity>],
  ) -> Option<(Arc<DetectableActivity>, usize)> {
    let proton = self.proton_detectable_ac.lock().unwrap();
    let automaton = proton.as_ref()?;
    let mat = automaton.find(reversed_path)?;
    let pattern_id: PatternID = mat.pattern();
    let exe_index = self.proton_detectable_indexes.lock().unwrap()[pattern_id.as_usize()];
    Some((detectable_list[exe_index[0]].clone(), exe_index[1]))
  }

  /// Shared variant loop: try `path` plus its 64-bit-stripped variants
  /// against the native (`proton = false`) or Proton (`proton = true`)
  /// automaton. First hit in variant order wins.
  fn probe_variants(
    &self,
    path: &str,
    variant_bufs: &mut [String; 5],
    reversed_path: &mut String,
    detectable_list: &[Arc<DetectableActivity>],
    proton: bool,
  ) -> Option<(Arc<DetectableActivity>, usize)> {
    let variant_count = path_variants_into(path, variant_bufs);
    for variant in &variant_bufs[..variant_count] {
      reversed_path.clear();
      reversed_path.extend(variant.chars().rev());
      let found = if proton {
        self.proton_probe(reversed_path, detectable_list)
      } else {
        self.ac_probe(reversed_path, detectable_list)
      };
      if found.is_some() {
        return found;
      }
    }
    None
  }

  /// Classify one enumerated process, cheapest source first:
  /// native AC, bare-exe+cwd, authoritative Steam AppId, Proton (`win32`)
  /// AC, Steam install-dir, then the stem/folder heuristics — except when
  /// the launcher supplied a shortcut-range AppId (Steam-assigned
  /// non-Steam id): then stem/folder run before the install-dir. Lazy `/proc` reads (cwd,
  /// environ, stat) happen only on misses/hits respectively — never for
  /// the whole table. Extracted from the scan loop for reuse and testing;
  /// the loop itself just maps over it.
  #[hotpath::measure]
  pub(crate) fn match_process(
    &self,
    process: &Exec,
    detectable_list: &[Arc<DetectableActivity>],
    variant_bufs: &mut [String; 5],
    reversed_path: &mut String,
    obs_open: &mut bool,
  ) -> Option<Arc<DetectableActivity>> {
    // Process path with consistent slashes (original case: the
    // automata match ASCII case-insensitively). Borrowed until a
    // rewrite is actually needed — the common Linux case (no
    // backslashes, absolute path) allocates nothing at all.
    let mut process_path: std::borrow::Cow<str> = std::borrow::Cow::Borrowed(&process.path);

    if process_path.contains('\\') {
      process_path = std::borrow::Cow::Owned(process_path.replace('\\', "/"));
    }

    if !process_path.starts_with('/') {
      process_path = std::borrow::Cow::Owned(format!("/{process_path}"));
    }

    // Discord exclusions first: installers, crash reporters and friends
    // are invisible before any matching (one basename lookup instead of
    // the full probe chain, and they can never shadow a real game).
    // Before the OBS flag too: an excluded process is absent, period.
    // Only the tiny basename is lowercased (the exclusion list is).
    let basename = process_path
      .rsplit('/')
      .next()
      .unwrap_or(&process_path)
      .to_ascii_lowercase();
    if self.exclusions.lock().unwrap().is_excluded(&basename) {
      debug!("[Process Scanner] Excluded process, skipping: {basename}");
      return None;
    }

    // OBS binaries ship lowercase; the flag only feeds an unconsumed
    // observer callback, so original-case matching is exact enough.
    if !*obs_open && (process_path.contains("obs64") || process_path.contains("streamlabs")) {
      *obs_open = true;
    }

    // Aho-Corasick matching against the path and its 64-bit-stripped
    // variants (so `wow64.exe` also matches a `wow.exe` pattern, like
    // arrpc/pog5-rsrpc). First hit in variant order wins.
    let mut found = self.probe_variants(
      &process_path,
      variant_bufs,
      reversed_path,
      detectable_list,
      false,
    );

    // Proton bare-exe probe (DOOM Eternal case): argv[0] without
    // directories plus the process cwd often reconstructs the install
    // path the DB knows. Only for bare exes (paths with directories
    // already had their full match above); one readlink per miss.
    if found.is_none()
      && let Some(exe) = bare_exe(&process_path)
      && let Some(cwd) = read_cwd(process.pid)
    {
      let candidate = format!("{cwd}/{exe}");
      debug!(
        "[Process Scanner] Bare exe, probing cwd-joined path for pid {}",
        process.pid
      );
      found = self.probe_variants(
        &candidate,
        variant_bufs,
        reversed_path,
        detectable_list,
        false,
      );
      if found.is_some() {
        debug!("[Process Scanner] Cwd match for pid {}", process.pid);
      }
    }

    let (obj, exe_index) = match found {
      Some(found) => found,
      None => {
        // Lowercase copy for the case-sensitive tail below (store-id
        // maps, stem/folder heuristics). Paid only on misses — the hot
        // AC path above never allocates it.
        let lowered = process_path.to_ascii_lowercase();
        // The AppId comes memoized (one environ read per process
        // lifetime); cmdline fallback when environ is unreadable
        // (sandboxed Proton runtimes hide it from service contexts).
        let app_id = self.cached_app_id(process.pid);
        let (app_id, via_cmdline) = match app_id {
          Some(id) => (Some(id), false),
          None => (app_id_from_args(process.arguments.as_deref()), true),
        };
        if via_cmdline && let Some(id) = app_id.as_deref() {
          debug!(
            "[Process Scanner] AppId {} for pid {} from command line (environ unreadable)",
            id, process.pid
          );
        }
        // Authoritative Steam AppId first: a store id beats every fuzzy
        // path heuristic below.
        if let Some(hit) = match_steam_id(
          app_id.as_deref(),
          process.pid,
          &self.steam_map,
          detectable_list,
        ) {
          return Some(hit);
        }
        // An AppId the launcher gave us but the database doesn't know needs
        // a second look: Steam itself assigns high-bit-set ids
        // (e.g. 2532755798) to non-Steam shortcuts, while real store ids
        // are small. A shortcut-range id is a self-identified non-Steam
        // game, so its own exe/folder name beats install location; a
        // small unknown id is a real game missing from the DB, where
        // location (Steam's ground truth) still beats name guessing.
        let non_steam = app_id.as_deref().is_some_and(is_shortcut_id);
        if non_steam
          && let Some(hit) =
            match_name_or_folder(&lowered, process.pid, &self.name_map, detectable_list)
        {
          return Some(hit);
        }
        // Proton fallback: `win32` executables from the main DB. Catches
        // Wine/Proton games whose store id is unreadable and whose exe is
        // too generic for the stem/folder heuristics.
        if let Some((obj, exe_index)) = self.probe_variants(
          &process_path,
          variant_bufs,
          reversed_path,
          detectable_list,
          true,
        ) {
          return finish_direct_hit(&obj, exe_index, process);
        }
        // Steam's own word: the process runs under a known install dir,
        // so it inherits that entry's AppId. Beats name guessing below,
        // loses to a DB-declared path above.
        if let Some(library_appid) = self.steam_prefix_app_id(&lowered)
          && let Some(hit) = match_steam_id(
            Some(&library_appid),
            process.pid,
            &self.steam_map,
            detectable_list,
          )
        {
          debug!(
            "[Process Scanner] Steam library match: {} (appid {})",
            hit.name, library_appid
          );
          return Some(hit);
        }
        // Last resort: exe-stem == game name, then install-folder walk
        // (already consulted above for shortcut-range AppIds).
        if non_steam {
          return None;
        }
        return match_name_or_folder(&lowered, process.pid, &self.name_map, detectable_list);
      }
    };

    finish_direct_hit(&obj, exe_index, process)
  }

  #[hotpath::measure]
  pub fn scan_for_processes(
    &self,
  ) -> Result<Vec<Arc<DetectableActivity>>, Box<dyn std::error::Error>> {
    #[cfg(not(target_os = "linux"))]
    let processes = self.process_list()?;
    #[cfg(target_os = "linux")]
    let processes = ProcessServer::process_list()?;

    debug!("[Process Scanner] Process scan triggered");

    if self.scanning.load(std::sync::atomic::Ordering::Relaxed) {
      debug!("[Process Scanner] Scanning already in progress");
      return Err("Scanning already in progress".into());
    }

    let mut obs_open = false;

    let detectable_list = self
      .detectable_list
      .lock()
      .map_err(|e| format!("detectable_list lock poisoned: {e}"))?;

    // Steam generation marker: one stat per watched libraryfolders.vdf;
    // the parse itself runs only when something actually changed.
    self.refresh_steam_libraries();

    // Drop memoized AppIds of dead pids (pid reuse must never serve a
    // stale id): one set build + retain per tick, replacing hundreds of
    // kilobyte environ re-reads.
    let live: HashSet<u64> = processes.iter().map(|process| process.pid).collect();
    self
      .appid_cache
      .lock()
      .unwrap()
      .retain(|pid, _| live.contains(pid));

    let mut reversed_path = String::with_capacity(256);
    // Variant scratch space, reused for every process: the scan allocates
    // nothing per process at steady state (see path_variants_into).
    let mut variant_bufs: [String; 5] = Default::default();

    let mut detected_list: Vec<Arc<DetectableActivity>> = processes
      .iter()
      .filter_map(|process| {
        self.match_process(
          process,
          &detectable_list,
          &mut variant_bufs,
          &mut reversed_path,
          &mut obs_open,
        )
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

    debug!("[Process Scanner] Process scan complete");

    Ok(detected_list)
  }
}

/// Sleep the scan cadence. `park_timeout` (not `sleep`) so the proc-events
/// watcher can wake the loop early when a tracked game exits; a permit
/// stored by an unpark during scan work just causes one early rescan,
/// which downstream dedups harmlessly.
fn wait_scan(wait_time: Duration) {
  std::thread::park_timeout(wait_time);
}

/// Idle-stretched cadence: base × 2^idle_ticks, capped at 30s
/// (5s → 10s → 20s → 30s at the default base). Overflow-safe.
pub(crate) fn idle_wait(base: Duration, idle_ticks: u32) -> Duration {
  const MAX_BACKOFF: Duration = Duration::from_secs(30);
  let stretched = base
    .checked_mul(1 << idle_ticks.min(4))
    .unwrap_or(MAX_BACKOFF);
  stretched.min(MAX_BACKOFF)
}

/// Spawn the netlink dispatch (Linux): EXEC classifies one process and
/// emits hits at once; EXIT of a tracked game unparks the scan loop for
/// an immediate natural clear. Setup failure (or a dead receiver on
/// shutdown) ends the thread quietly — polling carries on.
#[cfg(target_os = "linux")]
fn spawn_proc_watcher(server: &ProcessServer) {
  use super::proc_events::{ProcEvent, watch};

  let (tx, rx) = mpsc::channel();
  let dispatch = server.clone();
  std::thread::spawn(move || {
    let mut variant_bufs: [String; 5] = Default::default();
    let mut reversed_path = String::with_capacity(256);
    for event in rx {
      match event {
        ProcEvent::Exec(pid) => {
          let Some(exec) = read_exec(pid) else {
            debug!("[Process Scanner] exec event: pid {pid} unreadable, skipping");
            continue;
          };
          // Same pid, new image: the memoized AppId may be stale.
          dispatch.drop_appid(pid);
          let detectable_list = dispatch.detectable_list.lock().unwrap();
          let mut obs_open = false;
          if let Some(hit) = dispatch.match_process(
            &exec,
            &detectable_list,
            &mut variant_bufs,
            &mut reversed_path,
            &mut obs_open,
          ) && let Some(game_pid) = hit.pid
          {
            debug!(
              "[Process Scanner] exec event: pid {pid} matched {}",
              hit.name
            );
            dispatch.detected_pids.lock().unwrap().insert(game_pid);
            // Receiver gone means shutdown: end the thread, polling dies
            // with the daemon anyway.
            if dispatch
              .event_sender
              .send(ProcessDetectedEvent { activity: hit })
              .is_err()
            {
              break;
            }
          }
        }
        ProcEvent::Exit(pid) => {
          // Only tracked games wake the scan: the rest of the system's
          // exits (build storms included) cost one HashSet lookup.
          if dispatch.detected_pids.lock().unwrap().remove(&pid)
            && let Some(thread) = dispatch.scan_wake.lock().unwrap().as_ref()
          {
            thread.unpark();
          }
        }
      }
    }
  });
  std::thread::spawn(move || {
    if let Err(err) = watch(tx) {
      debug!("[Process Scanner] proc-events unavailable ({err}), polling only");
    }
  });
}

/// Read one process's cmdline into an `Exec` (Linux). `None` for kernel
/// threads, zombies, vanished or unreadable pids — the caller just skips
/// them; the periodic scan is the backstop.
#[cfg(target_os = "linux")]
fn read_exec(pid: u64) -> Option<Exec> {
  let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()?;
  if cmdline.is_empty() {
    return None;
  }
  let mut cmd_iter = cmdline.split('\0');
  let (cmd_path, cmd_args) = (
    cmd_iter.next().unwrap_or("").to_string(),
    cmd_iter.collect::<Vec<_>>().join(" "),
  );
  Some(Exec {
    pid,
    path: cmd_path,
    arguments: if cmd_args.is_empty() {
      None
    } else {
      Some(cmd_args)
    },
  })
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
  release_platform_arenas();
}

/// Linux (glibc/musl).
#[cfg(target_os = "linux")]
fn release_platform_arenas() {
  // SAFETY: malloc_trim only advises the allocator to release free pages;
  // it cannot invalidate live allocations, so it is always safe to call.
  unsafe {
    libc::malloc_trim(0);
  }
}

/// macOS: drain purgeable memory in all zones.
#[cfg(target_os = "macos")]
fn release_platform_arenas() {
  // Declared locally: libc 0.2 exposes only the zone-struct field, not
  // this stable Darwin function (malloc/malloc.h, present since 10.6).
  unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t) -> libc::size_t;
  }
  // SAFETY: (NULL, 0) means "all zones, no goal" and is advisory-only;
  // it cannot invalidate live allocations.
  unsafe {
    malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
  }
}

/// Windows: compact our own process heap.
#[cfg(target_os = "windows")]
fn release_platform_arenas() {
  // SAFETY: HeapCompact with flags=0 only coalesces free blocks of the
  // given heap; it cannot invalidate live allocations.
  unsafe {
    let heap = winapi::um::heapapi::GetProcessHeap();
    if !heap.is_null() {
      winapi::um::heapapi::HeapCompact(heap, 0);
    }
  }
}

/// Other platforms: nothing to release through a stable API.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn release_platform_arenas() {}

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

/// No environ AppIds outside Linux (Steam matching there was already
/// limited to the exe/name paths).
#[cfg(not(target_os = "linux"))]
fn read_steam_app_id(_pid: u64) -> Option<String> {
  None
}

/// Steam AppId from a process command line (`reaper SteamLaunch
/// AppId=4508340 ...`): fallback when `/proc/<pid>/environ` is
/// unreadable. Sandboxed Proton runtimes (pressure-vessel/bwrap) hide
/// environ from service contexts while cmdline stays readable — without
/// this, every such game is invisible to automatic detection. Same trust
/// as environ (both launcher-provided, display-only use): the token must
/// stand alone (`AppId=` at a word boundary, followed by digits).
pub(crate) fn app_id_from_args(arguments: Option<&str>) -> Option<String> {
  const TOKEN: &str = "AppId=";
  let args = arguments?;
  let mut rest = args;
  while let Some(pos) = rest.find(TOKEN) {
    let boundary = pos == 0
      || rest[..pos]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_ascii_alphanumeric());
    rest = &rest[pos + TOKEN.len()..];
    if !boundary {
      continue;
    }
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
      return Some(digits);
    }
  }
  None
}

/// Whether a process is currently suspended (`T` state: SIGSTOP'd, e.g. a
/// paused game). Suspended games show a frozen frame (or nothing) — they
/// are not being played, so matches on them are discarded and the scan
/// treats them as absent (clears). Checked lazily, only for matched
/// processes: one tiny `stat` read per hit, never per scan.
#[cfg(target_os = "linux")]
pub(crate) fn is_suspended(pid: u64) -> bool {
  std::fs::read_to_string(format!("/proc/{pid}/stat"))
    .ok()
    .and_then(|stat| parse_stat_state(&stat))
    .is_some_and(|state| state == 'T' || state == 't')
}

/// Stubbed on non-Linux (Steam matching there is already limited).
#[cfg(not(target_os = "linux"))]
fn is_suspended(_pid: u64) -> bool {
  false
}

/// Process state from `/proc/<pid>/stat`: the field right after the last
/// `)` (comm may itself contain spaces and parens). `None` when unreadable
/// or malformed — never counted as suspended. Linux-only like its sole
/// caller: without `/proc` there is nothing to parse.
#[cfg(target_os = "linux")]
pub(crate) fn parse_stat_state(stat: &str) -> Option<char> {
  stat
    .rfind(')')
    .and_then(|end| stat[end + 1..].split_whitespace().next())
    .and_then(|state| state.chars().next())
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
  // Epoch millis as a NUMBER: Discord's schema (and strict clients) want an
  // integer here — a stringified timestamp is silently dropped downstream.
  let start_ms = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .ok()
    .and_then(|age| u64::try_from(age.as_millis()).ok())
    .unwrap_or(0);
  new_activity.timestamp = Some(start_ms);
  Arc::new(new_activity)
}

/// Auxiliary lookup maps over the main DB:
/// Steam store id -> activity index, normalized game name -> activity index.
/// Alternative titles (`aliases`) join the name map under the same
/// conservative gate — exact, multi-word only — so a generic alias can
/// never collide; canonical names are inserted first and win ties.
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
    if let Some(aliases) = activity.aliases.as_ref() {
      for alias in aliases {
        let normalized = normalize_name(alias);
        if name_matchable(&normalized) {
          name_map.entry(normalized).or_insert(index);
        }
      }
    }
  }

  log!(
    "[Process Scanner] Aux maps: {} steam ids, {} matchable names",
    steam_map.len(),
    name_map.len()
  );

  (steam_map, name_map)
}

/// Drop matches on suspended processes (see [`is_suspended`]): a SIGSTOP'd
/// game shows a frozen frame at best — it is not being played. Single
/// choke point for every aux hit (appid/stem/folder), mirroring the scan
/// loop's post-match check for AC hits.
fn live_or_none(obj: &Arc<DetectableActivity>, pid: u64) -> Option<Arc<DetectableActivity>> {
  if is_suspended(pid) {
    debug!("[Process Scanner] Ignoring suspended process (pid {pid})");
    return None;
  }
  Some(stamp_activity(obj, pid))
}

/// Drop scan results whose application id is ignored, preserving order.
/// Pure: an ignored-only scan yields an empty vec, which the scan thread
/// already turns into the normal null event (clear). Tested below.
pub(crate) fn apply_ignore_list(
  detected: Vec<Arc<DetectableActivity>>,
  ignored_ids: &[String],
) -> Vec<Arc<DetectableActivity>> {
  if ignored_ids.is_empty() {
    return detected;
  }
  detected
    .into_iter()
    .filter(|game| !ignored_ids.iter().any(|id| *id == game.id))
    .collect()
}

/// Shared tail of every direct path hit (native or Proton): when the
/// database declares `arguments` for an executable, the process command
/// line must contain them (parity with arrpc/pog5-rsrpc, e.g. TF2
/// `-game tf`); then the suspended check + timestamp stamp via
/// `live_or_none`.
fn finish_direct_hit(
  obj: &Arc<DetectableActivity>,
  exe_index: usize,
  process: &Exec,
) -> Option<Arc<DetectableActivity>> {
  let executable = &obj.executables.as_ref().unwrap()[exe_index];

  if let Some(exec_args) = &executable.arguments {
    let has_args = process
      .arguments
      .as_ref()
      .is_some_and(|args| args.contains(exec_args));
    if !has_args {
      debug!(
        "[Process Scanner] Argument mismatch for pid {}, skipping",
        process.pid
      );
      return None;
    }
  }

  live_or_none(obj, process.pid)
}

/// Steam's non-Steam shortcut range: ids Steam itself assigns when a
/// shortcut is added carry the high bit (`crc32(exe + name) | 0x80000000`,
/// e.g. 2532755798 for "How to Fish" — verified against a live
/// `shortcuts.vdf`); real store ids are small. Unknown to the DB +
/// shortcut-range means a self-identified non-Steam game.
pub(crate) fn is_shortcut_id(app_id: &str) -> bool {
  app_id.parse::<u32>().is_ok_and(|id| id & 0x8000_0000 != 0)
}

/// Authoritative aux lookup: `SteamAppId` (store games with empty
/// `executables`, legit Steam). Runs before every fuzzy heuristic — a
/// store id beats path guessing.
pub(crate) fn match_steam_id(
  steam_app_id: Option<&str>,
  pid: u64,
  steam_map: &Mutex<HashMap<String, usize>>,
  detectable_list: &[Arc<DetectableActivity>],
) -> Option<Arc<DetectableActivity>> {
  let appid = steam_app_id?;
  let &idx = steam_map.lock().unwrap().get(appid)?;
  let obj = detectable_list.get(idx)?;
  debug!(
    "[Process Scanner] Steam match: {} (appid {})",
    obj.name, appid
  );
  live_or_none(obj, pid)
}
/// Heuristic aux lookup: exact exe-stem == multi-word
/// game name, then the install-folder walk. Runs after the Proton AC probe
/// in the scan loop — a DB-declared path (even `win32`) beats guessing.
pub(crate) fn match_name_or_folder(
  process_path: &str,
  pid: u64,
  name_map: &Mutex<HashMap<String, usize>>,
  detectable_list: &[Arc<DetectableActivity>],
) -> Option<Arc<DetectableActivity>> {
  let stem = exe_stem(process_path);
  if name_matchable(&stem)
    && let Some(&idx) = name_map.lock().unwrap().get(&stem)
    && let Some(obj) = detectable_list.get(idx)
  {
    debug!(
      "[Process Scanner] Name match: {} (exe stem `{}`)",
      obj.name, stem
    );
    return live_or_none(obj, pid);
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
      debug!(
        "[Process Scanner] Folder match: {} (folder `{}`)",
        obj.name, folder
      );
      return live_or_none(obj, pid);
    }
  }

  None
}

/// Outcome of one conditional refresh: either the database changed (new
/// ETag + parsed activities), its bytes are identical (new ETag, same
/// content: CDN etags flap without content changes), or the server said
/// 304 (keep everything as is).
pub(crate) enum FetchOutcome {
  Unchanged,
  SameContent {
    etag: Option<String>,
  },
  Updated {
    etag: Option<String>,
    content_hash: u64,
    detectable: Vec<DetectableActivity>,
  },
}

/// Content hash for change detection (std-only SipHash: deterministic
/// within a run, which is the only scope it is ever compared in).
pub(crate) fn body_hash(body: &str) -> u64 {
  use std::hash::{DefaultHasher, Hash, Hasher};
  let mut hasher = DefaultHasher::new();
  body.hash(&mut hasher);
  hasher.finish()
}

/**
 * Fetch Discord's detection exclusions (installer/crash-reporter names +
 * regex patterns). Tiny payload (a few KB): plain GET with a 1 MiB cap, no
 * ETag dance — the hourly cadence dominates the cost, and parsing is
 * `tolerant by design` (see [`parse_exclusions`]).
 */
pub(crate) fn fetch_exclusions(url: &str) -> Result<Exclusions, Box<dyn std::error::Error>> {
  let body = ureq::get(url)
    .call()?
    .into_body()
    .with_config()
    .limit(1024 * 1024)
    .read_to_string()?;
  Ok(parse_exclusions(&body))
}

/**
 * Fetch the detectable games database, skipping the download when it has
 * not changed since `etag` (Discord answers `304`, `ETag` + `max-age=3600`
 * line up with the hourly cadence). A 304 costs one header round trip and
 * zero parsing, so idle hours leave RSS untouched.
 */
pub(crate) fn fetch_detectable_etag(
  url: &str,
  etag: Option<&str>,
  known_hash: Option<u64>,
) -> Result<FetchOutcome, Box<dyn std::error::Error>> {
  let mut request = ureq::get(url);
  if let Some(tag) = etag {
    request = request.header("If-None-Match", tag);
  }
  let response = request.call()?;
  if response.status().as_u16() == 304 {
    return Ok(FetchOutcome::Unchanged);
  }
  let etag = response
    .headers()
    .get("etag")
    .and_then(|value| value.to_str().ok())
    .map(str::to_string);
  let body = response
    .into_body()
    .with_config()
    .limit(64 * 1024 * 1024)
    .read_to_string()?;

  // Same bytes under a new tag (CDN etag flaps): skip the rebuild, which
  // is where the retained memory comes from — not the download.
  let content_hash = body_hash(&body);
  if known_hash.is_some_and(|known| known == content_hash) {
    return Ok(FetchOutcome::SameContent { etag });
  }

  // Direct parse first: serde skips unknown fields, so the full body
  // parses with zero DOM overhead (~5x less transient memory than the
  // trimmed-Value pass). The trimming pass stays as fallback for entries
  // missing required fields (it defaults them); raw last.
  if let Ok(parsed) = serde_json::from_str::<Vec<DetectableActivity>>(&body) {
    return Ok(FetchOutcome::Updated {
      etag,
      content_hash,
      detectable: parsed,
    });
  }
  if let Ok(trimmed) = super::super::detection::trim_detectable_value(&body)
    && let Ok(parsed) = serde_json::from_value::<Vec<DetectableActivity>>(trimmed)
  {
    return Ok(FetchOutcome::Updated {
      etag,
      content_hash,
      detectable: parsed,
    });
  }
  Ok(FetchOutcome::Updated {
    etag,
    content_hash,
    detectable: serde_json::from_str(&body)?,
  })
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

/// Exe file name when the normalized argv[0] carries no directories
/// (bare exe, e.g. some Proton launches): `None` when directories are
/// present (the direct-path match covers those) or the name is empty.
pub(crate) fn bare_exe(normalized_path: &str) -> Option<&str> {
  let trimmed = normalized_path.strip_prefix('/').unwrap_or(normalized_path);
  if trimmed.is_empty() || trimmed.contains('/') {
    return None;
  }
  Some(trimmed)
}

/// Process working directory via `/proc/<pid>/cwd` (one readlink, no file
/// content). Proton launches with the game dir as cwd, so joining it with
/// a bare exe reconstructs the install path the DB knows (DOOM Eternal
/// case). Read lazily: only bare-exe misses pay for it.
#[cfg(target_os = "linux")]
fn read_cwd(pid: u64) -> Option<String> {
  let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
  Some(cwd.to_str()?.to_ascii_lowercase())
}

/// No cwd outside Linux (Steam matching there is already limited).
#[cfg(not(target_os = "linux"))]
fn read_cwd(_pid: u64) -> Option<String> {
  None
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

        exe_patterns.push(normalize_exe_pattern(&executable.name));
        exe_indexes.push([activity_index, exe_index]);
      }
    }
  }

  (build_ac_automaton(exe_patterns).unwrap(), exe_indexes)
}

/// Build the automaton with ASCII case-insensitive matching: process
/// paths are compared in their original case, so the scan loop never
/// allocates a lowercased copy per process (the single hottest
/// allocation in the profiler). Slashes/case in patterns are normalized
/// at build time (rare), never per scan (hot).
fn build_ac_automaton(exe_patterns: Vec<String>) -> Result<AhoCorasick, aho_corasick::BuildError> {
  AhoCorasick::builder()
    .ascii_case_insensitive(true)
    .build(exe_patterns)
}

/// Normalize one DB executable name into a reversed AC pattern:
/// consistent slashes, leading `/` (`>` becomes `/`, arrpc parity).
/// Case is left alone — the automaton matches insensitively.
fn normalize_exe_pattern(name: &str) -> String {
  // Make paths consistent, and fix some additional checks
  let mut exec_name = name.replace('\\', "/");

  // Checks adapted from arrpc, remain the '>' in DetectableActivity for later argument checks
  if exec_name.starts_with(">") {
    exec_name.replace_range(0..1, "/");
  } else if !exec_name.starts_with("/") {
    exec_name.insert(0, '/');
  }

  exec_name.chars().rev().collect::<String>()
}

/// Proton fallback automaton: `win32` executables from the main DB, for
/// Wine/Proton games on Linux whose store id is unreadable and whose exe
/// is too generic for the stem/folder heuristics. Linux-only: `None`
/// (plus empty indexes) elsewhere, so the probe is a cheap miss off-Linux.
fn build_proton_ac_patterns(
  detectables: &[Arc<DetectableActivity>],
) -> (Option<AhoCorasick>, Vec<[usize; 2]>) {
  #[cfg(not(target_os = "linux"))]
  {
    let _ = detectables;
    return (None, Vec::new());
  }
  #[cfg(target_os = "linux")]
  {
    let mut exe_patterns: Vec<String> = Vec::new();
    let mut exe_indexes: Vec<[usize; 2]> = Vec::new();

    for (activity_index, activity) in detectables.iter().enumerate() {
      if let Some(executables) = &activity.executables {
        for (exe_index, executable) in executables.iter().enumerate() {
          if executable.is_launcher || executable.os != "win32" {
            continue;
          }
          exe_patterns.push(normalize_exe_pattern(&executable.name));
          exe_indexes.push([activity_index, exe_index]);
        }
      }
    }

    if exe_patterns.is_empty() {
      return (None, Vec::new());
    }
    log!(
      "[Process Scanner] Proton fallback: {} win32 patterns",
      exe_patterns.len()
    );
    (Some(build_ac_automaton(exe_patterns).unwrap()), exe_indexes)
  }
}
