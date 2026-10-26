//! Toasts: short notices at the top of the screen.
//!
//! Any system writes a [`Toast`] message; the kit shows it on the toast
//! layer, newest at the bottom of the stack, fades it in and out over the
//! theme's motion times and removes it. At most `toast_max` show at once;
//! a new one pushes out the oldest. Toasts never take the pointer or the
//! focus, so they cannot interrupt play.

use bevy::picking::Pickable;
use bevy::prelude::*;

use super::layers::{UiLayer, layer_root};
use super::theme::{ColorToken, Motion, TextRole, UiFonts, UiTheme};
use super::widgets::text;

/// A notice to show.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    /// The color of its edge mark: `Info`, `Ok`, `Warning`, `Danger`.
    pub tone: ColorToken,
}

impl Toast {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: ColorToken::Info,
        }
    }
}

/// The column the toasts stack in.
#[derive(Component, Clone, Copy, Debug)]
pub struct ToastStack;

/// A shown toast and how long it has shown.
#[derive(Component, Clone, Copy, Debug)]
pub struct ToastEntry {
    pub age: f32,
    seq: u64,
}

/// The opacity of a toast `age` seconds old, or `None` once it is gone:
/// fades in over `fast`, holds for `toast`, fades out over `normal`.
pub fn toast_alpha(age: f32, motion: &Motion) -> Option<f32> {
    let fade_in = motion.fast.max(f32::EPSILON);
    let fade_out = motion.normal.max(f32::EPSILON);
    let end_hold = fade_in + motion.toast;
    if age < 0.0 || age >= end_hold + fade_out {
        None
    } else if age < fade_in {
        Some(age / fade_in)
    } else if age < end_hold {
        Some(1.0)
    } else {
        Some(1.0 - (age - end_hold) / fade_out)
    }
}

pub(crate) fn spawn_toast_stack(
    mut commands: Commands,
    theme: Res<UiTheme>,
    layers: Query<(Entity, &UiLayer)>,
    stacks: Query<(), With<ToastStack>>,
) {
    if !stacks.is_empty() {
        return;
    }
    let Some(root) = layer_root(&layers, UiLayer::Toast) else {
        return;
    };
    let inset = Val::VMin(theme.safe_area * 100.0);
    commands.spawn((
        ToastStack,
        Node {
            position_type: PositionType::Absolute,
            top: inset,
            left: Val::ZERO,
            right: Val::ZERO,
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(theme.space.sm),
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(root),
    ));
}

/// Shows new toasts, ages and fades the shown ones and drops the old.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_toasts(
    mut commands: Commands,
    mut toasts: MessageReader<Toast>,
    time: Res<Time>,
    theme: Res<UiTheme>,
    fonts: Res<UiFonts>,
    stacks: Query<Entity, With<ToastStack>>,
    mut shown: Query<(Entity, &mut ToastEntry, &mut UiTransparency)>,
    mut next_seq: Local<u64>,
) {
    let dt = time.delta_secs();
    let mut alive: Vec<(u64, Entity)> = Vec::new();
    for (e, mut entry, mut alpha) in &mut shown {
        entry.age += dt;
        match toast_alpha(entry.age, &theme.motion) {
            Some(a) => {
                alpha.0 = a;
                alive.push((entry.seq, e));
            }
            None => commands.entity(e).despawn(),
        }
    }
    let Some(stack) = stacks.iter().next() else {
        return;
    };
    for toast in toasts.read() {
        *next_seq += 1;
        let e = commands
            .spawn((
                ToastEntry {
                    age: 0.0,
                    seq: *next_seq,
                },
                UiTransparency(0.0),
                toast_card(&theme, &fonts, toast),
                ChildOf(stack),
            ))
            .id();
        alive.push((*next_seq, e));
    }
    alive.sort_unstable();
    let extra = alive.len().saturating_sub(theme.toast_max);
    for (_, e) in &alive[..extra] {
        commands.entity(*e).despawn();
    }
}

/// How opaque a toast is; [`apply_transparency`] multiplies its colors.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct UiTransparency(pub f32);

fn toast_card(theme: &UiTheme, fonts: &UiFonts, toast: &Toast) -> impl Bundle {
    let s = &theme.space;
    (
        Node {
            max_width: Val::Px(theme.panel_width * 1.5),
            padding: UiRect::axes(Val::Px(s.lg), Val::Px(s.md)),
            border: UiRect::left(Val::Px(s.xs)),
            border_radius: BorderRadius::all(Val::Px(theme.radius.sm)),
            ..default()
        },
        BackgroundColor(theme.colors.surface),
        BorderColor::all(theme.colors.get(toast.tone)),
        Pickable::IGNORE,
        children![(
            text(
                theme,
                fonts,
                TextRole::Body,
                ColorToken::Text,
                toast.text.clone()
            ),
            Pickable::IGNORE,
        )],
    )
}

/// Fades a toast: its fill, edge and text take the theme colors times its
/// [`UiTransparency`].
pub(crate) fn apply_transparency(
    theme: Res<UiTheme>,
    mut cards: Query<
        (
            &UiTransparency,
            &mut BackgroundColor,
            &mut BorderColor,
            &Children,
        ),
        Changed<UiTransparency>,
    >,
    mut texts: Query<&mut TextColor>,
) {
    let fade = |c: Color, a: f32| c.with_alpha(c.alpha() * a);
    for (t, mut fill, mut border, children) in &mut cards {
        fill.0 = fade(theme.colors.surface, t.0);
        let edge = border.left;
        *border = BorderColor::all(edge.with_alpha(t.0));
        for child in children {
            if let Ok(mut color) = texts.get_mut(*child) {
                color.0 = fade(theme.colors.text, t.0);
            }
        }
    }
}
