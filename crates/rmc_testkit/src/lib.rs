//! Headless Bevy test harness: app builder, input injection, screenshots, bots.
#![deny(unsafe_code)]

pub mod app;
pub mod bots;
pub mod e2e;
pub mod eco;
pub mod input;
pub mod player;
pub mod screenshot;

pub use app::{
    FRAME_HZ, MAX_SAVE_STEPS, RenderFailure, TestApp, TestCamera, TestConfig, TestRenderTarget,
    frame_duration,
};
pub use eco::{
    Fixture, FixtureSite, MAX_EDIT_STEPS, MAX_SEASON_STEPS, SeasonFrames, SeasonOverride, ToxKind,
};
pub use player::PlayerSnapshot;
pub use rmc_world::query::ToxProfile;
pub use screenshot::{MAX_SCREENSHOT_STEPS, ScreenshotError};
