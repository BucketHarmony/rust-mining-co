//! Helpers for the end-to-end scenarios (SAD §12.4).

use std::path::{Path, PathBuf};

use image::RgbaImage;

/// Where scenarios save their screenshots: `$RMC_E2E_OUT` (set by
/// `scripts/e2e.sh`), else `target/e2e` in the workspace.
pub fn out_dir() -> PathBuf {
    std::env::var_os("RMC_E2E_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e"))
}

/// Saves `image` as `<out_dir>/<name>-<step>.png` and returns the path.
///
/// Panics if the file cannot be written.
pub fn save_screenshot(image: &RgbaImage, name: &str, step: u32) -> PathBuf {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let path = dir.join(format!("{name}-{step}.png"));
    image
        .save(&path)
        .unwrap_or_else(|e| panic!("save {}: {e}", path.display()));
    path
}
