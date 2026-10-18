//! Headless Bevy test harness: app builder, input injection, screenshots, bots.
#![deny(unsafe_code)]

pub mod app;

pub use app::{
    FRAME_HZ, RenderFailure, TestApp, TestCamera, TestConfig, TestRenderTarget, frame_duration,
};
