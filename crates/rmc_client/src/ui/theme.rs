//! Design tokens (`assets/ui.ron`) and the fonts they name.
//!
//! [`UiTheme`] is the one place the UI's colors, type ramp, spacing,
//! radii, focus ring and motion live. Widgets read tokens by name
//! ([`TextRole`], [`ColorToken`]); nothing in the kit spells a color or a
//! size. Colors are sRGB hex strings, parsed once at load ([`parse_hex`]),
//! so a bad color fails the load instead of drawing magenta.
//!
//! [`contrast_ratio`] is WCAG 2's luminance ratio, which XAG 102 uses; the
//! shipped theme's text pairs are tested to keep 4.5:1.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::Deserialize;

/// Why [`UiTheme::load`] failed.
#[derive(Debug, thiserror::Error)]
pub enum UiThemeError {
    #[error("reading {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("parsing {0}: {1}")]
    Parse(PathBuf, Box<ron::error::SpannedError>),
    #[error("{0}: color {1}: {2}")]
    Color(PathBuf, &'static str, String),
}

/// The player's UI scale setting: default, range and step.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct ScaleRange {
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub step: f32,
}

/// Font files, relative to the assets directory.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct FontPaths {
    pub regular: String,
    pub bold: String,
}

/// The type ramp, logical px at the reference height.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct TypeRamp {
    pub caption: f32,
    pub body: f32,
    pub label: f32,
    pub title: f32,
    pub display: f32,
}

/// The spacing grid, logical px.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Spacing {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Radii {
    pub sm: f32,
    pub md: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct FocusRing {
    pub width: f32,
    pub offset: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct ButtonSize {
    pub width: f32,
    pub height: f32,
}

/// Durations, seconds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Motion {
    pub fast: f32,
    pub normal: f32,
    pub toast: f32,
}

/// The palette as written in `ui.ron`.
#[derive(Clone, Debug, Deserialize)]
struct HexPalette {
    text: String,
    text_dim: String,
    text_on_accent: String,
    surface: String,
    surface_raised: String,
    surface_hover: String,
    scrim: String,
    border: String,
    accent: String,
    accent_hover: String,
    accent_pressed: String,
    focus: String,
    info: String,
    ok: String,
    warning: String,
    danger: String,
    shadow: String,
}

/// The palette, parsed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub text: Color,
    pub text_dim: Color,
    pub text_on_accent: Color,
    pub surface: Color,
    pub surface_raised: Color,
    pub surface_hover: Color,
    pub scrim: Color,
    pub border: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_pressed: Color,
    pub focus: Color,
    pub info: Color,
    pub ok: Color,
    pub warning: Color,
    pub danger: Color,
    pub shadow: Color,
}

/// A palette entry, for widgets that take a color by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorToken {
    Text,
    TextDim,
    TextOnAccent,
    Accent,
    Info,
    Ok,
    Warning,
    Danger,
}

impl Palette {
    pub fn get(&self, token: ColorToken) -> Color {
        match token {
            ColorToken::Text => self.text,
            ColorToken::TextDim => self.text_dim,
            ColorToken::TextOnAccent => self.text_on_accent,
            ColorToken::Accent => self.accent,
            ColorToken::Info => self.info,
            ColorToken::Ok => self.ok,
            ColorToken::Warning => self.warning,
            ColorToken::Danger => self.danger,
        }
    }
}

/// A step of the type ramp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextRole {
    Caption,
    Body,
    Label,
    Title,
    Display,
}

impl TextRole {
    /// Titles and labels are bold.
    pub fn bold(self) -> bool {
        matches!(self, Self::Label | Self::Title | Self::Display)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ThemeFile {
    reference_height: f32,
    scale: ScaleRange,
    clamp: (f32, f32),
    safe_area: f32,
    fonts: FontPaths,
    text: TypeRamp,
    space: Spacing,
    radius: Radii,
    border: f32,
    focus_ring: FocusRing,
    button: ButtonSize,
    panel_width: f32,
    colors: HexPalette,
    motion: Motion,
    toast_max: usize,
}

/// The design tokens. See the module docs.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct UiTheme {
    /// The window height (logical px) the sizes are designed for.
    pub reference_height: f32,
    pub scale: ScaleRange,
    /// Bounds of the effective [`UiScale`].
    pub clamp: (f32, f32),
    /// Kept clear at each edge, as a fraction of the shorter side.
    pub safe_area: f32,
    pub fonts: FontPaths,
    pub text: TypeRamp,
    pub space: Spacing,
    pub radius: Radii,
    pub border: f32,
    pub focus_ring: FocusRing,
    pub button: ButtonSize,
    pub panel_width: f32,
    pub colors: Palette,
    pub motion: Motion,
    pub toast_max: usize,
}

impl UiTheme {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, UiThemeError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| UiThemeError::Io(path.into(), e))?;
        Self::parse(&text).map_err(|e| match e {
            ParseError::Ron(e) => UiThemeError::Parse(path.into(), e),
            ParseError::Color(name, value) => UiThemeError::Color(path.into(), name, value),
        })
    }

    fn parse(text: &str) -> Result<Self, ParseError> {
        let f: ThemeFile = ron::from_str(text).map_err(|e| ParseError::Ron(Box::new(e)))?;
        let c = &f.colors;
        let hex = |name: &'static str, v: &str| {
            parse_hex(v).ok_or_else(|| ParseError::Color(name, v.to_owned()))
        };
        let colors = Palette {
            text: hex("text", &c.text)?,
            text_dim: hex("text_dim", &c.text_dim)?,
            text_on_accent: hex("text_on_accent", &c.text_on_accent)?,
            surface: hex("surface", &c.surface)?,
            surface_raised: hex("surface_raised", &c.surface_raised)?,
            surface_hover: hex("surface_hover", &c.surface_hover)?,
            scrim: hex("scrim", &c.scrim)?,
            border: hex("border", &c.border)?,
            accent: hex("accent", &c.accent)?,
            accent_hover: hex("accent_hover", &c.accent_hover)?,
            accent_pressed: hex("accent_pressed", &c.accent_pressed)?,
            focus: hex("focus", &c.focus)?,
            info: hex("info", &c.info)?,
            ok: hex("ok", &c.ok)?,
            warning: hex("warning", &c.warning)?,
            danger: hex("danger", &c.danger)?,
            shadow: hex("shadow", &c.shadow)?,
        };
        Ok(Self {
            reference_height: f.reference_height,
            scale: f.scale,
            clamp: f.clamp,
            safe_area: f.safe_area,
            fonts: f.fonts,
            text: f.text,
            space: f.space,
            radius: f.radius,
            border: f.border,
            focus_ring: f.focus_ring,
            button: f.button,
            panel_width: f.panel_width,
            colors,
            motion: f.motion,
            toast_max: f.toast_max,
        })
    }

    /// The font size of `role`, logical px before [`UiScale`].
    pub fn font_size(&self, role: TextRole) -> f32 {
        match role {
            TextRole::Caption => self.text.caption,
            TextRole::Body => self.text.body,
            TextRole::Label => self.text.label,
            TextRole::Title => self.text.title,
            TextRole::Display => self.text.display,
        }
    }
}

enum ParseError {
    Ron(Box<ron::error::SpannedError>),
    Color(&'static str, String),
}

/// The regular and bold faces. Bevy's default font until the files load
/// (or when the app has no text assets, as in unit tests).
#[derive(Resource, Clone, Debug, Default)]
pub struct UiFonts {
    pub regular: Handle<Font>,
    pub bold: Handle<Font>,
}

impl UiFonts {
    pub fn get(&self, role: TextRole) -> Handle<Font> {
        if role.bold() {
            self.bold.clone()
        } else {
            self.regular.clone()
        }
    }
}

/// Reads the theme's fonts from `dir` into `Assets<Font>`. A missing file
/// keeps the default font for that face and logs a warning.
pub(crate) fn load_fonts(theme: &UiTheme, dir: &Path, fonts: Option<&mut Assets<Font>>) -> UiFonts {
    let Some(fonts) = fonts else {
        return UiFonts::default();
    };
    let mut load = |file: &str| match std::fs::read(dir.join(file)) {
        Ok(bytes) => fonts.add(Font::from_bytes(bytes)),
        Err(e) => {
            warn!("ui font {file}: {e}");
            Handle::default()
        }
    };
    UiFonts {
        regular: load(&theme.fonts.regular),
        bold: load(&theme.fonts.bold),
    }
}

/// `#RRGGBB` or `#RRGGBBAA` (sRGB) as a color.
pub fn parse_hex(s: &str) -> Option<Color> {
    let s = s.strip_prefix('#')?;
    if !matches!(s.len(), 6 | 8) || !s.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    let a = if s.len() == 8 { byte(6)? } else { u8::MAX };
    Some(Color::srgba_u8(byte(0)?, byte(2)?, byte(4)?, a))
}

/// WCAG 2 contrast ratio of two opaque colors, 1..=21. Alpha is ignored:
/// pass the color the eye sees.
pub fn contrast_ratio(a: Color, b: Color) -> f32 {
    let l = |c: Color| {
        let c = c.to_linear();
        0.2126 * c.red + 0.7152 * c.green + 0.0722 * c.blue
    };
    let (hi, lo) = {
        let (x, y) = (l(a), l(b));
        (x.max(y), x.min(y))
    };
    (hi + 0.05) / (lo + 0.05)
}

/// `top` with its alpha composited over opaque `under`, in sRGB as UI
/// blending does.
pub fn over(top: Color, under: Color) -> Color {
    let t = top.to_srgba();
    let u = under.to_srgba();
    let mix = |a: f32, b: f32| a * t.alpha + b * (1.0 - t.alpha);
    Color::srgb(
        mix(t.red, u.red),
        mix(t.green, u.green),
        mix(t.blue, u.blue),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const THEME_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/ui.ron");

    #[test]
    fn hex_colors_parse() {
        assert_eq!(
            parse_hex("#FF8000"),
            Some(Color::srgba_u8(255, 128, 0, 255))
        );
        assert_eq!(parse_hex("#00000080"), Some(Color::srgba_u8(0, 0, 0, 128)));
        for bad in ["FF8000", "#FF80", "#GG8000", "#FF8000801", ""] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn contrast_ratio_matches_wcag() {
        let r = contrast_ratio(Color::WHITE, Color::BLACK);
        assert!((r - 21.0).abs() < 0.01, "{r}");
        assert!((contrast_ratio(Color::WHITE, Color::WHITE) - 1.0).abs() < 1e-4);
        // #767676 on white is the classic 4.54:1.
        let grey = parse_hex("#767676").unwrap();
        let r = contrast_ratio(grey, Color::WHITE);
        assert!((r - 4.54).abs() < 0.02, "{r}");
    }

    #[test]
    fn shipped_theme_loads_and_text_keeps_contrast() {
        let t = UiTheme::load(THEME_PATH).expect("assets/ui.ron loads");
        let c = t.colors;
        // What the eye sees: translucent panels over the darkest and the
        // brightest scene behind them.
        for scene in [Color::BLACK, Color::WHITE] {
            for (name, fg, bg) in [
                ("text on surface", c.text, c.surface),
                ("text_dim on surface", c.text_dim, c.surface),
                ("text on surface_raised", c.text, c.surface_raised),
                ("text on surface_hover", c.text, c.surface_hover),
                ("text_on_accent on accent", c.text_on_accent, c.accent),
                (
                    "text_on_accent on accent_hover",
                    c.text_on_accent,
                    c.accent_hover,
                ),
                (
                    "text_on_accent on accent_pressed",
                    c.text_on_accent,
                    c.accent_pressed,
                ),
            ] {
                let bg = over(bg, scene);
                let r = contrast_ratio(over(fg, bg), bg);
                assert!(r >= 4.5, "{name} over {scene:?}: {r:.2}:1");
            }
        }
        // XAG 101: no text below 18 px at 1080p on PC.
        assert!(t.reference_height >= 1080.0);
        assert!(t.text.caption >= 18.0, "{}", t.text.caption);
        let s = t.scale;
        assert!(s.min <= s.default && s.default <= s.max && s.step > 0.0);
        assert!(t.toast_max > 0);
    }

    #[test]
    fn a_bad_color_names_its_token() {
        let text = std::fs::read_to_string(THEME_PATH).unwrap();
        let text = text.replacen("accent: \"#F2B33D\"", "accent: \"#F2B3\"", 1);
        match UiTheme::parse(&text) {
            Err(ParseError::Color(name, value)) => {
                assert_eq!((name, value.as_str()), ("accent", "#F2B3"))
            }
            _ => panic!("expected a color error"),
        }
    }
}
