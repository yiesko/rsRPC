use std::sync::{Arc, Mutex};

use crate::detection::{DetectableActivity, ThirdPartySku};
use crate::server::process::{
  build_aux_maps, exe_stem, match_name_or_folder, match_steam_id, name_matchable,
};

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
  let hit = match_steam_id(Some("4001890"), 123, &steam_map, &db);
  assert_eq!(hit.unwrap().id, "1");

  // No AppId (launcher shortcut): exe-stem fallback hits the same entry
  let hit = match_name_or_folder(
    "/home/user/games/how to fish/how to fish.exe",
    456,
    &name_map,
    &db,
  );
  assert_eq!(hit.unwrap().id, "1");

  // Generic shell must never match
  let miss = match_name_or_folder("/usr/bin/fish", 789, &name_map, &db);
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
  let hit = match_name_or_folder(
    "/home/user/games/meccha chameleon/meccha chameleon/chameleon/binaries/win64/penguinhotel-win64-shipping.exe",
    201,
    &name_map,
    &db,
  );
  assert_eq!(hit.unwrap().id, "2");

  // Steam AppId still wins over a conflicting folder name (the folder
  // alone would say "2", the store id says "1").
  let hit = match_steam_id(Some("4001890"), 202, &steam_map, &db);
  assert_eq!(hit.unwrap().id, "1");
  let folder_says = match_name_or_folder(
    "/home/user/steamapps/common/meccha chameleon/game.exe",
    202,
    &name_map,
    &db,
  );
  assert_eq!(folder_says.unwrap().id, "2");

  // Generic folders alone never match, even nested deep.
  let miss = match_name_or_folder(
    "/home/user/games/some game/binaries/win64/game-win64-shipping.exe",
    203,
    &name_map,
    &db,
  );
  assert!(miss.is_none());

  // Dotted components (versions, hidden dirs) are skipped, real title
  // behind them still hits.
  let hit = match_name_or_folder(
    "/home/user/.local/share/games/how to fish/v1.2.3/how to fish.bin",
    204,
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
#[cfg(target_os = "linux")]
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
    None,
    Vec::new(),
    None,
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

#[test]
fn conditional_refresh_skips_unchanged_database() {
  use std::io::{Read, Write};

  use crate::server::process::fetch_detectable_etag;

  // Local stub server: 304 when the tag matches the last issued one;
  // 200 + tiny DB with a rotating tag otherwise (CDN flap simulator).
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  let port = listener.local_addr().unwrap().port();
  let issued: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
  let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
  std::thread::spawn(move || {
    for stream in listener.incoming().take(3) {
      let mut stream = match stream {
        Ok(stream) => stream,
        Err(_) => continue,
      };
      let mut buf = vec![0u8; 4096];
      let n = stream.read(&mut buf).unwrap_or(0);
      let request = String::from_utf8_lossy(&buf[..n]).into_owned();
      let sent_tag = request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name.trim().to_lowercase() == "if-none-match").then(|| value.trim().to_string())
      });

      let body = if sent_tag.is_some() && sent_tag.as_deref() == issued.lock().unwrap().as_deref() {
        "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
      } else {
        let tag = format!(
          "\"tag-{}\"",
          counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        *issued.lock().unwrap() = Some(tag.clone());
        let db = "[]";
        format!(
          "HTTP/1.1 200 OK\r\nETag: {tag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
          db.len(),
          db
        )
      };
      let _ = stream.write_all(body.as_bytes());
    }
  });
  let url = format!("http://127.0.0.1:{port}/db");
  let no_hash = None;

  // No tag yet: full fetch, empty DB, tag captured.
  let (tag, empty) = match fetch_detectable_etag(&url, None, no_hash).unwrap() {
    crate::server::process::FetchOutcome::Updated {
      etag, detectable, ..
    } => (etag, detectable),
    crate::server::process::FetchOutcome::Unchanged => panic!("first fetch must download"),
    crate::server::process::FetchOutcome::SameContent { .. } => {
      panic!("nothing known yet, cannot be same-content")
    }
  };
  assert!(empty.is_empty());
  let tag = tag.expect("stub always tags 200s");

  // Same tag: 304, nothing downloaded or parsed.
  assert!(matches!(
    fetch_detectable_etag(&url, Some(&tag), no_hash),
    Ok(crate::server::process::FetchOutcome::Unchanged)
  ));

  // Rotated tag, identical bytes: no rebuild (the flap case).
  let known = crate::server::process::body_hash("[]");
  assert!(matches!(
    fetch_detectable_etag(&url, Some("\"stale\""), Some(known)),
    Ok(crate::server::process::FetchOutcome::SameContent { .. })
  ));
}

#[test]
fn apply_ignore_list_drops_only_ignored_ids() {
  use std::sync::Arc;

  use crate::detection::DetectableActivity;
  use crate::server::process::apply_ignore_list;

  fn activity(id: &str) -> Arc<DetectableActivity> {
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
      name: format!("Game {id}"),
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
    })
  }

  let detected = vec![activity("1"), activity("2"), activity("3")];
  // Empty list: everything passes, order preserved.
  let kept = apply_ignore_list(detected.clone(), &[]);
  assert_eq!(kept.len(), 3);
  // Ignored ids drop; the rest keep order (first-element semantics kept).
  let kept = apply_ignore_list(detected, &["2".to_string()]);
  let ids: Vec<_> = kept.iter().map(|game| game.id.clone()).collect();
  assert_eq!(ids, vec!["1".to_string(), "3".to_string()]);
}

#[test]
fn app_id_from_args_parses_steam_launcher_token() {
  use crate::server::process::app_id_from_args;

  // Real reaper shape: token stands alone, digits follow.
  assert_eq!(
    app_id_from_args(Some("SteamLaunch AppId=4508340 -- /games/nte")),
    Some("4508340".to_string())
  );
  // No token, empty input, token without digits: nothing.
  assert_eq!(app_id_from_args(Some("htgame.exe /Game/Map")), None);
  assert_eq!(app_id_from_args(None), None);
  assert_eq!(app_id_from_args(Some("AppId= -- flag")), None);
  // Suffix of a longer key is not a token (`SomeAppId=`).
  assert_eq!(app_id_from_args(Some("SomeAppId=123")), None);
  // First boundary-valid token with digits wins.
  assert_eq!(
    app_id_from_args(Some("SomeAppId=1 AppId=22")),
    Some("22".to_string())
  );
}

// --- Proton (`win32`) fallback automaton (F1.1) ---

/// Synthetic DB entry with one executable (or a Steam-only entry when
/// `exe` is `None`, like How to Fish).
fn proton_entry(
  id: &str,
  name: &str,
  exe: Option<(&str, &str, bool)>,
  steam_id: Option<&str>,
) -> Arc<DetectableActivity> {
  use crate::detection::Executable;

  let mut entry = activity(id, name, steam_id);
  let entry_mut = Arc::get_mut(&mut entry).expect("fresh Arc");
  entry_mut.executables = exe.map(|(exe_name, os, is_launcher)| {
    vec![Executable {
      name: exe_name.to_string(),
      is_launcher,
      os: os.to_string(),
      arguments: None,
    }]
  });
  entry
}

fn proton_server(db: Vec<Arc<DetectableActivity>>) -> crate::server::process::ProcessServer {
  use crate::server::process::{ProcessEventListeners, ProcessServer};

  let (_tx, _rx) = std::sync::mpsc::channel();
  ProcessServer::new(
    db,
    _tx,
    ProcessEventListeners::default(),
    None,
    false,
    None,
    Vec::new(),
    None,
  )
}

#[test]
fn proton_automaton_holds_win32_only() {
  let db = vec![
    proton_entry(
      "111",
      "Native Game",
      Some(("native/game", "linux", false)),
      None,
    ),
    proton_entry(
      "222",
      "Proton Game",
      Some(("quest\\game.exe", "win32", false)),
      None,
    ),
    proton_entry(
      "333",
      "Proton Launcher",
      Some(("launcher.exe", "win32", true)),
      None,
    ),
  ];
  let server = proton_server(db.clone());
  let reversed = |path: &str| path.chars().rev().collect::<String>();

  // win32 entry hits (backslash in the DB name is normalized).
  assert_eq!(
    server
      .proton_probe(&reversed("/games/quest/game.exe"), &db)
      .map(|(obj, _)| obj.id.clone()),
    Some("222".to_string())
  );
  // Native entries stay out of the fallback automaton...
  assert!(
    server
      .proton_probe(&reversed("/native/game"), &db)
      .is_none()
  );
  // ...as do launchers.
  assert!(
    server
      .proton_probe(&reversed("/launcher.exe"), &db)
      .is_none()
  );
  // Native automaton is untouched by win32 entries.
  assert!(
    server
      .ac_probe(&reversed("/games/quest/game.exe"), &db)
      .is_none()
  );
  assert_eq!(
    server
      .ac_probe(&reversed("/native/game"), &db)
      .map(|(obj, _)| obj.id.clone()),
    Some("111".to_string())
  );
}

#[test]
fn match_process_detects_win32_game_and_prefers_steam_id() {
  use crate::server::process::Exec;

  let db = vec![
    proton_entry(
      "222",
      "Proton Quest",
      Some(("quest/game.exe", "win32", false)),
      None,
    ),
    proton_entry("999", "Steam Other", None, Some("999999")),
  ];
  let server = proton_server(db.clone());
  let mut variant_bufs: [String; 5] = Default::default();
  let mut reversed_path = String::with_capacity(256);
  let mut obs_open = false;
  // Impossible pid: every lazy /proc read (environ, cwd, stat) fails
  // deterministically, so only the pure matching chain is exercised.
  let classify = |server: &crate::server::process::ProcessServer,
                  arguments: Option<String>,
                  variant_bufs: &mut [String; 5],
                  reversed_path: &mut String,
                  obs_open: &mut bool| {
    server.match_process(
      &Exec {
        pid: u64::MAX,
        path: "C:\\games\\quest\\game.exe".to_string(),
        arguments,
      },
      &db,
      variant_bufs,
      reversed_path,
      obs_open,
    )
  };

  // No AppId anywhere: the Proton fallback catches the win32 path.
  let hit = classify(
    &server,
    None,
    &mut variant_bufs,
    &mut reversed_path,
    &mut obs_open,
  );
  let hit = hit.expect("win32 path must match via Proton automaton");
  assert_eq!(hit.id, "222");
  assert_eq!(hit.pid, Some(u64::MAX));

  // Same process, but the command line carries another game's Steam AppId:
  // the authoritative store id wins over the fuzzy path match.
  let hit = classify(
    &server,
    Some("reaper SteamLaunch AppId=999999 -- /games/other".to_string()),
    &mut variant_bufs,
    &mut reversed_path,
    &mut obs_open,
  );
  let hit = hit.expect("steam AppId must match");
  assert_eq!(hit.id, "999");
}

// --- Aliases + exclusions (F1.2) ---

#[test]
fn trim_keeps_aliases_drops_unknown() {
  use crate::detection::trim_detectable_value;

  let body = r#"[{
    "id": "1", "name": "PUBG: Battlegrounds", "hook": true,
    "aliases": ["PUBG", "PlayerUnknown's Battlegrounds", "", 42],
    "executables": [{"name": "tslgame.exe", "is_launcher": false, "os": "win32"}],
    "third_party_skus": [{"distributor": "steam", "id": "578080"}],
    "themes": ["shooter"], "overlay": true
  }]"#;
  let trimmed = trim_detectable_value(body).unwrap();
  let entry = &trimmed.as_array().unwrap()[0];
  // Aliases survive the trim (strings only, blanks/non-strings dropped)...
  assert_eq!(
    entry.get("aliases").unwrap().as_array().unwrap(),
    &vec![
      serde_json::Value::String("PUBG".to_string()),
      serde_json::Value::String("PlayerUnknown's Battlegrounds".to_string())
    ]
  );
  // ...unknown top-level fields do not.
  assert!(entry.get("themes").is_none());
  assert!(entry.get("overlay").is_none());
  assert_eq!(entry.get("id").unwrap(), "1");
}

#[test]
fn aliases_indexed_and_gated() {
  // Multi-word alias indexed; single-word alias rejected by the same
  // conservative gate as canonical names; canonical names win ties.
  let mut pubg = activity("1", "PUBG: Battlegrounds", None);
  Arc::get_mut(&mut pubg).expect("fresh Arc").aliases = Some(vec![
    "PUBG".to_string(),
    "PlayerUnknown's Battlegrounds".to_string(),
  ]);
  let mut clash = activity("2", "PlayerUnknown's Battlegrounds", None);
  Arc::get_mut(&mut clash).expect("fresh Arc").aliases = Some(vec!["Some Other Title".to_string()]);
  let db = vec![pubg, clash];
  let (steam_map, name_map) = build_aux_maps(&db);
  assert_eq!(name_map.get("playerunknown's battlegrounds"), Some(&0));
  assert!(!name_map.contains_key("pubg"));
  assert_eq!(name_map.get("some other title"), Some(&1));
  let _ = steam_map;

  // The alias resolves through the real stem fallback.
  let hit = match_name_or_folder(
    "/games/playerunknown's battlegrounds/tslgame.exe",
    11,
    &Mutex::new(name_map),
    &db,
  );
  assert_eq!(hit.unwrap().id, "1");
}

#[test]
fn exclusions_parse_and_match() {
  use crate::detection::{Exclusions, parse_exclusions};

  // Shape mirrors the live endpoint (subset): exact names, one regex,
  // plus garbage a tolerant parser must swallow.
  let body = r#"{
    "executables": ["crashreportclient.exe", "  ", 42, "UnityCrashHandler64.exe"],
    "patterns": ["vcredist.*\\.exe$", "([invalid", 7],
    "unexpected": [1, 2]
  }"#;
  let exclusions = parse_exclusions(body);
  assert_eq!(
    exclusions.executables,
    vec!["crashreportclient.exe", "unitycrashhandler64.exe"]
  );
  assert_eq!(exclusions.patterns.len(), 1);

  // Exact (basenames arrive lowercased from the scanner)...
  assert!(exclusions.is_excluded("crashreportclient.exe"));
  assert!(exclusions.is_excluded("unitycrashhandler64.exe"));
  // ...regex, case-insensitively (Windows-centric DB, Proton paths)...
  assert!(exclusions.is_excluded("vcredist_x64.exe"));
  assert!(exclusions.is_excluded("VCREDIST_X86.EXE"));
  // ...and real games pass through.
  assert!(!exclusions.is_excluded("htgame.exe"));
  assert!(!exclusions.is_excluded("client-win64-shipping.exe"));

  // Wholly invalid body degrades to empty, never errors.
  let empty = parse_exclusions("not json{{{");
  assert!(!empty.is_excluded("crashreportclient.exe"));
  let _ = Exclusions::default();
}

#[test]
fn match_process_honors_exclusions() {
  use crate::detection::parse_exclusions;
  use crate::server::process::Exec;

  let db = vec![proton_entry(
    "222",
    "Proton Quest",
    Some(("quest/game.exe", "win32", false)),
    None,
  )];
  let server = proton_server(db.clone());
  server.set_exclusions(parse_exclusions(
    r#"{"executables": ["game.exe"], "patterns": []}"#,
  ));
  let mut variant_bufs: [String; 5] = Default::default();
  let mut reversed_path = String::with_capacity(256);
  let mut obs_open = false;

  // Would match via the Proton automaton — excluded first.
  let miss = server.match_process(
    &Exec {
      pid: u64::MAX,
      path: "C:\\games\\quest\\game.exe".to_string(),
      arguments: None,
    },
    &db,
    &mut variant_bufs,
    &mut reversed_path,
    &mut obs_open,
  );
  assert!(miss.is_none());

  // Same game under a non-excluded exe name still detects (fresh entry:
  // cloning the Arc would share ownership and block mutation).
  let db2 = vec![proton_entry(
    "222",
    "Proton Quest",
    Some(("quest/launcher-free.exe", "win32", false)),
    None,
  )];
  let server2 = proton_server(db2.clone());
  server2.set_exclusions(parse_exclusions(
    r#"{"executables": ["game.exe"], "patterns": []}"#,
  ));
  let hit = server2
    .match_process(
      &Exec {
        pid: u64::MAX,
        path: "/games/quest/launcher-free.exe".to_string(),
        arguments: None,
      },
      &db2,
      &mut variant_bufs,
      &mut reversed_path,
      &mut obs_open,
    )
    .expect("non-excluded path must match");
  assert_eq!(hit.id, "222");
}
