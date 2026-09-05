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
