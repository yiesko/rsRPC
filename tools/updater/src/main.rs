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

  // Single source of truth: the same trim the scanner, the CLI fallback
  // and the hourly refresh use (see `rsrpc::detection::trim_detectable`).
  // A hand-rolled copy here drifted before (aliases were silently dropped
  // from the bundled snapshot); never duplicate it again.
  let output = rsrpc::detection::trim_detectable(&body)?;
  let path = output_path();
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(&path, &output)?;
  println!("Wrote {} bytes to {}", output.len(), path.display());

  Ok(())
}
