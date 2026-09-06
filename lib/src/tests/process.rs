use std::sync::{Arc, Mutex};

use crate::detection::{DetectableActivity, ThirdPartySku};
use crate::server::process::{build_aux_maps, exe_stem, match_aux_process, name_matchable};

fn activity(id: &str, name: &str, steam_id: Option<&str>) -> Arc<DetectableActivity> {
  Arc::new(DetectableActivity {
    bot_public: None,
    bot_require_code_grant: None,
    cover_image: None,
    description: None,
    developers: None,
    executables: None,
    flags: None,
    guild_id: None,
    hook: true,
    icon: None,
    id: id.to_string(),
    name: name.to_string(),
    publishers: None,
    rpc_origins: None,
    splash: None,
    third_party_skus: steam_id.map(|sid| {
      vec![ThirdPartySku {
        distributor: "steam".to_string(),
        id: Some(sid.to_string()),
        sku: None,
      }]
    }),
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
  })
}

#[test]
fn exe_stem_strips_dirs_and_extension() {
  assert_eq!(
    exe_stem("/home/user/games/how to fish/how to fish.exe"),
    "how to fish"
  );
  assert_eq!(exe_stem("/usr/bin/fish"), "fish");
  assert_eq!(exe_stem("/games/nfs11remastered.exe"), "nfs11remastered");
}

#[test]
fn name_matchable_rejects_generic_stems() {
  assert!(!name_matchable("fish"));
  assert!(!name_matchable("steam"));
  assert!(!name_matchable("game"));
  assert!(!name_matchable("reaper"));
  assert!(name_matchable("how to fish"));
  assert!(name_matchable("need for speed hot pursuit remastered"));
}

#[test]
fn aux_maps_cover_empty_executable_entries() {
  let db = vec![
    activity("1", "How to Fish", Some("4001890")),
    activity("2", "Fish", Some("999")),
  ];
  let (steam_map, name_map) = build_aux_maps(&db);
  assert_eq!(steam_map.get("4001890"), Some(&0));
  // multi-word name indexed, single-word name excluded
  assert_eq!(name_map.get("how to fish"), Some(&0));
  assert!(!name_map.contains_key("fish"));
}

#[test]
fn aux_match_prefers_steam_then_name() {
  let db = vec![activity("1", "How to Fish", Some("4001890"))];
  let (steam_map, name_map) = build_aux_maps(&db);
  let steam_map = Mutex::new(steam_map);
  let name_map = Mutex::new(name_map);

  // Steam AppId hit (legit Steam install)
  let hit = match_aux_process(
    "/home/user/steamapps/common/how to fish/how to fish.exe",
    Some("4001890"),
    123,
    &steam_map,
    &name_map,
    &db,
  );
  assert_eq!(hit.unwrap().id, "1");

  // No AppId (launcher shortcut): exe-stem fallback hits the same entry
  let hit = match_aux_process(
    "/home/user/games/how to fish/how to fish.exe",
    Some("2532755798"),
    456,
    &steam_map,
    &name_map,
    &db,
  );
  assert_eq!(hit.unwrap().id, "1");

  // Generic shell must never match
  let miss = match_aux_process("/usr/bin/fish", None, 789, &steam_map, &name_map, &db);
  assert!(miss.is_none());
}

#[test]
fn aux_match_falls_back_to_install_folder() {
  let db = vec![
    activity("1", "How to Fish", Some("4001890")),
    activity("2", "Meccha Chameleon", Some("4704690")),
  ];
  let (steam_map, name_map) = build_aux_maps(&db);
  let steam_map = Mutex::new(steam_map);
  let name_map = Mutex::new(name_map);

  // Hydra-style layout: generic Unreal exe, title only in the folders
  // (wine path, already slash-normalized and lowercased by the caller).
  let hit = match_aux_process(
    "/home/user/games/meccha chameleon/meccha chameleon/chameleon/binaries/win64/penguinhotel-win64-shipping.exe",
    Some("2987654321"),
    201,
    &steam_map,
    &name_map,
    &db,
  );
  assert_eq!(hit.unwrap().id, "2");

  // Steam AppId still wins over a conflicting folder name.
  let hit = match_aux_process(
    "/home/user/steamapps/common/meccha chameleon/how to fish.exe",
    Some("4001890"),
    202,
    &steam_map,
    &name_map,
    &db,
  );
  assert_eq!(hit.unwrap().id, "1");

  // Generic folders alone never match, even nested deep.
  let miss = match_aux_process(
    "/home/user/games/some game/binaries/win64/game-win64-shipping.exe",
    Some("2987654321"),
    203,
    &steam_map,
    &name_map,
    &db,
  );
  assert!(miss.is_none());

  // Dotted components (versions, hidden dirs) are skipped, real title
  // behind them still hits.
  let hit = match_aux_process(
    "/home/user/.local/share/games/how to fish/v1.2.3/how to fish.bin",
    None,
    204,
    &steam_map,
    &name_map,
    &db,
  );
  assert_eq!(hit.unwrap().id, "1");
}

#[test]
fn path_variants_into_matches_legacy_semantics() {
  use crate::server::process::path_variants_into;

  fn variants(path: &str) -> Vec<String> {
    let mut bufs: [String; 5] = Default::default();
    let count = path_variants_into(path, &mut bufs);
    bufs[..count].to_vec()
  }

  // Base path always first, then 64-bit-stripped forms.
  assert_eq!(
    variants("/games/wow64.exe"),
    vec!["/games/wow64.exe".to_string(), "/games/wow.exe".to_string()]
  );
  // No markers: single variant, and buffers reuse without reallocating.
  let mut bufs: [String; 5] = Default::default();
  let first = path_variants_into("/usr/bin/fish", &mut bufs);
  let ptrs: Vec<*const String> = bufs.iter().map(|s| s as *const String).collect();
  let second = path_variants_into("/usr/bin/fish", &mut bufs);
  assert_eq!((first, second), (1, 1));
  assert_eq!(
    ptrs,
    bufs.iter().map(|s| s as *const String).collect::<Vec<_>>()
  );
  // Dedup and capacity cap hold (base + 4 markers max).
  let many = variants("/64/x64/.x64/_64/game64.exe");
  assert_eq!(many[0], "/64/x64/.x64/_64/game64.exe");
  assert!(many.len() <= 5);
  assert!(many.iter().all(|v| !v.is_empty()));
}

#[test]
fn parse_stat_state_reads_after_comm() {
  use crate::server::process::parse_stat_state;

  // comm may contain spaces and parens; state is the field after the
  // LAST ')'. Shapes from proc(5).
  assert_eq!(parse_stat_state("1234 (fish) S 1 2 3"), Some('S'));
  assert_eq!(
    parse_stat_state("61442 (PenguinHotel-Win64-Shipping.exe) T 1 2 3"),
    Some('T')
  );
  assert_eq!(parse_stat_state("99 (weird (name)) R 1"), Some('R'));
  assert_eq!(parse_stat_state("garbage without parens"), None);
  assert_eq!(parse_stat_state("1 (x)"), None);
  assert_eq!(parse_stat_state(""), None);
}

#[test]
#[cfg(target_os = "linux")]
fn own_running_process_is_not_suspended() {
  use crate::server::process::is_suspended;

  // The test runner itself is running (R/S), never stopped.
  assert!(!is_suspended(u64::from(std::process::id())));
}

#[test]
fn bare_exe_detects_missing_directories_only() {
  use crate::server::process::bare_exe;

  assert_eq!(bare_exe("/doomx64.exe"), Some("doomx64.exe"));
  assert_eq!(bare_exe("/DOOMX64.EXE"), Some("DOOMX64.EXE"));
  assert_eq!(bare_exe("/games/doom/doomx64.exe"), None);
  assert_eq!(bare_exe("/"), None);
  assert_eq!(bare_exe(""), None);
}

#[test]
fn ac_probe_needs_directories_that_cwd_reconstructs() {
  use std::sync::Arc;

  use crate::detection::{DetectableActivity, Executable};
  use crate::server::process::{ProcessEventListeners, ProcessServer};

  let entry = DetectableActivity {
    bot_public: None,
    bot_require_code_grant: None,
    cover_image: None,
    description: None,
    developers: None,
    executables: Some(vec![Executable {
      name: "doom/doomx64.exe".to_string(),
      is_launcher: false,
      os: "linux".to_string(),
      arguments: None,
    }]),
    flags: None,
    guild_id: None,
    hook: true,
    icon: None,
    id: "424242424242424242".to_string(),
    name: "Doom Eternal Probe".to_string(),
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
  };
  let arcs = vec![Arc::new(entry)];
  let (_tx, _rx) = std::sync::mpsc::channel();
  let server = ProcessServer::new(
    arcs.clone(),
    _tx,
    ProcessEventListeners::default(),
    None,
    false,
  );
  let reversed = |path: &str| path.chars().rev().collect::<String>();
  // Bare exe alone misses (no directories for the suffix to anchor on)...
  assert!(server.ac_probe(&reversed("/doomx64.exe"), &arcs).is_none());
  // ...while the cwd-joined candidate hits the same entry.
  assert_eq!(
    server
      .ac_probe(&reversed("/games/doom/doomx64.exe"), &arcs)
      .map(|(obj, _)| obj.id.clone()),
    Some("424242424242424242".to_string())
  );
}
