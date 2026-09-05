//! Fetches Discord's detectable games database and writes a trimmed offline
//! snapshot for rsrpc.
//!
//! Usage (from the workspace root):
//!
//! ```bash
//! cargo run --manifest-path tools/updater/Cargo.toml
//! ```
//!
//! This writes `lib/resources/detectable.json`, which is embedded into the
//! library at build time via `include_str!` (see `detection::BUNDLED_DETECTABLE`).

use std::path::PathBuf;

const DETECTABLE_URL: &str = "https://discord.com/api/v9/applications/detectable";

fn output_path() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("..")
    .join("..")
    .join("lib")
    .join("resources")
    .join("detectable.json")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  println!("Fetching detectable.json from {DETECTABLE_URL}...");

  let body = ureq::get(DETECTABLE_URL)
    .call()?
    .into_body()
    .with_config()
    .limit(64 * 1024 * 1024)
    .read_to_string()?;

  let games: Vec<serde_json::Value> = serde_json::from_str(&body)?;
  println!(
    "Loaded {} games, trimming to the fields rsrpc uses...",
    games.len()
  );

  let trimmed: Vec<serde_json::Value> = games
    .into_iter()
    .map(|game| {
      let mut entry = serde_json::Map::new();
      entry.insert(
        "id".to_string(),
        game.get("id").cloned().unwrap_or_else(|| "".into()),
      );
      entry.insert(
        "name".to_string(),
        game.get("name").cloned().unwrap_or_else(|| "".into()),
      );
      entry.insert(
        "hook".to_string(),
        game.get("hook").cloned().unwrap_or_else(|| false.into()),
      );

      let executables: Vec<serde_json::Value> = game
        .get("executables")
        .and_then(|value| value.as_array())
        .map(|executables| {
          executables
            .iter()
            .map(|exe| {
              let mut trimmed_exe = serde_json::Map::new();
              trimmed_exe.insert(
                "name".to_string(),
                exe.get("name").cloned().unwrap_or_else(|| "".into()),
              );
              trimmed_exe.insert(
                "is_launcher".to_string(),
                exe
                  .get("is_launcher")
                  .cloned()
                  .unwrap_or_else(|| false.into()),
              );
              trimmed_exe.insert(
                "os".to_string(),
                exe.get("os").cloned().unwrap_or_else(|| "".into()),
              );
              if let Some(args) = exe.get("arguments")
                && !args.as_str().unwrap_or_default().is_empty()
              {
                trimmed_exe.insert("arguments".to_string(), args.clone());
              }
              serde_json::Value::Object(trimmed_exe)
            })
            .collect()
        })
        .unwrap_or_default();
      entry.insert(
        "executables".to_string(),
        serde_json::Value::Array(executables),
      );

      // Keep third-party store ids (Steam AppId etc) for automatic matching
      // of games that ship empty `executables` (e.g. How to Fish).
      let skus: Vec<serde_json::Value> = game
        .get("third_party_skus")
        .and_then(|v| v.as_array())
        .map(|skus| {
          skus
            .iter()
            .filter_map(|sku| {
              let distributor = sku.get("distributor")?.as_str()?;
              let id = sku.get("id").map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
              })?;
              if distributor.is_empty() || id.is_empty() {
                return None;
              }
              let mut m = serde_json::Map::new();
              m.insert("distributor".to_string(), distributor.into());
              m.insert("id".to_string(), id.into());
              Some(serde_json::Value::Object(m))
            })
            .collect()
        })
        .unwrap_or_default();
      entry.insert(
        "third_party_skus".to_string(),
        serde_json::Value::Array(skus),
      );

      serde_json::Value::Object(entry)
    })
    .collect();

  let output = serde_json::to_string(&trimmed)?;
  let path = output_path();
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(&path, &output)?;
  println!("Wrote {} bytes to {}", output.len(), path.display());

  Ok(())
}
