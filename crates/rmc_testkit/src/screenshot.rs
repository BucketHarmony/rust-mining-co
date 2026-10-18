//! Screenshots of the test render target (SAD §12.1).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use bevy::render::render_resource::PollType;
use bevy::render::renderer::RenderDevice;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use image::RgbaImage;
use thiserror::Error;

use crate::app::{TestApp, TestRenderTarget};

/// How many steps [`TestApp::screenshot`] waits for the capture before it
/// gives up. A capture normally arrives within a few frames.
pub const MAX_SCREENSHOT_STEPS: u32 = 60;

/// Why a screenshot could not be taken.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ScreenshotError {
    #[error("the app was built with render: false")]
    NoRenderer,
    #[error("rendering stopped: {0}")]
    RenderFailed(String),
    #[error("no capture after {0} steps")]
    Timeout(u32),
    #[error("captured image is not 8-bit RGBA: {0}")]
    Format(String),
}

impl TestApp {
    /// Captures the render target as it is after the next rendered frame.
    ///
    /// Panics if the app does not render or no capture arrives; see
    /// [`TestApp::try_screenshot`].
    pub fn screenshot(&mut self) -> RgbaImage {
        self.try_screenshot()
            .unwrap_or_else(|e| panic!("screenshot failed: {e}"))
    }

    /// Spawns `Screenshot::image(TestRenderTarget)` with an observer on
    /// `ScreenshotCaptured` that stores the image, then calls
    /// [`TestApp::step`] until it arrives (at most
    /// [`MAX_SCREENSHOT_STEPS`] times). The host ticks with every step.
    pub fn try_screenshot(&mut self) -> Result<RgbaImage, ScreenshotError> {
        let Some(TestRenderTarget(target)) = self
            .app()
            .world()
            .get_resource::<TestRenderTarget>()
            .cloned()
        else {
            return Err(ScreenshotError::NoRenderer);
        };
        let slot: Arc<Mutex<Option<Image>>> = Arc::default();
        let store = slot.clone();
        // Bevy despawns the entity itself once the observer has run.
        self.app_mut()
            .world_mut()
            .spawn(Screenshot::image(target))
            .observe(move |captured: On<ScreenshotCaptured>| {
                *store.lock().expect("screenshot slot") = Some(captured.image.clone());
            });

        for _ in 0..MAX_SCREENSHOT_STEPS {
            self.step();
            if let Some(failure) = self.render_failure() {
                return Err(ScreenshotError::RenderFailed(failure.0.clone()));
            }
            self.wait_for_gpu();
            if let Some(image) = slot.lock().expect("screenshot slot").take() {
                return to_rgba8(image);
            }
        }
        Err(ScreenshotError::Timeout(MAX_SCREENSHOT_STEPS))
    }

    /// Blocks until the GPU has finished the work submitted so far, which
    /// runs the readback's buffer-map callback. Bevy only polls the device
    /// when it submits the next frame, and on a software adapter (WARP) that
    /// poll finds the copy unfinished frame after frame.
    pub(crate) fn wait_for_gpu(&self) {
        let device = self.app().world().resource::<RenderDevice>();
        // The only error with no submission index is a timeout; the loop in
        // try_screenshot then steps again.
        if let Err(error) = device.poll(PollType::Wait {
            submission_index: None,
            timeout: Some(GPU_WAIT),
        }) {
            warn!(%error, "waiting for the GPU");
        }
    }
}

/// The longest [`TestApp::wait_for_gpu`] waits for one frame's work.
const GPU_WAIT: Duration = Duration::from_secs(10);

/// The captured image as 8-bit RGBA (the render target is
/// `Rgba8UnormSrgb`, so this is a copy of its bytes).
fn to_rgba8(image: Image) -> Result<RgbaImage, ScreenshotError> {
    match image.try_into_dynamic() {
        Ok(image::DynamicImage::ImageRgba8(rgba)) => Ok(rgba),
        Ok(other) => Err(ScreenshotError::Format(format!("{:?}", other.color()))),
        Err(e) => Err(ScreenshotError::Format(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{TestCamera, TestConfig};
    use rmc_world::coords::{CellAabb, CellPos};

    /// Clear color: mostly blue. Cube: unlit red.
    const SKY: Color = Color::srgb(0.2, 0.4, 0.8);
    const CUBE: Color = Color::srgb(1.0, 0.0, 0.0);

    fn render_config() -> TestConfig {
        TestConfig {
            seed: 3,
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31)),
            ..TestConfig::default()
        }
    }

    fn is_red(p: &image::Rgba<u8>) -> bool {
        let [r, g, b, _] = p.0;
        r > 128 && r as i32 > g as i32 + 64 && r as i32 > b as i32 + 64
    }

    fn is_blue(p: &image::Rgba<u8>) -> bool {
        let [r, g, b, _] = p.0;
        b > 96 && b as i32 > r as i32 + 40 && b as i32 > g as i32 + 10
    }

    /// Points the test camera straight up from above the world, so it sees
    /// nothing of the world but the clear color, and spawns a red cube 4 m
    /// in front of it, filling the middle of the view.
    fn spawn_cube(t: &mut TestApp) {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<TestCamera>>()
            .single_mut(world)
            .expect("test camera");
        *camera = Transform::from_xyz(8.0, 40.0, 8.0).looking_to(Vec3::Y, Vec3::Z);
        let camera = *camera;
        let mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(1.0, 1.0, 1.0));
        let material = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: CUBE,
                unlit: true,
                ..default()
            });
        world.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_translation(camera.translation + camera.forward() * 4.0)
                .with_rotation(camera.rotation),
        ));
    }

    #[test]
    fn captures_nonblack_image() {
        let mut t = TestApp::new(render_config());
        t.app_mut().insert_resource(ClearColor(SKY));
        spawn_cube(&mut t);

        let frame0 = t.app().world().resource::<bevy::diagnostic::FrameCount>().0;
        let shot = t.screenshot();
        let waited = t.app().world().resource::<bevy::diagnostic::FrameCount>().0 - frame0;
        eprintln!("capture took {waited} steps");
        assert!((1..=MAX_SCREENSHOT_STEPS).contains(&waited));
        assert_eq!(t.render_failure(), None);

        let (w, h) = t.config().size;
        assert_eq!(shot.dimensions(), (w, h), "the render target's size");
        let pixels = (w * h) as usize;
        let nonblack = shot
            .pixels()
            .filter(|p| p.0[..3].iter().any(|&c| c > 16))
            .count();
        assert!(
            nonblack * 100 >= pixels * 99,
            "only {nonblack} of {pixels} pixels are not black"
        );
        assert!(shot.pixels().all(|p| p.0[3] == 255), "opaque");

        // It is this scene: the cube in the middle, the clear color at the
        // corners.
        let center = shot.get_pixel(w / 2, h / 2);
        assert!(is_red(center), "center {center:?} is the cube");
        for (x, y) in [(2, 2), (w - 3, 2), (2, h - 3), (w - 3, h - 3)] {
            let p = shot.get_pixel(x, y);
            assert!(is_blue(p), "corner ({x}, {y}) {p:?} is the clear color");
        }
        let red = shot.pixels().filter(|p| is_red(p)).count();
        let blue = shot.pixels().filter(|p| is_blue(p)).count();
        assert!(red * 100 >= pixels, "cube covers {red} pixels");
        assert!(blue * 2 >= pixels, "clear color covers {blue} pixels");
        assert!(
            red + blue >= pixels * 98 / 100,
            "{red} + {blue} of {pixels}"
        );

        // Each call captures a new frame, not the previous image.
        t.app_mut()
            .insert_resource(ClearColor(Color::srgb(0.1, 0.8, 0.2)));
        let again = t.screenshot();
        let corner = again.get_pixel(2, 2);
        let [r, g, b, _] = corner.0;
        assert!(
            g as i32 > r as i32 + 64 && g as i32 > b as i32 + 64,
            "corner {corner:?} after the clear color turned green"
        );
        assert!(is_red(again.get_pixel(w / 2, h / 2)));
        assert_eq!(t.render_failure(), None);

        // Without a renderer there is nothing to capture.
        let mut logic = TestApp::new(TestConfig {
            render: false,
            ..render_config()
        });
        assert_eq!(logic.try_screenshot(), Err(ScreenshotError::NoRenderer));
    }
}
