//! The HUD (SAD §11.5): crosshair, targeted cell outline, material name,
//! damage progress and inventory counts, drawn with Bevy UI and gizmos.
//!
//! [`HudState`] mirrors what the HUD shows, so tests assert on it instead of
//! reading pixels. The UI and the outline are drawn from it and nothing
//! else.
//!
//! - Crosshair: shown while there is a [`Player`].
//! - Target: the [`PickTarget`] cell, its material's key as the name, and a
//!   gizmo outline around it (when the app has gizmos). Nothing is targeted
//!   while the fly camera is on, because the pickaxe does not swing then.
//! - Damage progress: an estimate. Damage lives on the host only and is
//!   never sent (SAD §7; a damage that does not break produces no delta), so
//!   [`DamageEstimate`] adds up the `Damage` of the edits this client sent,
//!   per cell, for cells that are diggable in the replica, capped at the
//!   cell's hardness. A cell's count is forgotten when a delta or snapshot
//!   changes it (it broke or was replaced) and on `Welcome`. Damage dealt by
//!   other players, or before a load, is not seen.
//! - Inventory: the host sends an `Inventory` message with the new totals
//!   after every `Applied` result that yields (SAD §8.4). [`ClientInventory`]
//!   holds the last totals; `Welcome` empties it. Yields in `Applied` are
//!   not added on top, so counts are never doubled.
//! - Swing feedback: every swing the pickaxe sends ([`SwingState`]) starts a
//!   pulse of [`SWING_PULSE_S`]: the crosshair arms grow and warm, and the
//!   outline grows and lights up, both fading back.
//! - Break feedback: when an `Applied` delta turns a solid cell into a
//!   non-solid one, and that cell was the targeted cell or one this client
//!   has damaged, the cell gets a break effect of [`BREAK_EFFECT_S`]: a
//!   flash outline that grows and fades, and chips in the cell's material
//!   color flying out (gizmos, when the app has them). The material is read
//!   from the replica before the delta applies. `Full` deltas (big edits,
//!   never a pickaxe swing) and snapshots show no effect.

use std::collections::BTreeMap;
use std::time::Duration;

use bevy::gizmos::GizmoPlugin;
use bevy::prelude::*;
use bevy::ui::UiTargetCamera;
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::coords::{CELL_SIZE_M, CellPos, ChunkPos, LocalPos};
use rmc_world::edit::{ChunkDelta, EditOp};
use rmc_world::inventory::Inventory;
use rmc_world::materials::{MaterialId, MaterialTable};

use crate::controller::Player;
use crate::flycam::FlyCam;
use crate::net::{ClientMaterials, FromHost, NetSystems, ToHost};
use crate::pickaxe::{PickTarget, PickaxeSystems, SwingState};
use crate::render::GameCamera;
use crate::replica::{Replica, apply_host_messages};

/// The targeted cell's outline color: dark, to read against the mostly
/// light terrain colors.
pub const OUTLINE_COLOR: Color = Color::srgb(0.05, 0.05, 0.05);

/// The outline's size relative to the cell, a little larger so it is not
/// hidden in the cell's faces.
pub const OUTLINE_SCALE: f32 = 1.02;

/// Size of the HUD text, in logical pixels.
pub const HUD_FONT_SIZE: f32 = 16.0;

/// The damage bar's size, in logical pixels.
pub const DAMAGE_BAR_SIZE: Vec2 = Vec2::new(120.0, 6.0);

/// The crosshair's arm length and thickness, in logical pixels.
pub const CROSSHAIR_SIZE: Vec2 = Vec2::new(14.0, 2.0);

/// How long the crosshair and outline pulse after a swing, in seconds.
pub const SWING_PULSE_S: f32 = 0.15;

/// How long the effect at a broken cell lasts, in seconds.
pub const BREAK_EFFECT_S: f32 = 0.4;

/// The crosshair arms' extra length at the peak of a swing pulse, as a
/// fraction of [`CROSSHAIR_SIZE`].
pub const SWING_PULSE_GROWTH: f32 = 0.6;

/// The crosshair and outline color at the peak of a swing pulse.
pub const SWING_PULSE_COLOR: Color = Color::srgb(1.0, 0.82, 0.35);

/// The outline's extra size at the peak of a swing pulse, relative to the
/// cell.
pub const OUTLINE_PULSE_GROWTH: f32 = 0.12;

/// The break flash outline's extra size at the end of the effect, relative
/// to the cell.
pub const BREAK_FLASH_GROWTH: f32 = 0.5;

/// Number of chips a breaking cell throws.
pub const BREAK_CHIPS: usize = 8;

/// A chip's edge length, in meters.
pub const CHIP_SIZE_M: f32 = 0.1;

/// A chip's initial speed, in meters per second.
pub const CHIP_SPEED_M_S: f32 = 2.0;

/// The downward acceleration on chips, in meters per second squared.
pub const CHIP_GRAVITY_M_S2: f32 = 9.8;

/// Damage dealt to the targeted cell so far, out of its hardness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageProgress {
    /// Estimated damage on the cell, at most `hardness`.
    pub dealt: u16,
    /// Damage that breaks the cell.
    pub hardness: u16,
}

impl DamageProgress {
    /// `dealt / hardness`, in `0.0..=1.0` (1.0 for a hardness of 0).
    pub fn fraction(&self) -> f32 {
        if self.hardness == 0 {
            1.0
        } else {
            f32::from(self.dealt.min(self.hardness)) / f32::from(self.hardness)
        }
    }
}

/// The effect shown where a cell broke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BreakEffect {
    /// The cell that broke.
    pub cell: CellPos,
    /// The material the cell held before it broke.
    pub material: MaterialId,
    /// 1.0 when the cell breaks, falling linearly to 0.0 at the end of the
    /// effect.
    pub fade: f32,
}

/// One inventory line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HudItem {
    pub material: MaterialId,
    /// The material's key.
    pub name: String,
    pub count: u32,
}

/// What the HUD shows.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct HudState {
    /// Whether the crosshair is drawn.
    pub crosshair: bool,
    /// The outlined cell.
    pub target: Option<CellPos>,
    /// The targeted cell's material key.
    pub material: Option<String>,
    /// Damage on the targeted cell, when there is any.
    pub damage: Option<DamageProgress>,
    /// Held materials with a nonzero count, ascending by material id.
    pub inventory: Vec<HudItem>,
    /// Swing feedback: 1.0 on the frame of a swing, falling linearly to 0.0.
    pub swing_pulse: f32,
    /// The break effect being shown, if any.
    pub last_break: Option<BreakEffect>,
}

/// The player's inventory as of the host's last `Inventory` message.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientInventory(pub Inventory);

/// Damage this client has sent per cell, not yet undone by a change to the
/// cell. Entries are above zero and at most the cell's hardness.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct DamageEstimate(BTreeMap<CellPos, u16>);

impl DamageEstimate {
    /// Estimated damage on `cell` (0 when none).
    pub fn get(&self, cell: CellPos) -> u16 {
        self.0.get(&cell).copied().unwrap_or(0)
    }

    /// Number of cells with damage.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn clear_chunk(&mut self, pos: ChunkPos) {
        self.0.retain(|cell, _| cell.chunk() != pos);
    }
}

/// When the last swing and the last break happened, in `Time` elapsed.
#[derive(Resource, Debug, Default)]
struct Feedback {
    /// [`SwingState::swings`] as of the last frame.
    seen_swings: u64,
    swung_at: Option<Duration>,
    /// The broken cell, the material it held, and when it broke.
    broke: Option<(CellPos, MaterialId, Duration)>,
}

/// `1.0` at age zero, falling linearly to `0.0` at `length_s`.
fn fade(age: Duration, length_s: f32) -> f32 {
    (1.0 - age.as_secs_f32() / length_s).max(0.0)
}

/// [`HudPlugin`]'s system sets.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HudSystems {
    /// `PreUpdate`, after [`NetSystems::Receive`]: inventory and damage
    /// estimate follow the host's messages.
    Track,
    /// `Update`, after [`PickaxeSystems::Target`]: the estimate takes the
    /// frame's edits, [`HudState`] is recomputed, the UI follows it.
    Show,
}

/// Root node of the HUD.
#[derive(Component, Debug)]
pub struct HudRoot;

#[derive(Component, Debug)]
struct CrosshairNode;

/// One of the crosshair's two arms.
#[derive(Component, Debug)]
pub(crate) struct CrosshairArm {
    pub(crate) vertical: bool,
}

#[derive(Component, Debug)]
struct TargetText;

#[derive(Component, Debug)]
struct DamageBar;

#[derive(Component, Debug)]
struct DamageFill;

#[derive(Component, Debug)]
struct InventoryText;

/// The HUD. Needs [`NetPlugin`](crate::net::NetPlugin) for host messages
/// and [`PickaxePlugin`](crate::pickaxe::PickaxePlugin) for the target.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<FromHost>()
            .add_message::<ToHost>()
            .init_resource::<HudState>()
            .init_resource::<ClientInventory>()
            .init_resource::<DamageEstimate>()
            .init_resource::<PickTarget>()
            .init_resource::<Feedback>()
            .add_systems(Startup, spawn_hud)
            .add_systems(
                PreUpdate,
                track_host
                    .in_set(HudSystems::Track)
                    .after(NetSystems::Receive)
                    // Reads the cells a delta replaces before they change.
                    .before(apply_host_messages),
            )
            .add_systems(
                Update,
                (
                    attach_to_camera,
                    estimate_damage,
                    update_state,
                    show_state.run_if(resource_changed::<HudState>),
                )
                    .chain()
                    .in_set(HudSystems::Show)
                    .after(PickaxeSystems::Target),
            );
        // A logic-only app has no gizmos.
        if app.is_plugin_added::<GizmoPlugin>() {
            app.add_systems(Update, (draw_outline, draw_break).after(HudSystems::Show));
        }
    }
}

/// Follows the host: inventory totals, cells whose estimate is void, and
/// cells the player sees break.
#[allow(clippy::too_many_arguments)]
fn track_host(
    mut inbox: MessageReader<FromHost>,
    replica: Option<Res<Replica>>,
    target: Res<PickTarget>,
    time: Res<Time>,
    mut inventory: ResMut<ClientInventory>,
    mut estimate: ResMut<DamageEstimate>,
    mut feedback: ResMut<Feedback>,
) {
    let world = replica.as_deref().map(Replica::world);
    let solid = |id: Option<MaterialId>| {
        let def = id.and_then(|id| world.and_then(|w| w.materials().get(id)));
        def.is_some_and(|d| d.is_solid())
    };
    let aimed = target.0.map(|hit| hit.cell);
    for FromHost(msg) in inbox.read() {
        match msg {
            ServerMsg::Welcome { .. } => {
                inventory.0 = Inventory::new();
                estimate.0.clear();
                feedback.broke = None;
            }
            ServerMsg::Inventory { items } => inventory.0 = Inventory::from_items(items),
            ServerMsg::ChunkSnapshot { pos, .. } => estimate.clear_chunk(*pos),
            ServerMsg::Applied(result) => {
                // The targeted cell's break wins over the other cells'.
                let mut broke: Option<(CellPos, MaterialId)> = None;
                for delta in &result.deltas {
                    match delta {
                        ChunkDelta::Cells { pos, changes, .. } => {
                            for &(index, new) in changes {
                                let cell =
                                    CellPos::from_chunk_local(*pos, LocalPos::from_index(index));
                                let ours = aimed == Some(cell) || estimate.0.contains_key(&cell);
                                let old = world.and_then(|w| w.get_cell(cell));
                                if ours
                                    && solid(old)
                                    && !solid(Some(new))
                                    && let Some(old) = old
                                    && (broke.is_none() || aimed == Some(cell))
                                {
                                    broke = Some((cell, old));
                                }
                                estimate.0.remove(&cell);
                            }
                        }
                        ChunkDelta::Full { pos, .. } => estimate.clear_chunk(*pos),
                        ChunkDelta::Layers { .. } => {}
                    }
                }
                if let Some((cell, material)) = broke {
                    feedback.broke = Some((cell, material, time.elapsed()));
                }
            }
            ServerMsg::Rejected { .. }
            | ServerMsg::PlayerStates(_)
            | ServerMsg::Saved { .. }
            | ServerMsg::LayerSnapshot { .. }
            | ServerMsg::SeasonStarted { .. }
            | ServerMsg::SeasonDone { .. } => {}
        }
    }
}

/// Adds the damage of this frame's `Damage` edits to the estimate.
fn estimate_damage(
    mut sent: MessageReader<ToHost>,
    replica: Option<Res<Replica>>,
    mut estimate: ResMut<DamageEstimate>,
) {
    let Some(replica) = replica else {
        sent.clear();
        return;
    };
    let world = replica.world();
    for ToHost(msg) in sent.read() {
        let ClientMsg::Edit(cmd) = msg else {
            continue;
        };
        let EditOp::Damage(dmg) = cmd.op else {
            continue;
        };
        for cell in cmd.shape.cells() {
            let Some(def) = world
                .get_cell(cell)
                .and_then(|id| world.materials().get(id))
            else {
                continue;
            };
            if !def.diggable || !def.is_solid() {
                continue;
            }
            let total = estimate.get(cell).saturating_add(dmg).min(def.hardness);
            if total > 0 {
                estimate.0.insert(cell, total);
            }
        }
    }
}

/// Recomputes [`HudState`].
#[allow(clippy::too_many_arguments)]
fn update_state(
    target: Res<PickTarget>,
    replica: Option<Res<Replica>>,
    client_materials: Option<Res<ClientMaterials>>,
    estimate: Res<DamageEstimate>,
    inventory: Res<ClientInventory>,
    fly: Option<Res<FlyCam>>,
    swings: Option<Res<SwingState>>,
    time: Res<Time>,
    mut feedback: ResMut<Feedback>,
    players: Query<(), With<Player>>,
    mut hud: ResMut<HudState>,
) {
    let now = time.elapsed();
    if let Some(swings) = swings
        && swings.swings() != feedback.seen_swings
    {
        feedback.seen_swings = swings.swings();
        feedback.swung_at = Some(now);
    }
    let world = replica.as_deref().map(Replica::world);
    let materials = world
        .map(|w| w.materials().as_ref())
        .or(client_materials.as_deref().map(|m| m.0.as_ref()));
    let crosshair = !players.is_empty();
    let flying = fly.is_some_and(|f| f.enabled);
    let mut next = HudState {
        crosshair,
        inventory: inventory
            .0
            .items()
            .into_iter()
            .map(|(material, count)| HudItem {
                material,
                name: material_name(materials, material),
                count,
            })
            .collect(),
        swing_pulse: feedback
            .swung_at
            .map_or(0.0, |at| fade(now.saturating_sub(at), SWING_PULSE_S)),
        last_break: feedback.broke.and_then(|(cell, material, at)| {
            let fade = fade(now.saturating_sub(at), BREAK_EFFECT_S);
            (fade > 0.0).then_some(BreakEffect {
                cell,
                material,
                fade,
            })
        }),
        ..default()
    };
    let aimed = target.0.filter(|_| crosshair && !flying);
    if let (Some(hit), Some(world)) = (aimed, world)
        && let Some(id) = world.get_cell(hit.cell)
    {
        next.target = Some(hit.cell);
        next.material = Some(material_name(materials, id));
        let dealt = estimate.get(hit.cell);
        if dealt > 0 {
            let hardness = materials.and_then(|m| m.get(id)).map_or(0, |d| d.hardness);
            next.damage = Some(DamageProgress {
                dealt: dealt.min(hardness),
                hardness,
            });
        }
    }
    hud.set_if_neq(next);
}

/// The material key of `id`, or `#<id>` if the table does not have it.
fn material_name(materials: Option<&MaterialTable>, id: MaterialId) -> String {
    materials
        .and_then(|m| m.get(id))
        .map_or_else(|| format!("#{}", id.0), |d| d.key.clone())
}

/// The HUD's nodes, under one full-screen root.
fn spawn_hud(mut commands: Commands) {
    let text = || {
        (
            Text::new(""),
            TextFont::from_font_size(HUD_FONT_SIZE),
            TextColor(Color::WHITE),
            TextShadow {
                offset: Vec2::splat(1.0),
                ..default()
            },
        )
    };
    let arm = |w: f32, h: f32| {
        (
            CrosshairArm { vertical: h > w },
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(-w / 2.0),
                top: Val::Px(-h / 2.0),
                width: Val::Px(w),
                height: Val::Px(h),
                ..default()
            },
            BackgroundColor(Color::WHITE),
        )
    };
    let (len, thick) = (CROSSHAIR_SIZE.x, CROSSHAIR_SIZE.y);
    commands.spawn((
        HudRoot,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        children![
            (
                CrosshairNode,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    ..default()
                },
                Visibility::Hidden,
                children![arm(len, thick), arm(thick, len)],
            ),
            (
                Node {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(24.0),
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(4.0),
                    ..default()
                },
                children![
                    (TargetText, text()),
                    (
                        DamageBar,
                        Node {
                            width: Val::Px(DAMAGE_BAR_SIZE.x),
                            height: Val::Px(DAMAGE_BAR_SIZE.y),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
                        Visibility::Hidden,
                        children![(
                            DamageFill,
                            Node {
                                width: Val::Percent(0.0),
                                height: Val::Percent(100.0),
                                ..default()
                            },
                            BackgroundColor(Color::srgb(0.95, 0.75, 0.2)),
                        )],
                    ),
                ],
            ),
            (
                InventoryText,
                text(),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(8.0),
                    top: Val::Px(8.0),
                    ..default()
                },
            ),
        ],
    ));
}

/// Draws the HUD on the game camera, whatever it renders to (a window, or
/// the test kit's image).
fn attach_to_camera(
    mut commands: Commands,
    roots: Query<Entity, (With<HudRoot>, Without<UiTargetCamera>)>,
    cameras: Query<Entity, With<GameCamera>>,
) {
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    for root in &roots {
        commands.entity(root).insert(UiTargetCamera(camera));
    }
}

/// Puts [`HudState`] on screen.
#[allow(clippy::type_complexity)]
fn show_state(
    hud: Res<HudState>,
    mut crosshair: Query<&mut Visibility, (With<CrosshairNode>, Without<DamageBar>)>,
    mut bar: Query<&mut Visibility, (With<DamageBar>, Without<CrosshairNode>)>,
    mut fill: Query<&mut Node, With<DamageFill>>,
    mut arms: Query<(&CrosshairArm, &mut Node, &mut BackgroundColor), Without<DamageFill>>,
    mut target_text: Query<&mut Text, (With<TargetText>, Without<InventoryText>)>,
    mut inventory_text: Query<&mut Text, (With<InventoryText>, Without<TargetText>)>,
) {
    let pulse = hud.swing_pulse;
    let len = CROSSHAIR_SIZE.x * (1.0 + SWING_PULSE_GROWTH * pulse);
    let color = pulse_color(Color::WHITE, pulse);
    for (arm, mut node, mut background) in &mut arms {
        let (w, h) = if arm.vertical {
            (CROSSHAIR_SIZE.y, len)
        } else {
            (len, CROSSHAIR_SIZE.y)
        };
        node.left = Val::Px(-w / 2.0);
        node.top = Val::Px(-h / 2.0);
        node.width = Val::Px(w);
        node.height = Val::Px(h);
        background.set_if_neq(BackgroundColor(color));
    }
    let shown = |on: bool| {
        if on {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        }
    };
    for mut v in &mut crosshair {
        v.set_if_neq(shown(hud.crosshair));
    }
    for mut v in &mut bar {
        v.set_if_neq(shown(hud.damage.is_some()));
    }
    let percent = hud.damage.map_or(0.0, |d| d.fraction() * 100.0);
    for mut node in &mut fill {
        node.width = Val::Percent(percent);
    }
    for mut text in &mut target_text {
        text.0 = hud.material.clone().unwrap_or_default();
    }
    let lines: Vec<String> = hud
        .inventory
        .iter()
        .map(|item| format!("{} {}", item.name, item.count))
        .collect();
    for mut text in &mut inventory_text {
        text.0 = lines.join("\n");
    }
}

/// `base` blended toward [`SWING_PULSE_COLOR`] by `pulse` (0.0..=1.0).
fn pulse_color(base: Color, pulse: f32) -> Color {
    base.mix(&SWING_PULSE_COLOR, pulse)
}

/// Outlines the targeted cell; a swing makes it grow and light up.
fn draw_outline(hud: Res<HudState>, mut gizmos: Gizmos) {
    if let Some(cell) = hud.target {
        let scale = OUTLINE_SCALE + OUTLINE_PULSE_GROWTH * hud.swing_pulse;
        gizmos.cube(
            Transform::from_translation(cell.center_m())
                .with_scale(Vec3::splat(CELL_SIZE_M * scale)),
            pulse_color(OUTLINE_COLOR, hud.swing_pulse),
        );
    }
}

/// The break effect: a flash outline growing out of the cell, and
/// [`BREAK_CHIPS`] chips of the cell's material thrown up and out from its
/// center, all fading out.
fn draw_break(
    hud: Res<HudState>,
    replica: Option<Res<Replica>>,
    client_materials: Option<Res<ClientMaterials>>,
    mut gizmos: Gizmos,
) {
    let Some(effect) = hud.last_break else {
        return;
    };
    let materials = replica
        .as_deref()
        .map(|r| r.world().materials().as_ref())
        .or(client_materials.as_deref().map(|m| m.0.as_ref()));
    let [r, g, b, _] = materials
        .and_then(|m| m.get(effect.material))
        .map_or([255; 4], |d| d.color);
    let chip_color = Color::srgba_u8(r, g, b, u8::MAX).with_alpha(effect.fade);
    let age = BREAK_EFFECT_S * (1.0 - effect.fade);
    let center = effect.cell.center_m();
    let flash = CELL_SIZE_M * (OUTLINE_SCALE + BREAK_FLASH_GROWTH * (1.0 - effect.fade));
    gizmos.cube(
        Transform::from_translation(center).with_scale(Vec3::splat(flash)),
        Color::WHITE.with_alpha(effect.fade),
    );
    for i in 0..BREAK_CHIPS {
        // Evenly around the vertical axis, alternately steep and shallow.
        let angle = std::f32::consts::TAU * i as f32 / BREAK_CHIPS as f32;
        let rise = if i % 2 == 0 { 1.0 } else { 0.4 };
        let velocity = Vec3::new(angle.cos(), rise, angle.sin()).normalize() * CHIP_SPEED_M_S;
        let pos = center + velocity * age + Vec3::NEG_Y * (CHIP_GRAVITY_M_S2 * age * age / 2.0);
        gizmos.cube(
            Transform::from_translation(pos)
                .with_rotation(Quat::from_rotation_y(angle + age * CHIP_SPEED_M_S))
                .with_scale(Vec3::splat(CHIP_SIZE_M)),
            chip_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use bevy::input::mouse::AccumulatedMouseMotion;
    use bevy::time::TimeUpdateStrategy;
    use rmc_world::ToolProfile;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::CellAabb;
    use rmc_world::edit::{CommandId, EditCommand, EditResult, EditStatus, PlayerId};
    use rmc_world::shape::Shape;
    use rmc_world::world::World;

    use crate::controller::ControllerPlugin;
    use crate::net::NetPlugin;
    use crate::pickaxe::{Pickaxe, PickaxePlugin, SWING_BUTTON};
    use crate::replica::ReplicaPlugin;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const TOOLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tools.ron");

    /// The wall cell straight ahead of the eye (see [`world`] and [`FEET`]).
    const WALL_CELL: CellPos = CellPos::new(32, 7, 27);

    fn materials() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"))
    }

    fn profile() -> ToolProfile {
        rmc_world::ToolTable::load(TOOLS_PATH)
            .expect("assets/tools.ron loads")
            .pickaxe
    }

    /// A 64³-cell world: a floor of `floor` in cell rows y = 0..=3 (top at
    /// 2.0 m), and a one-cell-thick wall of `wall` filling cell row z = 27
    /// from the floor up to y = 15. Its +Z face is at 14.0 m.
    fn world(floor: &str, wall: &str) -> World {
        let materials = materials();
        let (floor, wall) = (materials.id(floor).unwrap(), materials.id(wall).unwrap());
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 63));
        let mut world = World::new(materials, bounds);
        for pos in bounds.chunks() {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        for (min, max, id) in [
            (CellPos::new(0, 0, 0), CellPos::new(63, 3, 63), floor),
            (CellPos::new(0, 4, 27), CellPos::new(63, 15, 27), wall),
        ] {
            let cmd = EditCommand {
                id: CommandId::default(),
                issuer: PlayerId(0),
                op: EditOp::Set(id),
                shape: Shape::Box { min, max },
            };
            assert_eq!(world.apply(&cmd).status, EditStatus::Applied);
        }
        world
    }

    /// Feet in the middle of cell column x = 32, z = 32 on the floor, 2 m
    /// from the wall; the eye (3.55 m) is in cell row y = 7.
    const FEET: Vec3 = Vec3::new(16.25, 2.0, 16.0);

    /// The HUD with the pickaxe, the controller and the replica's message
    /// handler, at 60 frames per second, the replica holding `world` and a
    /// player standing at [`FEET`] looking along -Z. Host messages are
    /// written straight into the app as [`FromHost`].
    struct Rig {
        app: App,
        player: Entity,
    }

    impl Rig {
        fn new(world: World) -> Self {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                    1.0 / 60.0,
                )))
                .init_resource::<ButtonInput<KeyCode>>()
                .init_resource::<ButtonInput<MouseButton>>()
                .init_resource::<AccumulatedMouseMotion>()
                .init_resource::<FlyCam>()
                .insert_resource(ClientMaterials(materials()))
                .insert_resource(Replica::from_world(world))
                .insert_resource(Pickaxe(profile()))
                .add_plugins((
                    NetPlugin,
                    ReplicaPlugin,
                    ControllerPlugin,
                    PickaxePlugin,
                    HudPlugin,
                ));
            let player = app.world_mut().spawn(Player::at(FEET)).id();
            let mut rig = Self { app, player };
            rig.frames(1);
            rig
        }

        fn frames(&mut self, n: u32) {
            for _ in 0..n {
                self.app.update();
            }
        }

        fn hud(&self) -> HudState {
            self.app.world().resource::<HudState>().clone()
        }

        fn player_mut(&mut self) -> Mut<'_, Player> {
            self.app.world_mut().get_mut::<Player>(self.player).unwrap()
        }

        /// Waits out the swing interval, holds the swing button for exactly
        /// `n` swings (one per interval), then releases it.
        fn swing(&mut self, n: u32) {
            let interval = (profile().interval_s * 60.0).round() as u32;
            self.frames(interval);
            let before = self
                .app
                .world()
                .resource::<crate::pickaxe::SwingState>()
                .swings();
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(SWING_BUTTON);
            self.frames((n - 1) * interval + 1);
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .release(SWING_BUTTON);
            self.frames(1);
            let after = self
                .app
                .world()
                .resource::<crate::pickaxe::SwingState>()
                .swings();
            assert_eq!(after - before, u64::from(n), "swung {n} times");
        }

        /// Delivers `msgs` as if the host had sent them, and runs a frame.
        fn deliver(&mut self, msgs: impl IntoIterator<Item = ServerMsg>) {
            for msg in msgs {
                self.app.world_mut().write_message(FromHost(msg));
            }
            self.frames(1);
        }

        fn replica_world(&self) -> &World {
            self.app.world().resource::<Replica>().world()
        }
    }

    fn damage(dealt: u16, hardness: u16) -> Option<DamageProgress> {
        Some(DamageProgress { dealt, hardness })
    }

    /// What the host would answer to `n` swings at `cell`: the result of
    /// each `Damage` edit applied to `host`, and an `Inventory` message
    /// after each one that yields, as the host sends them (SAD §8.5).
    fn host_answers(
        host: &mut World,
        inventory: &mut Inventory,
        cell: CellPos,
        n: u32,
    ) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        for _ in 0..n {
            let result: EditResult = host.apply(&EditCommand {
                id: CommandId::default(),
                issuer: PlayerId(1),
                op: EditOp::Damage(profile().damage),
                shape: Shape::Cell(cell),
            });
            let yields = result.yields.clone();
            out.push(ServerMsg::Applied(result));
            if !yields.is_empty() {
                inventory.add_all(&yields);
                out.push(ServerMsg::Inventory {
                    items: inventory.items(),
                });
            }
        }
        out
    }

    #[test]
    fn hudstate_reflects_target() {
        let mut host = world("granite", "stone");
        let mut rig = Rig::new(world("granite", "stone"));
        let stone = rig.replica_world().materials().id("stone").unwrap();
        let hardness = rig.replica_world().materials().get(stone).unwrap().hardness;
        let dmg = profile().damage;
        assert!(hardness > 2 * dmg, "stone takes more than two swings");

        // Looking at the wall: crosshair, outline on the wall cell, its name,
        // no damage yet.
        assert_eq!(
            rig.hud(),
            HudState {
                crosshair: true,
                target: Some(WALL_CELL),
                material: Some("stone".into()),
                damage: None,
                inventory: vec![],
                swing_pulse: 0.0,
                last_break: None,
            }
        );

        // Each swing adds the profile's damage to the progress.
        rig.swing(1);
        assert_eq!(rig.hud().damage, damage(dmg, hardness));
        rig.swing(1);
        assert_eq!(rig.hud().damage, damage(2 * dmg, hardness));
        assert_eq!(rig.hud().target, Some(WALL_CELL));

        // Looking down: the floor cell, another material, no damage on it.
        rig.player_mut().pitch = -1.0;
        rig.frames(1);
        let hud = rig.hud();
        let floor = hud.target.expect("the floor is in reach");
        assert_eq!(floor.0.y, 3);
        assert_eq!(hud.material.as_deref(), Some("granite"));
        assert_eq!(hud.damage, None);
        assert!(hud.crosshair);

        // At the sky: nothing targeted, the crosshair stays.
        rig.player_mut().pitch = 1.2;
        rig.frames(1);
        let hud = rig.hud();
        assert_eq!((hud.target, hud.material, hud.damage), (None, None, None));
        assert!(hud.crosshair);

        // Back at the wall: the damage sent before is still shown.
        rig.player_mut().pitch = 0.0;
        rig.frames(1);
        assert_eq!(rig.hud().damage, damage(2 * dmg, hardness));

        // The host breaks the cell: the replica cell is air and its damage
        // is forgotten. The wall was one cell thick, so nothing is in reach.
        let swings = u32::from(hardness.div_ceil(dmg));
        let answers = host_answers(&mut host, &mut Inventory::new(), WALL_CELL, swings);
        rig.swing(swings - 2);
        assert_eq!(rig.hud().damage, damage(hardness, hardness), "capped");
        rig.deliver(answers);
        assert_eq!(
            rig.replica_world().get_cell(WALL_CELL),
            Some(MaterialId::AIR)
        );
        let hud = rig.hud();
        assert_eq!((hud.target, hud.material, hud.damage), (None, None, None));
        assert!(rig.app.world().resource::<DamageEstimate>().is_empty());

        // Damage at the sky or past reach sends nothing and estimates nothing.
        rig.swing_at_nothing();

        // In fly mode the pickaxe is off: no target is shown.
        let mut rig = Rig::new(world("granite", "stone"));
        rig.app.world_mut().resource_mut::<FlyCam>().enabled = true;
        rig.frames(1);
        let hud = rig.hud();
        assert_eq!((hud.target, hud.material), (None, None));

        // Without a player there is no crosshair.
        let mut rig = Rig::new(world("granite", "stone"));
        let player = rig.player;
        rig.app.world_mut().despawn(player);
        rig.frames(1);
        assert_eq!(rig.hud(), HudState::default());
    }

    impl Rig {
        /// Holds the swing button for a while with nothing in reach.
        fn swing_at_nothing(&mut self) {
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(SWING_BUTTON);
            self.frames(30);
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .release(SWING_BUTTON);
            self.frames(1);
            assert!(self.app.world().resource::<DamageEstimate>().is_empty());
            assert_eq!(self.hud().damage, None);
        }
    }

    #[test]
    fn inventory_count_updates_on_applied() {
        let mut host = world("stone", "stone");
        let mut rig = Rig::new(world("stone", "stone"));
        let stone = rig.replica_world().materials().id("stone").unwrap();
        let hardness = rig.replica_world().materials().get(stone).unwrap().hardness;
        let swings = u32::from(hardness.div_ceil(profile().damage));
        let mut held = Inventory::new();
        assert!(rig.hud().inventory.is_empty());

        // Results that break nothing yield nothing; the count stays.
        let mut answers = host_answers(&mut host, &mut held, WALL_CELL, swings);
        assert!(matches!(answers.last(), Some(ServerMsg::Inventory { .. })));
        let last = answers.split_off(answers.len() - 2);
        rig.deliver(answers);
        assert!(rig.hud().inventory.is_empty());

        // The breaking result and the host's new totals: one stone.
        rig.deliver(last);
        let line = |count| HudItem {
            material: stone,
            name: "stone".into(),
            count,
        };
        assert_eq!(rig.hud().inventory, vec![line(1)]);
        assert_eq!(
            rig.app.world().resource::<ClientInventory>().0.count(stone),
            1
        );

        // A second cell broken: two. The counts are the host's totals; the
        // yields in Applied are not added on top of them.
        let next = CellPos::new(33, 7, 27);
        rig.deliver(host_answers(&mut host, &mut held, next, swings));
        assert_eq!(rig.hud().inventory, vec![line(2)]);

        // Another material joins, listed after stone (by id), and a later
        // Inventory message replaces the totals.
        let glow = rig.replica_world().materials().id("glowcrystal").unwrap();
        assert!(glow > stone);
        rig.deliver([ServerMsg::Inventory {
            items: vec![(glow, 4), (stone, 2)],
        }]);
        assert_eq!(
            rig.hud().inventory,
            vec![
                line(2),
                HudItem {
                    material: glow,
                    name: "glowcrystal".into(),
                    count: 4,
                }
            ]
        );

        // The inventory text shows one line per material.
        let text = rig
            .app
            .world_mut()
            .query_filtered::<&Text, With<InventoryText>>()
            .single(rig.app.world())
            .unwrap()
            .0
            .clone();
        assert_eq!(text, "stone 2\nglowcrystal 4");

        // A new Welcome starts with an empty inventory.
        let bounds = rig.replica_world().bounds();
        rig.deliver([ServerMsg::Welcome {
            player: PlayerId(1),
            seed: 0,
            bounds,
            gen_params_hash: 0,
        }]);
        assert!(rig.hud().inventory.is_empty());
        assert!(rig.app.world().resource::<ClientInventory>().0.is_empty());
    }
}
