//! Headless Bevy test harness: app builder, input injection, screenshots, bots.
#![deny(unsafe_code)]

pub mod app;
pub mod bots;
pub mod e2e;
pub mod input;
pub mod player;
pub mod screenshot;

pub use app::{
    FRAME_HZ, RenderFailure, TestApp, TestCamera, TestConfig, TestRenderTarget, frame_duration,
};
pub use player::PlayerSnapshot;
pub use screenshot::{MAX_SCREENSHOT_STEPS, ScreenshotError};
