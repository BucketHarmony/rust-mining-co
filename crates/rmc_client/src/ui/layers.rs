//! Layers and HUD anchors.
//!
//! The kit's UI lives on [`UiLayer`] roots, each a full-screen node with a
//! fixed [`GlobalZIndex`], so draw order is decided once: the HUD under the
//! menus, notices over both. The roots ignore the pointer; only widgets
//! take clicks.
//!
//! The HUD layer is inset by the theme's safe area and holds the nine
//! [`HudAnchor`] regions. A HUD element does not position itself: it
//! carries a [`HudSlot`] and is moved into its anchor, where elements stack
//! in spawn order. Moving an element to another corner is a one-word
//! change, and nothing overlaps the screen edge.

use bevy::picking::Pickable;
use bevy::prelude::*;

use super::theme::UiTheme;
use crate::render::GameCamera;

/// A UI layer. See the module docs.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UiLayer {
    Hud,
    Menu,
    Toast,
}

impl UiLayer {
    pub const ALL: [Self; 3] = [Self::Hud, Self::Menu, Self::Toast];

    /// Above the existing HUD root (z 0), in layer order.
    pub fn z(self) -> i32 {
        match self {
            Self::Hud => 10,
            Self::Menu => 100,
            Self::Toast => 200,
        }
    }
}

/// One of the nine HUD regions inside the safe area.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HudAnchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl HudAnchor {
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Center,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// (vertical, horizontal) placement of the anchor's stack.
    fn align(self) -> (JustifyContent, AlignItems) {
        use AlignItems as H;
        use JustifyContent as V;
        match self {
            Self::TopLeft => (V::Start, H::Start),
            Self::Top => (V::Start, H::Center),
            Self::TopRight => (V::Start, H::End),
            Self::Left => (V::Center, H::Start),
            Self::Center => (V::Center, H::Center),
            Self::Right => (V::Center, H::End),
            Self::BottomLeft => (V::End, H::Start),
            Self::Bottom => (V::End, H::Center),
            Self::BottomRight => (V::End, H::End),
        }
    }
}

/// Put on a HUD element to place it in an anchor.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HudSlot(pub HudAnchor);

fn full_screen() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::ZERO,
        right: Val::ZERO,
        top: Val::ZERO,
        bottom: Val::ZERO,
        ..default()
    }
}

pub(crate) fn spawn_layers(mut commands: Commands, theme: Res<UiTheme>) {
    let gap = Val::Px(theme.space.sm);
    for layer in UiLayer::ALL {
        let root = commands
            .spawn((
                layer,
                full_screen(),
                GlobalZIndex(layer.z()),
                Pickable::IGNORE,
            ))
            .id();
        if layer != UiLayer::Hud {
            continue;
        }
        let inset = Val::VMin(theme.safe_area * 100.0);
        let safe = commands
            .spawn((
                Node {
                    left: inset,
                    right: inset,
                    top: inset,
                    bottom: inset,
                    ..full_screen()
                },
                Pickable::IGNORE,
                ChildOf(root),
            ))
            .id();
        for anchor in HudAnchor::ALL {
            let (justify_content, align_items) = anchor.align();
            commands.spawn((
                anchor,
                Node {
                    flex_direction: FlexDirection::Column,
                    justify_content,
                    align_items,
                    row_gap: gap,
                    ..full_screen()
                },
                Pickable::IGNORE,
                ChildOf(safe),
            ));
        }
    }
}

/// Draws the layers on the game camera (a window, or the test kit's image).
pub(crate) fn attach_to_camera(
    mut commands: Commands,
    roots: Query<Entity, (With<UiLayer>, Without<UiTargetCamera>)>,
    cameras: Query<Entity, With<GameCamera>>,
) {
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    for root in &roots {
        commands.entity(root).insert(UiTargetCamera(camera));
    }
}

/// Moves every [`HudSlot`] element under its anchor.
pub(crate) fn place_in_slots(
    mut commands: Commands,
    slots: Query<(Entity, &HudSlot, Option<&ChildOf>)>,
    anchors: Query<(Entity, &HudAnchor)>,
) {
    for (e, slot, parent) in &slots {
        let Some((anchor, _)) = anchors.iter().find(|(_, a)| **a == slot.0) else {
            continue;
        };
        if parent.is_none_or(|p| p.parent() != anchor) {
            commands.entity(e).insert(ChildOf(anchor));
        }
    }
}

/// The root entity of `layer`, once spawned.
pub fn layer_root(roots: &Query<(Entity, &UiLayer)>, layer: UiLayer) -> Option<Entity> {
    roots.iter().find(|(_, l)| **l == layer).map(|(e, _)| e)
}
