//! Helpers for the end-to-end scenarios (SAD §12.4).

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};

use image::{DynamicImage, RgbaImage};
use image_compare::Algorithm;

use crate::app::TestApp;

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

/// How far apart two screenshots of the same size are: the root mean
/// square difference of the worst RGB channel, normalized to 0..=1
/// (image-compare's RMS, SAD §12.4). Alpha is ignored. 0 is identical.
///
/// Panics if the sizes differ.
pub fn rms(a: &RgbaImage, b: &RgbaImage) -> f64 {
    let rgb = |i: &RgbaImage| DynamicImage::ImageRgba8(i.clone()).to_rgb8();
    let similarity =
        image_compare::rgb_similarity_structure(&Algorithm::RootMeanSquared, &rgb(a), &rgb(b))
            .unwrap_or_else(|e| panic!("comparing screenshots: {e}"));
    1.0 - similarity.score
}

/// Runs a scenario's `body` on `t`. If it panics, saves what the camera
/// sees then as `<out_dir>/<name>-fail-<step>.png` (when the app still
/// renders) and re-raises the panic, so a failing scenario leaves a
/// screenshot as well as a passing one (SAD §12.4).
pub fn run_scenario(t: &mut TestApp, name: &str, body: impl FnOnce(&mut TestApp)) {
    let Err(panic) = catch_unwind(AssertUnwindSafe(|| body(&mut *t))) else {
        return;
    };
    let saved = catch_unwind(AssertUnwindSafe(|| {
        let shot = t.try_screenshot().ok()?;
        Some(save_screenshot(&shot, &format!("{name}-fail"), t.frame()))
    }));
    match saved {
        Ok(Some(path)) => eprintln!("failure screenshot: {}", path.display()),
        _ => eprintln!("{name}: no failure screenshot"),
    }
    resume_unwind(panic);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TestConfig;
    use rmc_world::coords::{CellAabb, CellPos};

    fn failure_shots(name: &str) -> Vec<PathBuf> {
        let prefix = format!("{name}-fail-");
        std::fs::read_dir(out_dir())
            .map(|dir| {
                dir.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name()
                            .and_then(|f| f.to_str())
                            .is_some_and(|f| f.starts_with(&prefix) && f.ends_with(".png"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn rms_measures_difference() {
        let black = RgbaImage::from_pixel(20, 10, image::Rgba([0, 0, 0, 255]));
        assert_eq!(rms(&black, &black), 0.0);
        let mut white = black.clone();
        for p in white.pixels_mut() {
            *p = image::Rgba([255, 255, 255, 0]);
        }
        assert!((rms(&black, &white) - 1.0).abs() < 1e-9, "alpha ignored");

        // A tenth of the pixels fully off in red only: sqrt(0.1).
        let mut part = black.clone();
        for x in 0..2 {
            for y in 0..10 {
                part.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        assert!((rms(&black, &part) - 0.1f64.sqrt()).abs() < 1e-6);
        assert_eq!(rms(&black, &part), rms(&part, &black));
        // Every pixel off by 51 (a fifth) in every channel: 0.2.
        let gray = RgbaImage::from_pixel(20, 10, image::Rgba([51, 51, 51, 255]));
        assert!((rms(&black, &gray) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn run_scenario_saves_screenshot_on_failure() {
        let name = "testkit_run_scenario";
        for old in failure_shots(name) {
            std::fs::remove_file(old).expect("remove an old failure screenshot");
        }
        let mut t = TestApp::new(TestConfig {
            seed: 3,
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31)),
            ..TestConfig::default()
        });

        // A passing body runs once on the app and saves nothing.
        let mut ran = 0;
        run_scenario(&mut t, name, |t| {
            t.step();
            ran += 1;
        });
        assert_eq!(ran, 1);
        assert!(failure_shots(name).is_empty());

        // A failing body's panic comes back out, after a screenshot.
        let result = catch_unwind(AssertUnwindSafe(|| {
            run_scenario(&mut t, name, |_| panic!("scenario failed"));
        }));
        let panic = result.expect_err("the panic is re-raised");
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"scenario failed"));
        let shots = failure_shots(name);
        assert_eq!(shots.len(), 1, "one failure screenshot: {shots:?}");
        let image = image::open(&shots[0]).expect("a readable PNG");
        assert_eq!((image.width(), image.height()), t.config().size);
        std::fs::remove_file(&shots[0]).expect("remove the test's screenshot");
    }
}
