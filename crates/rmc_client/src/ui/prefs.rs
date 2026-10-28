//! The player's settings on disk: the UI scale and the temperature unit.
//!
//! [`SettingsFilePlugin`] reads its file when it is added (after
//! [`UiKitPlugin`](super::UiKitPlugin)) and writes it whenever a setting
//! changes. A missing file keeps the defaults; an unreadable one is
//! logged and keeps them too, and the next change overwrites it. The game
//! keeps the file in its save directory ([`SETTINGS_FILE`]).

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::UiSystems;
use super::scale::UiSettings;
use super::theme::UiTheme;
use crate::hud::TemperatureUnit;

/// The settings file's name in the game's save directory.
pub const SETTINGS_FILE: &str = "settings.ron";

/// What the file holds. A field it lacks keeps its default.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(default)]
pub struct SavedSettings {
    pub ui_scale: Option<f32>,
    pub temperature_unit: Option<TemperatureUnit>,
}

impl SavedSettings {
    /// The settings as they are in `world`.
    fn current(world: &World) -> Self {
        Self {
            ui_scale: world.get_resource::<UiSettings>().map(|s| s.scale),
            temperature_unit: world.get_resource::<TemperatureUnit>().copied(),
        }
    }

    /// Reads `path`; `None` if it is missing or unreadable (logged).
    pub fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        match ron::from_str(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!("ignoring {}: {e}", path.display());
                None
            }
        }
    }

    /// Writes `path` through a temporary file, so a crash mid-write
    /// leaves the old file.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("ron.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

/// Where the settings live and what was last read or written there.
#[derive(Resource, Clone, Debug)]
pub struct SettingsFile {
    pub path: PathBuf,
    written: SavedSettings,
}

/// Loads the settings from `path` and saves them there when they change.
pub struct SettingsFilePlugin {
    pub path: PathBuf,
}

impl Plugin for SettingsFilePlugin {
    fn build(&self, app: &mut App) {
        let world = app.world_mut();
        if let Some(saved) = SavedSettings::read(&self.path) {
            let range = world.get_resource::<UiTheme>().map(|t| t.scale);
            if let (Some(scale), Some(mut settings)) =
                (saved.ui_scale, world.get_resource_mut::<UiSettings>())
            {
                settings.scale = range.map_or(scale, |r| scale.clamp(r.min, r.max));
            }
            if let Some(unit) = saved.temperature_unit {
                world.insert_resource(unit);
            }
            info!(path = %self.path.display(), "settings loaded");
        }
        let written = SavedSettings::current(world);
        world.insert_resource(SettingsFile {
            path: self.path.clone(),
            written,
        });
        app.add_systems(Update, save_settings.after(UiSystems));
    }
}

fn save_settings(world: &mut World) {
    let now = SavedSettings::current(world);
    let Some(mut file) = world.get_resource_mut::<SettingsFile>() else {
        return;
    };
    if file.written == now {
        return;
    }
    file.written = now;
    match now.write(&file.path) {
        Ok(()) => debug!(path = %file.path.display(), "settings saved"),
        Err(e) => warn!("could not save settings to {}: {e}", file.path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests::THEME_PATH;
    use crate::ui::{UiAction, UiKitPlugin};
    use bevy::ui_widgets::Activate;

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("rmc_client_prefs_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir.join(SETTINGS_FILE))
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn app(path: &Path) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(UiKitPlugin {
                theme: UiTheme::load(THEME_PATH).unwrap(),
                assets_dir: None,
            })
            .add_plugins(SettingsFilePlugin {
                path: path.to_path_buf(),
            });
        app.update();
        app
    }

    fn activate(app: &mut App, action: UiAction) {
        let mut q = app.world_mut().query::<(Entity, &UiAction)>();
        let entity = q
            .iter(app.world())
            .find(|(_, a)| **a == action)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no {action:?} button"));
        app.world_mut().trigger(Activate { entity });
        app.update();
    }

    #[test]
    fn settings_are_saved_on_change_and_loaded_on_start() {
        let file = TempFile::new("roundtrip");
        let mut a = app(&file.0);
        assert!(!file.0.exists(), "nothing changed, nothing written");
        assert_eq!(
            *a.world().resource::<TemperatureUnit>(),
            TemperatureUnit::Fahrenheit
        );
        let default_scale = a.world().resource::<UiSettings>().scale;

        a.world_mut()
            .resource_mut::<crate::ui::UiScreens>()
            .push(crate::ui::Screen::Settings);
        a.update();
        activate(&mut a, UiAction::ToggleTemperatureUnit);
        activate(&mut a, UiAction::ScaleUp);
        let scale = a.world().resource::<UiSettings>().scale;
        assert!(scale > default_scale);
        let saved = SavedSettings::read(&file.0).expect("written");
        assert_eq!(
            saved,
            SavedSettings {
                ui_scale: Some(scale),
                temperature_unit: Some(TemperatureUnit::Celsius),
            }
        );
        drop(a);

        let b = app(&file.0);
        assert_eq!(
            *b.world().resource::<TemperatureUnit>(),
            TemperatureUnit::Celsius
        );
        assert_eq!(b.world().resource::<UiSettings>().scale, scale);
    }

    #[test]
    fn a_bad_or_partial_file_keeps_the_defaults() {
        let file = TempFile::new("bad");
        std::fs::create_dir_all(file.0.parent().unwrap()).unwrap();
        std::fs::write(&file.0, "not ron (").unwrap();
        let a = app(&file.0);
        assert_eq!(
            *a.world().resource::<TemperatureUnit>(),
            TemperatureUnit::Fahrenheit
        );
        drop(a);

        // A file with only the unit, and a scale out of range, clamped.
        std::fs::write(&file.0, "(temperature_unit: Some(Celsius))").unwrap();
        let a = app(&file.0);
        assert_eq!(
            *a.world().resource::<TemperatureUnit>(),
            TemperatureUnit::Celsius
        );
        drop(a);
        std::fs::write(&file.0, "(ui_scale: Some(50.0))").unwrap();
        let a = app(&file.0);
        let theme = UiTheme::load(THEME_PATH).unwrap();
        assert_eq!(a.world().resource::<UiSettings>().scale, theme.scale.max);
    }
}
