//! Resolution-aware scaling.
//!
//! Every size in [`UiTheme`] is designed at its `reference_height`. Each
//! frame [`UiScale`] becomes the viewport's logical height over that
//! reference, times the player's [`UiSettings::scale`], clamped to the
//! theme's `clamp` ([`effective_scale`]). So a 1440p window draws the UI
//! a third larger than 1080p, and a player who wants larger text sets the
//! scale and every widget follows: no widget scales itself.
//!
//! The viewport is the [`GameCamera`]'s, so the test kit's image target
//! scales like a window does.

use bevy::prelude::*;

use super::theme::UiTheme;
use crate::render::GameCamera;

/// The player's UI preferences.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct UiSettings {
    /// Multiplies the resolution scale; the theme's `scale` range bounds it.
    pub scale: f32,
}

impl UiSettings {
    pub fn new(theme: &UiTheme) -> Self {
        Self {
            scale: theme.scale.default,
        }
    }

    /// Steps the scale by `steps` of the theme's step, within its range,
    /// rounded to the step so repeated presses do not drift.
    pub fn step_scale(&mut self, theme: &UiTheme, steps: i32) {
        let s = theme.scale;
        let n = (self.scale / s.step).round() + steps as f32;
        self.scale = (n * s.step).clamp(s.min, s.max);
    }
}

/// The [`UiScale`] for a viewport `height` logical px tall.
pub fn effective_scale(theme: &UiTheme, height: f32, settings: &UiSettings) -> f32 {
    let resolution = if height > 0.0 {
        height / theme.reference_height
    } else {
        1.0
    };
    (resolution * settings.scale).clamp(theme.clamp.0, theme.clamp.1)
}

pub(crate) fn update_scale(
    theme: Res<UiTheme>,
    settings: Res<UiSettings>,
    cameras: Query<&Camera, With<GameCamera>>,
    mut scale: ResMut<UiScale>,
) {
    let height = cameras
        .iter()
        .find_map(Camera::logical_viewport_size)
        .map_or(theme.reference_height, |s| s.y);
    let want = effective_scale(&theme, height, &settings);
    if (scale.0 - want).abs() > f32::EPSILON {
        scale.0 = want;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests::THEME_PATH;

    #[test]
    fn scale_follows_height_and_setting_within_clamp() {
        let t = UiTheme::load(THEME_PATH).unwrap();
        let mut s = UiSettings::new(&t);
        let at = |h, s: &UiSettings| effective_scale(&t, h, s);
        assert!((at(t.reference_height, &s) - s.scale).abs() < 1e-6);
        assert!((at(t.reference_height * 2.0, &s) - 2.0 * s.scale).abs() < 1e-5);
        assert_eq!(at(1.0, &s), t.clamp.0, "a tiny viewport clamps");
        assert_eq!(at(0.0, &s), s.scale.clamp(t.clamp.0, t.clamp.1));

        let before = s.scale;
        s.step_scale(&t, 1);
        assert!((s.scale - before - t.scale.step).abs() < 1e-5);
        assert!(at(t.reference_height, &s) > before);
        s.step_scale(&t, 1000);
        assert_eq!(s.scale, t.scale.max);
        s.step_scale(&t, -1000);
        assert_eq!(s.scale, t.scale.min);
    }
}
