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
//! - Selection (owner extension, F093): the material right mouse places
//!   ([`SelectedMaterial`]). Nothing is selected at first. The mouse wheel
//!   (down: next, up: previous) and [`NEXT_SLOT_KEY`] / [`PREV_SLOT_KEY`]
//!   step through the held materials in inventory order, wrapping. A
//!   selected material that runs out stays selected (the host refuses to
//!   place it) until the next step, which goes to the held material after
//!   it. The inventory line of the selected material is marked with `> `.
//!   While the editor is open, the wheel and keys leave it alone. `Welcome`
//!   clears it.
//! - Swing feedback: every swing the pickaxe sends ([`SwingState`]) starts a
//!   pulse of [`SWING_PULSE_S`]: the crosshair arms grow and warm, and the
//!   outline grows and lights up, both fading back.
//! - Break feedback: when an `Applied` delta turns a solid cell into a
//!   non-solid one, and that cell was the targeted cell or one this client
//!   has damaged, the cell gets a break effect of [`BREAK_EFFECT_S`]: a
//!   flash outline that closes into the hole and fades, and [`BREAK_CHIPS`]
//!   chips flying out ([`BreakChip`]: small solid cubes in the cell's
//!   material color, lit and depth-tested like the terrain, despawned when
//!   the effect ends). The material is read from the replica before the
//!   delta applies. `Full` deltas (big edits, never a pickaxe swing) and
//!   snapshots show no effect.
//! - Damage on the cell (owner extension, F096): while the targeted cell
//!   shows damage, a [`CrackOverlay`] cube just outside its faces darkens
//!   them by the damage fraction; it is gone when the damage is.
//! - Depth: the outline and the flash are [`HudGizmos`] lines, whose config
//!   has no depth bias ([`HUD_GIZMO_DEPTH_BIAS`]), so nearer geometry hides
//!   them, and the flash never reaches out of the cell toward the camera
//!   ([`flash_scale`]). Chips and the overlay are meshes, so they need an
//!   app with mesh and material assets; the lines need Bevy's gizmos.

use std::collections::BTreeMap;
use std::time::Duration;

use bevy::gizmos::GizmoPlugin;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::ui::UiTargetCamera;
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::coords::{CELL_SIZE_M, CellPos, ChunkPos, LocalPos};
use rmc_world::edit::{ChunkDelta, EditOp};
use rmc_world::inventory::Inventory;
use rmc_world::materials::{MaterialId, MaterialTable};

use crate::controller::Player;
use crate::editor::EditorState;
use crate::flycam::FlyCam;
use crate::net::{ClientMaterials, FromHost, NetSystems, ToHost};
use crate::pickaxe::{PickTarget, PickaxeSystems, SwingState};
use crate::render::{GameCamera, chunk_material};
use crate::replica::{Replica, apply_host_messages};

/// Selects the next held material (as does the mouse wheel turned down).
pub const NEXT_SLOT_KEY: KeyCode = KeyCode::KeyE;

/// Selects the previous held material (as does the mouse wheel turned up).
pub const PREV_SLOT_KEY: KeyCode = KeyCode::KeyQ;

/// The prefix of the selected material's inventory line.
pub const SELECTED_MARK: &str = "> ";

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

/// How much the break flash outline shrinks over the effect, relative to
/// the cell: it starts at the outline's size and closes into the hole. It
/// used to grow, and at close range its near edges then crossed the open
/// air right in front of the eye as lines over the whole view (pt-725).
pub const BREAK_FLASH_SHRINK: f32 = 0.5;

/// The break flash outline's size relative to the cell at `fade` (of
/// [`BreakEffect::fade`]): [`OUTLINE_SCALE`] at the break, never larger.
pub fn flash_scale(fade: f32) -> f32 {
    OUTLINE_SCALE - BREAK_FLASH_SHRINK * (1.0 - fade.clamp(0.0, 1.0))
}

/// Number of chips a breaking cell throws.
pub const BREAK_CHIPS: usize = 8;

/// A chip's edge length, in meters.
pub const CHIP_SIZE_M: f32 = 0.1;

/// A chip's initial speed, in meters per second.
pub const CHIP_SPEED_M_S: f32 = 2.0;

/// The downward acceleration on chips, in meters per second squared.
pub const CHIP_GRAVITY_M_S2: f32 = 9.8;

/// Chips shrink to nothing over the last part of the effect: from this
/// fade (of [`BreakEffect::fade`]) down to 0.
pub const CHIP_SHRINK_FADE: f32 = 0.25;

/// The depth bias of the [`HudGizmos`] lines. 0 is no bias toward the
/// camera: they are depth-tested like the terrain, so nearer geometry hides
/// them (a negative bias pulls them in front, -1 over everything).
pub const HUD_GIZMO_DEPTH_BIAS: f32 = 0.0;

/// The crack overlay's size relative to the cell: just outside its faces,
/// so it covers them without fighting them for depth.
pub const CRACK_OVERLAY_SCALE: f32 = 1.004;

/// How much the crack overlay darkens a cell about to break (damage
/// fraction 1.0): its faces keep `1 - CRACK_DARKEN` of their (linear)
/// light.
pub const CRACK_DARKEN: f32 = 0.7;

/// The gizmo group of the targeted cell's outline and the break flash.
/// Its config ([`HUD_GIZMO_DEPTH_BIAS`]) keeps them depth-tested.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct HudGizmos;

/// One chip thrown by a breaking cell: a small solid cube of the cell's
/// material ([`CHIP_SIZE_M`]), lit and depth-tested like the terrain. It
/// lives for [`BREAK_EFFECT_S`] after the break.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct BreakChip {
    /// The cell that broke.
    pub cell: CellPos,
    /// Which of the [`BREAK_CHIPS`] chips it is: sets its direction.
    pub index: usize,
    /// When the cell broke, in `Time` elapsed.
    pub born: Duration,
}

/// The darkening on the targeted cell's faces while it is damaged: a cube
/// just outside the cell ([`CRACK_OVERLAY_SCALE`]) whose multiply-blended
/// color ([`crack_color`]) darkens what is behind it.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct CrackOverlay {
    /// The damaged cell.
    pub cell: CellPos,
    /// The HUD's damage fraction on it, in `0.0..=1.0`.
    pub strength: f32,
}

/// The crack overlay's color for `strength`: white (no change) at 0,
/// darkening by [`CRACK_DARKEN`] at 1.
pub fn crack_color(strength: f32) -> Color {
    let keep = 1.0 - CRACK_DARKEN * strength.clamp(0.0, 1.0);
    Color::linear_rgb(keep, keep, keep)
}

/// Chip `index`'s transform `age_s` seconds after the cell centered at
/// `center` broke: thrown evenly around the vertical axis, alternately
/// steep and shallow, at [`CHIP_SPEED_M_S`], falling under
/// [`CHIP_GRAVITY_M_S2`] and spinning. Full size ([`CHIP_SIZE_M`]) until
/// the fade reaches [`CHIP_SHRINK_FADE`], then shrinking to nothing.
pub fn chip_transform(center: Vec3, index: usize, age_s: f32) -> Transform {
    let angle = std::f32::consts::TAU * index as f32 / BREAK_CHIPS as f32;
    let rise = if index.is_multiple_of(2) { 1.0 } else { 0.4 };
    let velocity = Vec3::new(angle.cos(), rise, angle.sin()).normalize() * CHIP_SPEED_M_S;
    let pos = center + velocity * age_s + Vec3::NEG_Y * (CHIP_GRAVITY_M_S2 * age_s * age_s / 2.0);
    let fade = (1.0 - age_s / BREAK_EFFECT_S).max(0.0);
    let size = CHIP_SIZE_M * (fade / CHIP_SHRINK_FADE).min(1.0);
    Transform::from_translation(pos)
        .with_rotation(Quat::from_rotation_y(angle + age_s * CHIP_SPEED_M_S))
        .with_scale(Vec3::splat(size))
}

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
    /// The material right mouse places ([`SelectedMaterial`]).
    pub selected: Option<MaterialId>,
}

/// The material right mouse places, if one is selected. It may no longer
/// be held.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectedMaterial(pub Option<MaterialId>);

/// The held material one step from `current` in inventory order, wrapping:
/// the first one after it (`forward`) or the last one before it. With
/// nothing selected, the first or the last. `None` when nothing is held.
pub fn step_selection(
    inventory: &Inventory,
    current: Option<MaterialId>,
    forward: bool,
) -> Option<MaterialId> {
    let held: Vec<MaterialId> = inventory.items().into_iter().map(|(id, _)| id).collect();
    let (first, last) = (held.first().copied(), held.last().copied());
    let Some(current) = current else {
        return if forward { first } else { last };
    };
    if forward {
        held.iter().copied().find(|&id| id > current).or(first)
    } else {
        held.iter().rev().copied().find(|&id| id < current).or(last)
    }
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
            .init_resource::<SelectedMaterial>()
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
                    select_material,
                    update_state,
                    show_state.run_if(resource_changed::<HudState>),
                )
                    .chain()
                    .in_set(HudSystems::Show)
                    .after(PickaxeSystems::Target),
            )
            // Chips and the crack overlay need mesh and material assets,
            // which a logic-only app does not have; they do nothing there.
            .init_resource::<HudMeshes>()
            .add_systems(
                Update,
                (spawn_chips, move_chips, show_crack)
                    .chain()
                    .after(HudSystems::Show),
            );
        // A logic-only app has no gizmos.
        if app.is_plugin_added::<GizmoPlugin>() {
            app.insert_gizmo_config(
                HudGizmos,
                GizmoConfig {
                    depth_bias: HUD_GIZMO_DEPTH_BIAS,
                    ..default()
                },
            )
            .add_systems(Update, (draw_outline, draw_break).after(HudSystems::Show));
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
    mut selected: ResMut<SelectedMaterial>,
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
                selected.set_if_neq(SelectedMaterial(None));
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

/// Steps the selection on this frame's wheel turn (one step per frame,
/// whatever the amount) and on [`NEXT_SLOT_KEY`] / [`PREV_SLOT_KEY`]
/// presses. Not while the editor is open.
fn select_material(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    editor: Option<Res<EditorState>>,
    inventory: Res<ClientInventory>,
    mut selected: ResMut<SelectedMaterial>,
) {
    if editor.is_some_and(|e| e.open) {
        return;
    }
    let wheel = scroll.map_or(0.0, |s| s.delta.y);
    let mut steps = Vec::new();
    if wheel != 0.0 {
        steps.push(wheel < 0.0);
    }
    if let Some(keys) = keys {
        if keys.just_pressed(NEXT_SLOT_KEY) {
            steps.push(true);
        }
        if keys.just_pressed(PREV_SLOT_KEY) {
            steps.push(false);
        }
    }
    let mut current = selected.0;
    for forward in steps {
        current = step_selection(&inventory.0, current, forward).or(current);
    }
    selected.set_if_neq(SelectedMaterial(current));
}

/// Recomputes [`HudState`].
#[allow(clippy::too_many_arguments)]
fn update_state(
    target: Res<PickTarget>,
    replica: Option<Res<Replica>>,
    client_materials: Option<Res<ClientMaterials>>,
    estimate: Res<DamageEstimate>,
    inventory: Res<ClientInventory>,
    selected: Res<SelectedMaterial>,
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
        selected: selected.0,
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
        .map(|item| {
            let mark = if hud.selected == Some(item.material) {
                SELECTED_MARK
            } else {
                ""
            };
            format!("{mark}{} {}", item.name, item.count)
        })
        .collect();
    for mut text in &mut inventory_text {
        text.0 = lines.join("\n");
    }
}

/// `base` blended toward [`SWING_PULSE_COLOR`] by `pulse` (0.0..=1.0).
fn pulse_color(base: Color, pulse: f32) -> Color {
    base.mix(&SWING_PULSE_COLOR, pulse)
}

/// The shared chip and overlay cube mesh (unit edge), and one chip
/// material per broken material, made on first use.
#[derive(Resource, Debug, Default)]
struct HudMeshes {
    cube: Option<Handle<Mesh>>,
    chips: BTreeMap<MaterialId, Handle<StandardMaterial>>,
}

impl HudMeshes {
    fn cube(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.cube
            .get_or_insert_with(|| meshes.add(Cuboid::from_length(1.0)))
            .clone()
    }

    /// The chip material for `id`: its color, shaded like the terrain.
    fn chip(
        &mut self,
        id: MaterialId,
        table: Option<&MaterialTable>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.chips
            .entry(id)
            .or_insert_with(|| {
                let [r, g, b, _] = table.and_then(|m| m.get(id)).map_or([255; 4], |d| d.color);
                materials.add(StandardMaterial {
                    base_color: Color::srgba_u8(r, g, b, u8::MAX),
                    ..chunk_material()
                })
            })
            .clone()
    }
}

/// The material table: the replica's, or the client's before a replica.
fn material_table<'a>(
    replica: Option<&'a Replica>,
    client_materials: Option<&'a ClientMaterials>,
) -> Option<&'a MaterialTable> {
    replica
        .map(|r| r.world().materials().as_ref())
        .or(client_materials.map(|m| m.0.as_ref()))
}

/// Spawns [`BREAK_CHIPS`] [`BreakChip`]s when a cell breaks.
#[allow(clippy::too_many_arguments)]
fn spawn_chips(
    mut commands: Commands,
    feedback: Res<Feedback>,
    replica: Option<Res<Replica>>,
    client_materials: Option<Res<ClientMaterials>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    mut handles: ResMut<HudMeshes>,
    mut spawned_for: Local<Option<Duration>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    let Some((cell, material, at)) = feedback.broke else {
        return;
    };
    if *spawned_for == Some(at) {
        return;
    }
    *spawned_for = Some(at);
    let table = material_table(replica.as_deref(), client_materials.as_deref());
    let mesh = handles.cube(&mut meshes);
    let chip_material = handles.chip(material, table, &mut materials);
    for index in 0..BREAK_CHIPS {
        commands.spawn((
            BreakChip {
                cell,
                index,
                born: at,
            },
            Mesh3d(mesh.clone()),
            MeshMaterial3d(chip_material.clone()),
            chip_transform(cell.center_m(), index, 0.0),
            NotShadowCaster,
        ));
    }
}

/// Moves the chips along [`chip_transform`]; despawns them when the effect
/// ends.
fn move_chips(
    mut commands: Commands,
    time: Res<Time>,
    mut chips: Query<(Entity, &BreakChip, &mut Transform)>,
) {
    let now = time.elapsed();
    for (entity, chip, mut transform) in &mut chips {
        let age_s = now.saturating_sub(chip.born).as_secs_f32();
        if age_s >= BREAK_EFFECT_S {
            commands.entity(entity).despawn();
        } else {
            *transform = chip_transform(chip.cell.center_m(), chip.index, age_s);
        }
    }
}

/// Keeps one [`CrackOverlay`] on the targeted cell while [`HudState`] shows
/// damage on it, with the damage fraction as its strength; despawns it
/// when there is none.
fn show_crack(
    mut commands: Commands,
    hud: Res<HudState>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    mut handles: ResMut<HudMeshes>,
    mut overlays: Query<(
        Entity,
        &mut CrackOverlay,
        &mut Transform,
        &MeshMaterial3d<StandardMaterial>,
    )>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    let shown = hud
        .target
        .zip(hud.damage)
        .map(|(cell, damage)| (cell, damage.fraction()));
    let Some((cell, strength)) = shown else {
        for (entity, ..) in &overlays {
            commands.entity(entity).despawn();
        }
        return;
    };
    let transform = Transform::from_translation(cell.center_m())
        .with_scale(Vec3::splat(CELL_SIZE_M * CRACK_OVERLAY_SCALE));
    if let Some((_, mut overlay, mut at, material)) = overlays.iter_mut().next() {
        let next = CrackOverlay { cell, strength };
        if *overlay != next {
            *overlay = next;
            *at = transform;
            if let Some(mut m) = materials.get_mut(&material.0) {
                m.base_color = crack_color(strength);
            }
        }
        return;
    }
    let material = materials.add(StandardMaterial {
        base_color: crack_color(strength),
        alpha_mode: AlphaMode::Multiply,
        unlit: true,
        fog_enabled: false,
        ..default()
    });
    commands.spawn((
        CrackOverlay { cell, strength },
        Mesh3d(handles.cube(&mut meshes)),
        MeshMaterial3d(material),
        transform,
        NotShadowCaster,
    ));
}

/// Outlines the targeted cell; a swing makes it grow and light up.
fn draw_outline(hud: Res<HudState>, mut gizmos: Gizmos<HudGizmos>) {
    if let Some(cell) = hud.target {
        let scale = OUTLINE_SCALE + OUTLINE_PULSE_GROWTH * hud.swing_pulse;
        gizmos.cube(
            Transform::from_translation(cell.center_m())
                .with_scale(Vec3::splat(CELL_SIZE_M * scale)),
            pulse_color(OUTLINE_COLOR, hud.swing_pulse),
        );
    }
}

/// The break flash: an outline closing into the broken cell and fading.
/// The chips are meshes ([`BreakChip`]).
fn draw_break(hud: Res<HudState>, mut gizmos: Gizmos<HudGizmos>) {
    let Some(effect) = hud.last_break else {
        return;
    };
    let flash = CELL_SIZE_M * flash_scale(effect.fade);
    gizmos.cube(
        Transform::from_translation(effect.cell.center_m()).with_scale(Vec3::splat(flash)),
        Color::WHITE.with_alpha(effect.fade),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
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
            Self::build(world, false)
        }

        /// As [`Rig::new`], with what a rendering app gives the HUD: mesh
        /// and material assets, and Bevy's gizmos.
        fn rendering(world: World) -> Self {
            Self::build(world, true)
        }

        fn build(world: World, rendering: bool) -> Self {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins);
            if rendering {
                app.add_plugins((AssetPlugin::default(), GizmoPlugin))
                    .init_asset::<Mesh>()
                    .init_asset::<StandardMaterial>()
                    .init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>();
            }
            app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
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
                selected: None,
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

    /// The inventory text as drawn.
    fn inventory_text(rig: &mut Rig) -> String {
        rig.app
            .world_mut()
            .query_filtered::<&Text, With<InventoryText>>()
            .single(rig.app.world())
            .unwrap()
            .0
            .clone()
    }

    /// One frame with the wheel turned by `y` notches (positive is up).
    fn wheel(rig: &mut Rig, y: f32) {
        rig.app
            .world_mut()
            .resource_mut::<AccumulatedMouseScroll>()
            .delta = Vec2::new(0.0, y);
        rig.frames(1);
        rig.app
            .world_mut()
            .resource_mut::<AccumulatedMouseScroll>()
            .delta = Vec2::ZERO;
        rig.frames(1);
    }

    /// One frame with `key` just pressed, then one with it released.
    fn tap(rig: &mut Rig, key: KeyCode) {
        rig.app
            .world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        rig.frames(1);
        let mut keys = rig.app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(key);
        keys.clear();
        rig.frames(1);
    }

    #[test]
    fn selected_slot_cycles_with_wheel_and_shows_in_hudstate() {
        let mut rig = Rig::new(world("stone", "stone"));
        rig.app.init_resource::<AccumulatedMouseScroll>();
        let m = rig.replica_world().materials().clone();
        let (dirt, stone, copper) = (
            m.id("dirt").unwrap(),
            m.id("stone").unwrap(),
            m.id("copper_ore").unwrap(),
        );
        assert!(dirt < stone && stone < copper);
        assert_eq!(
            (NEXT_SLOT_KEY, PREV_SLOT_KEY),
            (KeyCode::KeyE, KeyCode::KeyQ)
        );

        // Nothing held: nothing to select.
        assert_eq!(rig.hud().selected, None);
        wheel(&mut rig, -1.0);
        tap(&mut rig, NEXT_SLOT_KEY);
        assert_eq!(rig.hud().selected, None);

        // Holding materials selects nothing by itself.
        rig.deliver([ServerMsg::Inventory {
            items: vec![(dirt, 2), (stone, 1), (copper, 4)],
        }]);
        assert_eq!(rig.hud().selected, None);
        assert_eq!(inventory_text(&mut rig), "dirt 2\nstone 1\ncopper_ore 4");

        // Wheel down steps forward in inventory order, wrapping; wheel up
        // steps back.
        let mut seen = Vec::new();
        for _ in 0..4 {
            wheel(&mut rig, -1.0);
            seen.push(rig.hud().selected);
        }
        assert_eq!(seen, [Some(dirt), Some(stone), Some(copper), Some(dirt)]);
        wheel(&mut rig, 1.0);
        assert_eq!(rig.hud().selected, Some(copper));
        assert_eq!(
            rig.app.world().resource::<SelectedMaterial>().0,
            Some(copper),
            "HudState mirrors the selection"
        );

        // E and Q do the same.
        tap(&mut rig, NEXT_SLOT_KEY);
        assert_eq!(rig.hud().selected, Some(dirt));
        tap(&mut rig, PREV_SLOT_KEY);
        tap(&mut rig, PREV_SLOT_KEY);
        assert_eq!(rig.hud().selected, Some(stone));

        // The selected line is marked.
        assert_eq!(inventory_text(&mut rig), "dirt 2\n> stone 1\ncopper_ore 4");

        // The last stone spent: still selected (a place of it is refused by
        // the host), no line marked, and the next step skips it.
        rig.deliver([ServerMsg::Inventory {
            items: vec![(dirt, 2), (copper, 4)],
        }]);
        assert_eq!(rig.hud().selected, Some(stone));
        assert_eq!(inventory_text(&mut rig), "dirt 2\ncopper_ore 4");
        wheel(&mut rig, -1.0);
        assert_eq!(rig.hud().selected, Some(copper));
        assert_eq!(inventory_text(&mut rig), "dirt 2\n> copper_ore 4");

        // With the editor open the wheel and keys leave the selection alone.
        rig.app
            .world_mut()
            .insert_resource(crate::editor::EditorState {
                open: true,
                ..default()
            });
        wheel(&mut rig, -1.0);
        tap(&mut rig, NEXT_SLOT_KEY);
        assert_eq!(rig.hud().selected, Some(copper));
        rig.app
            .world_mut()
            .resource_mut::<crate::editor::EditorState>()
            .open = false;
        wheel(&mut rig, -1.0);
        assert_eq!(rig.hud().selected, Some(dirt));

        // A new Welcome empties the inventory and the selection.
        let bounds = rig.replica_world().bounds();
        rig.deliver([ServerMsg::Welcome {
            player: PlayerId(1),
            seed: 0,
            bounds,
            gen_params_hash: 0,
        }]);
        assert_eq!(rig.hud().selected, None);
    }

    /// Line vertices in all gizmo assets: what the gizmos drew last frame.
    fn gizmo_vertices(rig: &Rig) -> usize {
        rig.app
            .world()
            .resource::<Assets<GizmoAsset>>()
            .iter()
            .map(|(_, g)| g.buffer().list_positions.len() + g.buffer().strip_positions.len())
            .sum()
    }

    /// The chips: their component, mesh, material and transform.
    fn chips(rig: &mut Rig) -> Vec<(BreakChip, Handle<Mesh>, Handle<StandardMaterial>, Transform)> {
        let world = rig.app.world_mut();
        let mut chips: Vec<_> = world
            .query::<(
                &BreakChip,
                &Mesh3d,
                &MeshMaterial3d<StandardMaterial>,
                &Transform,
            )>()
            .iter(world)
            .map(|(c, m, mat, t)| (*c, m.0.clone(), mat.0.clone(), *t))
            .collect();
        chips.sort_by_key(|c| c.0.index);
        chips
    }

    /// The crack overlays: their component, material and transform.
    fn cracks(rig: &mut Rig) -> Vec<(CrackOverlay, Handle<StandardMaterial>, Transform)> {
        let world = rig.app.world_mut();
        world
            .query::<(&CrackOverlay, &MeshMaterial3d<StandardMaterial>, &Transform)>()
            .iter(world)
            .map(|(c, mat, t)| (*c, mat.0.clone(), *t))
            .collect()
    }

    #[test]
    fn break_chips_are_depth_tested_meshes() {
        let mut host = world("granite", "stone");
        let mut rig = Rig::rendering(world("granite", "stone"));
        let m = rig.replica_world().materials().clone();
        let stone = m.get(m.id("stone").unwrap()).unwrap().clone();

        // Before the break only the outline is drawn: one cube's lines.
        rig.frames(1);
        let one_cube = gizmo_vertices(&rig);
        assert!(one_cube > 0, "the outline is drawn");
        assert!(chips(&mut rig).is_empty());

        let swings = u32::from(stone.hardness.div_ceil(profile().damage));
        let answers = host_answers(&mut host, &mut Inventory::new(), WALL_CELL, swings);
        rig.swing(swings);
        rig.deliver(answers);
        let effect = rig.hud().last_break.expect("the wall cell broke");
        assert_eq!(effect.cell, WALL_CELL);

        // BREAK_CHIPS chip entities: a shared cube mesh, an opaque (depth
        // writing and depth tested) material of the cell's color.
        let found = chips(&mut rig);
        assert_eq!(found.len(), BREAK_CHIPS);
        let meshes = rig.app.world().resource::<Assets<Mesh>>();
        let materials = rig.app.world().resource::<Assets<StandardMaterial>>();
        let [r, g, b, _] = stone.color;
        for (i, (chip, mesh, material, transform)) in found.iter().enumerate() {
            assert_eq!(chip.index, i);
            assert_eq!(chip.cell, WALL_CELL);
            assert_eq!(mesh, &found[0].1, "one mesh for all chips");
            assert!(meshes.get(mesh).is_some(), "the mesh exists");
            let material = materials.get(material).expect("the material exists");
            assert_eq!(material.base_color, Color::srgba_u8(r, g, b, u8::MAX));
            assert_eq!(material.alpha_mode, AlphaMode::Opaque);
            assert!(!material.unlit, "chips are lit like the terrain");
            assert_eq!(
                transform.scale,
                Vec3::splat(CHIP_SIZE_M),
                "full size at first"
            );
        }

        // The chips draw no gizmos: only the flash (one cube) is drawn now,
        // since nothing is left in reach to outline.
        assert_eq!(rig.hud().target, None);
        assert_eq!(gizmo_vertices(&rig), one_cube, "the flash only");

        // They fly on the old motion: up and out from the center, falling.
        rig.frames(6);
        let fade = rig.hud().last_break.expect("still breaking").fade;
        let age_s = BREAK_EFFECT_S * (1.0 - fade);
        for (chip, _, _, transform) in chips(&mut rig) {
            let expected = chip_transform(WALL_CELL.center_m(), chip.index, age_s);
            assert!(
                transform.translation.distance(expected.translation) < 1e-4,
                "chip {} at {} (expected {})",
                chip.index,
                transform.translation,
                expected.translation
            );
            assert!(transform.translation.distance(WALL_CELL.center_m()) > CHIP_SIZE_M);
        }

        // Despawned when the effect ends.
        rig.frames((BREAK_EFFECT_S * 60.0).ceil() as u32);
        assert_eq!(rig.hud().last_break, None);
        assert!(chips(&mut rig).is_empty(), "despawned with the effect");

        // A logic-only app spawns no chips.
        let mut host = world("granite", "stone");
        let mut rig = Rig::new(world("granite", "stone"));
        let answers = host_answers(&mut host, &mut Inventory::new(), WALL_CELL, swings);
        rig.swing(swings);
        rig.deliver(answers);
        assert!(rig.hud().last_break.is_some());
        assert!(chips(&mut rig).is_empty());
    }

    #[test]
    fn target_outline_is_depth_tested() {
        let mut rig = Rig::rendering(world("granite", "stone"));
        rig.frames(1);
        assert_eq!(rig.hud().target, Some(WALL_CELL));

        // The HUD group draws with no bias toward the camera: its lines are
        // depth-tested against the terrain like any geometry.
        let store = rig.app.world().resource::<GizmoConfigStore>();
        let (config, _) = store.config::<HudGizmos>();
        assert!(config.enabled);
        assert_eq!(HUD_GIZMO_DEPTH_BIAS, 0.0);
        assert_eq!(config.depth_bias, HUD_GIZMO_DEPTH_BIAS);

        // The flash never reaches out of the cell toward the camera: it
        // starts at the outline's size and closes into the hole.
        assert_eq!(flash_scale(1.0), OUTLINE_SCALE);
        let mut last = OUTLINE_SCALE;
        for step in 0..=10u8 {
            let scale = flash_scale(1.0 - f32::from(step) / 10.0);
            assert!(scale <= last && scale <= OUTLINE_SCALE && scale > 0.0);
            last = scale;
        }
        assert!(last < 1.0, "inside the cell by the end");

        // The outline is drawn in that group: turned off, nothing is drawn.
        assert!(gizmo_vertices(&rig) > 0);
        rig.app
            .world_mut()
            .resource_mut::<GizmoConfigStore>()
            .config_mut::<HudGizmos>()
            .0
            .enabled = false;
        // A few frames free the gizmo asset of the last frame drawn.
        rig.frames(3);
        assert_eq!(gizmo_vertices(&rig), 0, "the outline is a HudGizmos line");

        // So is the break flash.
        let mut host = world("granite", "stone");
        let mut rig = Rig::rendering(world("granite", "stone"));
        let stone = rig.replica_world().materials().id("stone").unwrap();
        let hardness = rig.replica_world().materials().get(stone).unwrap().hardness;
        let swings = u32::from(hardness.div_ceil(profile().damage));
        let answers = host_answers(&mut host, &mut Inventory::new(), WALL_CELL, swings);
        rig.swing(swings);
        rig.app
            .world_mut()
            .resource_mut::<GizmoConfigStore>()
            .config_mut::<HudGizmos>()
            .0
            .enabled = false;
        rig.frames(3);
        assert_eq!(gizmo_vertices(&rig), 0);
        rig.deliver(answers);
        assert!(rig.hud().last_break.is_some());
        assert_eq!(gizmo_vertices(&rig), 0, "the flash is a HudGizmos line");
    }

    #[test]
    fn damaged_cell_gets_crack_tint() {
        let mut host = world("granite", "stone");
        let mut rig = Rig::rendering(world("granite", "stone"));
        let stone = rig.replica_world().materials().id("stone").unwrap();
        let hardness = rig.replica_world().materials().get(stone).unwrap().hardness;
        let dmg = profile().damage;
        assert!(hardness > 2 * dmg, "stone takes more than two swings");

        // No damage, no overlay.
        assert!(cracks(&mut rig).is_empty());

        // Each swing deepens the overlay on the targeted cell: its strength
        // is the HUD's damage fraction, and its color follows it.
        let mut last = 0.0;
        for n in 1..=2 {
            rig.swing(1);
            let fraction = rig.hud().damage.expect("damaged").fraction();
            assert_eq!(fraction, f32::from(n * dmg) / f32::from(hardness));
            let found = cracks(&mut rig);
            assert_eq!(found.len(), 1, "one overlay");
            let (crack, material, transform) = &found[0];
            assert_eq!(crack.cell, WALL_CELL);
            assert_eq!(crack.strength, fraction);
            assert!(crack.strength > last);
            last = crack.strength;
            assert_eq!(transform.translation, WALL_CELL.center_m());
            assert_eq!(
                transform.scale,
                Vec3::splat(CELL_SIZE_M * CRACK_OVERLAY_SCALE),
                "just outside the cell's faces"
            );
            let materials = rig.app.world().resource::<Assets<StandardMaterial>>();
            let material = materials.get(material).expect("the material exists");
            assert_eq!(material.base_color, crack_color(fraction));
            assert_eq!(material.alpha_mode, AlphaMode::Multiply, "darkens");
            assert!(material.unlit);
            assert!(!material.fog_enabled);
        }
        assert!(crack_color(1.0) != crack_color(0.0));
        assert_eq!(crack_color(0.0), Color::WHITE, "no damage, no change");

        // Looking away clears the target and the overlay; looking back shows
        // it again.
        rig.player_mut().pitch = 1.2;
        rig.frames(1);
        assert!(cracks(&mut rig).is_empty(), "nothing targeted");
        rig.player_mut().pitch = 0.0;
        rig.frames(1);
        assert_eq!(cracks(&mut rig).len(), 1);

        // The break clears the damage and removes the overlay.
        let swings = u32::from(hardness.div_ceil(dmg));
        let answers = host_answers(&mut host, &mut Inventory::new(), WALL_CELL, swings);
        rig.swing(swings - 2);
        rig.deliver(answers);
        assert_eq!(rig.hud().damage, None);
        assert!(cracks(&mut rig).is_empty(), "removed with the damage");

        // A logic-only app shows none.
        let mut rig = Rig::new(world("granite", "stone"));
        rig.swing(1);
        assert!(rig.hud().damage.is_some());
        assert!(cracks(&mut rig).is_empty());
    }
}
