//! Themed widgets: text, panels and buttons.
//!
//! Each builder returns a bundle drawn from [`UiTheme`] tokens; a widget
//! never takes a color or a size. Buttons are Bevy's headless
//! `ui_widgets::Button` (pointer, Enter and Space activate it, and it is
//! announced as a button to screen readers through AccessKit) with the
//! kit's look applied every frame from its state ([`button_look`]):
//! hovered, pressed, focused. What a button does is its [`UiAction`],
//! handled in one place ([`super::screens`]).
//!
//! Keyboard focus is always visible: the focused button gets the theme's
//! focus ring, whatever moved the focus.

use bevy::input_focus::InputFocus;
use bevy::input_focus::tab_navigation::TabIndex;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::Pressed;
use bevy::ui_widgets;

use super::theme::{ColorToken, TextRole, UiFonts, UiTheme};

/// What activating a button does.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UiAction {
    /// Close every screen and return to play.
    Resume,
    OpenSettings,
    /// Close the top screen.
    Back,
    Quit,
    ScaleDown,
    ScaleUp,
    ScaleReset,
    /// Switch the temperature unit (Fahrenheit, Celsius).
    ToggleTemperatureUnit,
}

/// A button's emphasis.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    /// The one main choice on a screen: accent fill.
    Primary,
    /// Everything else: raised surface.
    Secondary,
}

/// The state a button is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonVisual {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
}

/// What a button looks like: fill, label color and focus ring color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonLook {
    pub fill: Color,
    pub label: Color,
    pub ring: Color,
}

pub fn button_look(theme: &UiTheme, style: ButtonStyle, v: ButtonVisual) -> ButtonLook {
    let c = &theme.colors;
    let (fill, label) = match (style, v.pressed, v.hovered) {
        (ButtonStyle::Primary, true, _) => (c.accent_pressed, c.text_on_accent),
        (ButtonStyle::Primary, false, true) => (c.accent_hover, c.text_on_accent),
        (ButtonStyle::Primary, false, false) => (c.accent, c.text_on_accent),
        (ButtonStyle::Secondary, true, _) => (c.surface, c.text),
        (ButtonStyle::Secondary, false, true) => (c.surface_hover, c.text),
        (ButtonStyle::Secondary, false, false) => (c.surface_raised, c.text),
    };
    let ring = if v.focused { c.focus } else { Color::NONE };
    ButtonLook { fill, label, ring }
}

/// Marks a text whose role and color the kit set, so it can be restyled.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiText {
    pub role: TextRole,
    pub color: ColorToken,
}

/// A text in the theme's font and size for `role`.
pub fn text(
    theme: &UiTheme,
    fonts: &UiFonts,
    role: TextRole,
    color: ColorToken,
    value: impl Into<String>,
) -> impl Bundle {
    (
        UiText { role, color },
        Text::new(value),
        TextFont::from_font_size(theme.font_size(role)).with_font(fonts.get(role)),
        TextColor(theme.colors.get(color)),
    )
}

/// A raised panel that stacks its children: surface fill, hairline
/// border, rounded corners and a soft shadow.
pub fn panel(theme: &UiTheme) -> impl Bundle {
    let s = &theme.space;
    (
        Node {
            width: Val::Px(theme.panel_width),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            padding: UiRect::all(Val::Px(s.xl)),
            row_gap: Val::Px(s.md),
            border: UiRect::all(Val::Px(theme.border)),
            border_radius: BorderRadius::all(Val::Px(theme.radius.md)),
            ..default()
        },
        BackgroundColor(theme.colors.surface),
        BorderColor::all(theme.colors.border),
        BoxShadow::new(
            theme.colors.shadow,
            Val::ZERO,
            Val::Px(s.sm),
            Val::ZERO,
            Val::Px(s.xl),
        ),
    )
}

/// A button labelled `label` that does `action`.
pub fn button(
    theme: &UiTheme,
    fonts: &UiFonts,
    style: ButtonStyle,
    action: UiAction,
    label: impl Into<String>,
) -> impl Bundle {
    let look = button_look(theme, style, ButtonVisual::default());
    let ring = theme.focus_ring;
    (
        (
            ui_widgets::Button,
            style,
            action,
            Hovered::default(),
            TabIndex(0),
        ),
        Node {
            min_height: Val::Px(theme.button.height),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            padding: UiRect::axes(Val::Px(theme.space.lg), Val::Px(theme.space.sm)),
            border_radius: BorderRadius::all(Val::Px(theme.radius.sm)),
            ..default()
        },
        BackgroundColor(look.fill),
        Outline::new(Val::Px(ring.width), Val::Px(ring.offset), look.ring),
        children![text(
            theme,
            fonts,
            TextRole::Label,
            label_token(style),
            label
        )],
    )
}

fn label_token(style: ButtonStyle) -> ColorToken {
    match style {
        ButtonStyle::Primary => ColorToken::TextOnAccent,
        ButtonStyle::Secondary => ColorToken::Text,
    }
}

/// Draws every button in its current state.
#[allow(clippy::type_complexity)]
pub(crate) fn style_buttons(
    theme: Res<UiTheme>,
    focus: Option<Res<InputFocus>>,
    mut buttons: Query<(
        Entity,
        &ButtonStyle,
        &Hovered,
        Has<Pressed>,
        &mut BackgroundColor,
        &mut Outline,
        &Children,
    )>,
    mut labels: Query<&mut TextColor>,
) {
    let focused = focus.and_then(|f| f.get());
    for (e, style, hovered, pressed, mut fill, mut outline, children) in &mut buttons {
        let v = ButtonVisual {
            hovered: hovered.get(),
            pressed,
            focused: focused == Some(e),
        };
        let look = button_look(&theme, *style, v);
        fill.set_if_neq(BackgroundColor(look.fill));
        if outline.color != look.ring {
            outline.color = look.ring;
        }
        for child in children {
            if let Ok(mut color) = labels.get_mut(*child) {
                color.set_if_neq(TextColor(look.label));
            }
        }
    }
}

/// Applies the theme's fonts to kit texts once they are loaded or change.
pub(crate) fn restyle_texts(
    theme: Res<UiTheme>,
    fonts: Res<UiFonts>,
    mut texts: Query<(&UiText, &mut TextFont)>,
) {
    if !fonts.is_changed() && !theme.is_changed() {
        return;
    }
    for (t, mut font) in &mut texts {
        *font = TextFont::from_font_size(theme.font_size(t.role)).with_font(fonts.get(t.role));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::contrast_ratio;
    use crate::ui::theme::tests::THEME_PATH;

    #[test]
    fn button_states_differ_and_focus_shows_a_ring() {
        let t = UiTheme::load(THEME_PATH).unwrap();
        for style in [ButtonStyle::Primary, ButtonStyle::Secondary] {
            let at = |hovered, pressed, focused| {
                button_look(
                    &t,
                    style,
                    ButtonVisual {
                        hovered,
                        pressed,
                        focused,
                    },
                )
            };
            let rest = at(false, false, false);
            assert_ne!(rest.fill, at(true, false, false).fill, "{style:?} hover");
            assert_ne!(rest.fill, at(true, true, false).fill, "{style:?} press");
            assert_eq!(rest.ring, Color::NONE);
            assert_eq!(at(false, false, true).ring, t.colors.focus);
            // The ring stands out against the panel it sits on.
            let r = contrast_ratio(t.colors.focus, t.colors.surface);
            assert!(r >= 3.0, "focus ring {r:.2}:1");
        }
    }
}
