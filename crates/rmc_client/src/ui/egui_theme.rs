//! The kit's look for the egui windows: the F1 editor panel and the F11
//! Surfaces window.
//!
//! Those tools are egui, not Bevy UI: they are dense forms (sliders,
//! pickers, an inspector) that egui already does well. They take the same
//! tokens as the rest of the UI ([`UiTheme`]): the umber surfaces, the
//! amber accent, the off-white text, the status colors, the type ramp, the
//! spacing grid and the radii. Fonts are Atkinson Hyperlegible, and the
//! windows scale with [`UiScale`] like the kit's own UI. Without the kit
//! (no `UiTheme`) egui keeps its default look.

use std::path::Path;
use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle,
};

use super::theme::UiTheme;

/// The egui name of the kit's regular font.
pub const EGUI_REGULAR: &str = "ui-regular";
/// The egui name of the kit's bold font, the family headings use.
pub const EGUI_BOLD: &str = "ui-bold";

/// `c` as an egui color (sRGB, alpha kept).
pub fn color32(c: Color) -> Color32 {
    let [r, g, b, a] = c.to_srgba().to_u8_array();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// A width or radius token as egui's byte-sized corner radius.
fn corner(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().clamp(0.0, f32::from(u8::MAX)) as u8)
}

/// A spacing token as an egui margin.
fn margin(m: f32) -> Margin {
    Margin::same(m.round().clamp(0.0, f32::from(i8::MAX)) as i8)
}

/// `style` in the kit's look: colors, type sizes, spacing, radii.
pub fn apply_style(theme: &UiTheme, style: &mut egui::Style) {
    let c = &theme.colors;
    let t = &theme.text;
    let s = &theme.space;
    let bold = FontFamily::Name(EGUI_BOLD.into());
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(t.caption)),
        (TextStyle::Body, FontId::proportional(t.caption)),
        (TextStyle::Button, FontId::proportional(t.caption)),
        (TextStyle::Monospace, FontId::monospace(t.caption)),
        (TextStyle::Heading, FontId::new(t.label, bold)),
    ]
    .into();

    let sp = &mut style.spacing;
    sp.item_spacing = egui::vec2(s.sm, s.sm);
    sp.button_padding = egui::vec2(s.md, s.xs);
    sp.window_margin = margin(s.md);
    sp.menu_margin = margin(s.sm);
    sp.indent = s.lg;
    sp.interact_size.y = t.caption + s.md;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = None;
    v.panel_fill = color32(c.surface);
    v.window_fill = color32(c.surface);
    v.window_stroke = Stroke::new(theme.border, color32(c.border));
    v.window_corner_radius = corner(theme.radius.md);
    v.menu_corner_radius = corner(theme.radius.md);
    let shadow = Shadow {
        offset: [0, s.sm.round() as i8],
        blur: s.xl.round() as u8,
        spread: 0,
        color: color32(c.shadow),
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.faint_bg_color = color32(c.surface_raised);
    v.extreme_bg_color = color32(c.surface);
    v.text_edit_bg_color = Some(color32(c.surface));
    v.code_bg_color = color32(c.surface_raised);
    v.hyperlink_color = color32(c.info);
    v.warn_fg_color = color32(c.warning);
    v.error_fg_color = color32(c.danger);
    v.selection.bg_fill = color32(c.accent);
    v.selection.stroke = Stroke::new(theme.border, color32(c.text_on_accent));
    v.slider_trailing_fill = true;

    let radius = corner(theme.radius.sm);
    let w = &mut v.widgets;
    for (wv, fill, stroke, text) in [
        (&mut w.noninteractive, c.surface, c.border, c.text_dim),
        (&mut w.inactive, c.surface_raised, c.border, c.text),
        (&mut w.hovered, c.surface_hover, c.accent_hover, c.text),
        (&mut w.active, c.accent_pressed, c.accent, c.text_on_accent),
        (&mut w.open, c.surface_hover, c.accent, c.text),
    ] {
        wv.bg_fill = color32(fill);
        wv.weak_bg_fill = color32(fill);
        wv.bg_stroke = Stroke::new(theme.border, color32(stroke));
        wv.fg_stroke = Stroke::new(theme.border, color32(text));
        wv.corner_radius = radius;
    }
    // The panel's separators and frames read as hairlines, not boxes.
    w.noninteractive.bg_stroke = Stroke::new(theme.border, color32(c.border));
    w.noninteractive.bg_fill = color32(c.surface);
}

/// egui's fonts with the kit's two in front: regular for the proportional
/// family, bold as its own family for headings. `None` if a file does not
/// read.
pub fn font_definitions(theme: &UiTheme, dir: &Path) -> Option<FontDefinitions> {
    let read = |file: &str| match std::fs::read(dir.join(file)) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            warn!("ui font {file}: {e}");
            None
        }
    };
    let regular = read(&theme.fonts.regular)?;
    let bold = read(&theme.fonts.bold)?;
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert(EGUI_REGULAR.into(), Arc::new(FontData::from_owned(regular)));
    fonts
        .font_data
        .insert(EGUI_BOLD.into(), Arc::new(FontData::from_owned(bold)));
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, EGUI_REGULAR.into());
    let mut heading = vec![EGUI_BOLD.to_string()];
    heading.extend(fallback);
    fonts
        .families
        .insert(FontFamily::Name(EGUI_BOLD.into()), heading);
    Some(fonts)
}

/// Gives `ctx` the kit's look and fonts (fonts only with `dir`).
pub fn apply(ctx: &egui::Context, theme: &UiTheme, dir: Option<&Path>) {
    match dir.and_then(|d| font_definitions(theme, d)) {
        Some(fonts) => ctx.set_fonts(fonts),
        None => {
            // The heading family must exist even without the files.
            let mut fonts = FontDefinitions::default();
            let fallback = fonts
                .families
                .get(&FontFamily::Proportional)
                .cloned()
                .unwrap_or_default();
            fonts
                .families
                .insert(FontFamily::Name(EGUI_BOLD.into()), fallback);
            ctx.set_fonts(fonts);
        }
    }
    ctx.all_styles_mut(|style| apply_style(theme, style));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests::THEME_PATH;

    fn theme() -> UiTheme {
        UiTheme::load(THEME_PATH).unwrap()
    }

    #[test]
    fn egui_style_takes_the_kit_tokens() {
        let t = theme();
        let mut style = egui::Style::default();
        apply_style(&t, &mut style);
        let v = &style.visuals;
        assert_eq!(v.panel_fill, color32(t.colors.surface));
        assert_eq!(v.window_fill, color32(t.colors.surface));
        assert_eq!(v.window_stroke.color, color32(t.colors.border));
        assert_eq!(v.selection.bg_fill, color32(t.colors.accent));
        assert_eq!(v.widgets.inactive.bg_fill, color32(t.colors.surface_raised));
        assert_eq!(v.widgets.hovered.bg_fill, color32(t.colors.surface_hover));
        assert_eq!(v.widgets.active.bg_fill, color32(t.colors.accent_pressed));
        assert_eq!(v.widgets.inactive.text_color(), color32(t.colors.text));
        assert_eq!(v.warn_fg_color, color32(t.colors.warning));
        assert_eq!(v.error_fg_color, color32(t.colors.danger));
        // No text below the type ramp's smallest size (XAG 101).
        for (style, font) in &style.text_styles {
            assert!(font.size >= t.text.caption, "{style:?} {}", font.size);
        }
        assert_eq!(style.text_styles[&TextStyle::Heading].size, t.text.label);
        assert_eq!(
            style.spacing.item_spacing,
            egui::vec2(t.space.sm, t.space.sm)
        );
    }

    #[test]
    fn egui_fonts_put_the_kit_fonts_first() {
        let t = theme();
        let dir = Path::new(THEME_PATH).parent().unwrap();
        let fonts = font_definitions(&t, dir).expect("the shipped fonts read");
        assert_eq!(fonts.families[&FontFamily::Proportional][0], EGUI_REGULAR);
        assert_eq!(
            fonts.families[&FontFamily::Name(EGUI_BOLD.into())][0],
            EGUI_BOLD
        );
        assert!(font_definitions(&t, Path::new("no/such/dir")).is_none());

        // A context draws a heading and body text in the kit's look.
        let ctx = egui::Context::default();
        apply(&ctx, &t, Some(dir));
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.heading("Editor");
            ui.label("Brush");
        });
        out.drop_without_applying_deltas();
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
            color32(t.colors.surface),
            "the context has the kit's style"
        );
    }
}
