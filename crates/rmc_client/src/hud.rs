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
//! - Temperature (SAD-P2 §10): the replica's temperature layer at the cell
//!   the player's feet stand in, top right, in degrees C with one decimal
//!   ([`format_temperature`]). The
//!   replica's layers follow `ChunkSnapshot`, `LayerSnapshot` and
//!   `ChunkDelta::Layers`. Nothing is shown without a player or outside
//!   the resident chunks. With a bag ([`ClientBag`], `docs/ITEMS.md` I4)
//!   the line adds the felt temperature: the cell's plus the worn items'
//!   `warmth` ([`format_felt`]).
//! - Spoil (SAD-P2 §10): when the targeted cell is spoil for the player's
//!   toxicity profile ([`ClientEco::player_tox`], `tox_player` in
//!   `substances.ron`), [`SPOIL_LABEL`] shows under the material name in
//!   [`SPOIL_COLOR`]. It reads the replica's substance layers, so it follows
//!   seasons and layer edits. Without [`ClientEco`] it never shows.
//! - Gas (SAD-P2 §10): every frame the [`GameCamera`]'s own cell is
//!   sampled in the replica's gas layers, whether the player or the fly
//!   camera moves it.
//!   - The readout, under the temperature, lists each nonzero gas in layer
//!     order as `<key> <concentration>` ([`HudGas`]).
//!   - The view tint is a full-screen [`GasTintOverlay`] node behind the
//!     rest of the HUD: the `eco.gas_tint` rows composited in order, each
//!     a layer of its color at opacity [`SubstanceTint::share`] of the
//!     concentration ([`gas_overlay`]). Only `visible` gases have rows.
//!   - The [`LanternIcon`] next to it shows [`LanternState`]: steady with
//!     no flammable gas at the camera, flaring with some below its
//!     `ignite_min`, danger at or above it. Gas harm and lantern ignition
//!     are not modelled (SAD-P2 §16: HUD only).
//!   - Without [`ClientEco`], gases read by layer index, with no tint and
//!     a steady lantern. Without a camera, or outside the resident chunks,
//!     nothing is shown.
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
//!   clears it. With a bag ([`ClientBag`]) the hotbar
//!   ([`HotbarPlugin`](crate::hotbar::HotbarPlugin)) selects instead.
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
use rmc_world::items::Bag;
use rmc_world::layers::{LayerId, N_GASES};
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::query::SubstanceTint;
use rmc_world::world::World;

use crate::controller::Player;
use crate::eco::{ClientEco, unnamed};
use crate::editor::EditorState;
use crate::flycam::{FlyCam, FlyCamSystems};
use crate::hotbar::ClientItems;
use crate::net::{ClientMaterials, FromHost, NetSystems, ToHost};
use crate::pickaxe::{PickTarget, PickaxeSystems, SwingState};
use crate::render::{GameCamera, chunk_material};
use crate::replica::{Replica, apply_host_messages};
use crate::ui::{ColorToken, TextRole, UiText, UiTheme};

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

/// What the spoil indicator says.
pub const SPOIL_LABEL: &str = "SPOIL";

/// The caption over the inventory list, with the UI kit's theme.
pub const INVENTORY_HEADING: &str = "Inventory";

/// The spoil indicator's text color: a sickly yellow-green.
pub const SPOIL_COLOR: Color = Color::srgb(0.78, 0.92, 0.22);

/// Size of the [`LanternIcon`], in logical pixels.
pub const LANTERN_ICON_SIZE: Vec2 = Vec2::new(10.0, 16.0);

/// The lantern icon with no flammable gas at the camera: a warm flame.
pub const LANTERN_STEADY_COLOR: Color = Color::srgb(1.0, 0.82, 0.35);

/// The lantern icon with flammable gas below its `ignite_min`: the blue
/// cap a safety lamp's flame shows in firedamp.
pub const LANTERN_FLARING_COLOR: Color = Color::srgb(0.45, 0.68, 1.0);

/// The lantern icon with flammable gas at or above its `ignite_min`.
pub const LANTERN_DANGER_COLOR: Color = Color::srgb(1.0, 0.22, 0.12);

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
/// One gas in the HUD readout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HudGas {
    pub layer: LayerId,
    /// The gas's key, or `#<index>` without [`ClientEco`].
    pub name: String,
    pub amount: u8,
}

/// What the lantern icon shows (SAD-P2 §10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LanternState {
    /// No flammable gas at the camera.
    #[default]
    Steady,
    /// Some flammable gas, below its `ignite_min`.
    Flaring,
    /// A flammable gas at or above its `ignite_min`.
    Danger,
}

impl LanternState {
    /// The icon's color.
    pub fn color(self) -> Color {
        match self {
            Self::Steady => LANTERN_STEADY_COLOR,
            Self::Flaring => LANTERN_FLARING_COLOR,
            Self::Danger => LANTERN_DANGER_COLOR,
        }
    }
}

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
    /// The temperature of the player's cell in the replica, in 0.1 °C.
    pub temperature: Option<i16>,
    /// With a bag: `temperature` plus the worn items' warmth, in 0.1 °C.
    pub felt_temperature: Option<i16>,
    /// Whether the targeted cell is spoil for the player's toxicity
    /// profile.
    pub spoil: bool,
    /// The nonzero gases at the camera's cell, in layer order.
    pub gases: Vec<HudGas>,
    /// The full-screen gas tint ([`gas_overlay`]), if any gas tints.
    pub gas_tint: Option<Color>,
    /// The lantern icon's state.
    pub lantern: LanternState,
}

/// The view tint of a cell whose gas layer `id` holds `amount(id)`: the
/// `tints` rows composited in order, each a layer of its color at opacity
/// [`SubstanceTint::share`]` / 255` laid over the ones before it (sRGB).
/// `None` when no row has a share.
pub fn gas_overlay(tints: &[SubstanceTint], amount: impl Fn(LayerId) -> u8) -> Option<Color> {
    let mut out: Option<(Vec3, f32)> = None;
    for row in tints {
        let a = f32::from(row.share(amount(row.layer))) / 255.0;
        if a <= 0.0 {
            continue;
        }
        let [r, g, b] = row.color.map(|c| f32::from(c) / 255.0);
        let color = Vec3::new(r, g, b);
        out = Some(match out {
            None => (color, a),
            Some((under, under_a)) => {
                let total = a + under_a * (1.0 - a);
                ((color * a + under * under_a * (1.0 - a)) / total, total)
            }
        });
    }
    out.map(|(c, a)| Color::srgba(c.x, c.y, c.z, a))
}

/// The lantern icon's state for a cell whose gas layer `id` holds
/// `amount(id)`, given the flammable gases' layers and `ignite_min`.
pub fn lantern_state(flammable: &[(LayerId, u8)], amount: impl Fn(LayerId) -> u8) -> LanternState {
    let mut state = LanternState::Steady;
    for &(layer, ignite_min) in flammable {
        let c = amount(layer);
        if c > 0 && c >= ignite_min {
            return LanternState::Danger;
        }
        if c > 0 {
            state = LanternState::Flaring;
        }
    }
    state
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

/// The player's bag as of the host's last `Bag` message (`docs/ITEMS.md`);
/// `None` before one, after a `Welcome`, and with a host that has no items.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientBag(pub Option<Bag>);

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

/// A cell the player saw break (the break feedback above), as a message
/// for whatever else reacts to it ([`crate::sound`]).
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct CellBroke {
    pub cell: CellPos,
    /// The material the cell held.
    pub material: MaterialId,
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

/// The spoil indicator under the material name.
#[derive(Component, Debug)]
pub struct SpoilIndicator;

#[derive(Component, Debug)]
struct DamageBar;

#[derive(Component, Debug)]
struct DamageFill;

#[derive(Component, Debug)]
struct InventoryText;

#[derive(Component, Debug)]
struct TemperatureText;

/// The gas readout under the temperature.
#[derive(Component, Debug)]
struct GasText;

/// A HUD group that hides while it has nothing to show ([`show_chips`]).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudChip {
    /// Top left: the inventory.
    Inventory,
    /// Top right: the temperature, the lantern and the gases.
    Readout,
    /// Bottom center: the targeted material, spoil and damage.
    Target,
}

/// The full-screen gas tint, behind the rest of the HUD.
#[derive(Component, Debug)]
pub struct GasTintOverlay;

/// The lantern icon, colored by [`LanternState::color`].
#[derive(Component, Debug)]
pub struct LanternIcon;

/// `t` (0.1 °C) as the HUD shows it: whole degrees and one tenth, with
/// the sign of the whole value (`-0.5 C`). ASCII only: Bevy's default font
/// has no degree sign.
pub fn format_temperature(t: i16) -> String {
    let t = i32::from(t);
    let sign = if t < 0 { "-" } else { "" };
    let abs = t.abs();
    format!("{sign}{}.{} C", abs / 10, abs % 10)
}

/// The temperature line: `t`, and the felt temperature when known.
pub fn format_felt(t: i16, felt: Option<i16>) -> String {
    match felt {
        Some(felt) => format!(
            "{}, feels {}",
            format_temperature(t),
            format_temperature(felt)
        ),
        None => format_temperature(t),
    }
}

/// The HUD. Needs [`NetPlugin`](crate::net::NetPlugin) for host messages
/// and [`PickaxePlugin`](crate::pickaxe::PickaxePlugin) for the target.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<FromHost>()
            .add_message::<ToHost>()
            .add_message::<CellBroke>()
            .init_resource::<HudState>()
            .init_resource::<ClientInventory>()
            .init_resource::<ClientBag>()
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
                    (show_state, show_gas, show_chips)
                        .chain()
                        .run_if(resource_changed::<HudState>),
                )
                    .chain()
                    .in_set(HudSystems::Show)
                    .after(PickaxeSystems::Target)
                    // The gas readout samples where the camera is this frame.
                    .after(FlyCamSystems),
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

/// Follows the host: inventory totals, the bag, cells whose estimate is
/// void, and cells the player sees break.
#[allow(clippy::too_many_arguments)]
fn track_host(
    mut inbox: MessageReader<FromHost>,
    replica: Option<Res<Replica>>,
    target: Res<PickTarget>,
    time: Res<Time>,
    mut inventory: ResMut<ClientInventory>,
    mut bag: ResMut<ClientBag>,
    mut estimate: ResMut<DamageEstimate>,
    mut feedback: ResMut<Feedback>,
    mut selected: ResMut<SelectedMaterial>,
    mut broken: MessageWriter<CellBroke>,
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
                bag.0 = None;
                estimate.0.clear();
                feedback.broke = None;
                selected.set_if_neq(SelectedMaterial(None));
            }
            ServerMsg::Inventory { items } => inventory.0 = Inventory::from_items(items),
            ServerMsg::Bag(new) => bag.0 = Some((**new).clone()),
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
                    broken.write(CellBroke { cell, material });
                }
            }
            ServerMsg::Rejected { .. }
            | ServerMsg::PlayerStates(_)
            | ServerMsg::Saved { .. }
            | ServerMsg::LayerSnapshot { .. }
            | ServerMsg::SeasonStarted { .. }
            | ServerMsg::SeasonDone { .. }
            | ServerMsg::Clock(_) => {}
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
/// presses. Not while the editor is open, nor with a bag (the hotbar
/// selects).
fn select_material(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    editor: Option<Res<EditorState>>,
    inventory: Res<ClientInventory>,
    bag: Res<ClientBag>,
    mut selected: ResMut<SelectedMaterial>,
) {
    if editor.is_some_and(|e| e.open) || bag.0.is_some() {
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
    eco: Option<Res<ClientEco>>,
    estimate: Res<DamageEstimate>,
    inventory: Res<ClientInventory>,
    selected: Res<SelectedMaterial>,
    fly: Option<Res<FlyCam>>,
    swings: Option<Res<SwingState>>,
    time: Res<Time>,
    mut feedback: ResMut<Feedback>,
    bag: Res<ClientBag>,
    items: Option<Res<ClientItems>>,
    players: Query<&Player>,
    cameras: Query<&Transform, With<GameCamera>>,
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
        temperature: players
            .iter()
            .next()
            .zip(world)
            .and_then(|(player, world)| cell_temperature(world, CellPos::from_meters(player.feet))),
        ..default()
    };
    if let (Some(t), Some(bag), Some(items)) = (next.temperature, &bag.0, &items) {
        let felt = i32::from(t).saturating_add(bag.warmth(&items.items));
        next.felt_temperature = Some(felt.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
    }
    let camera_cell = cameras
        .iter()
        .next()
        .map(|t| CellPos::from_meters(t.translation));
    if let Some(gases) = camera_cell
        .zip(world)
        .and_then(|(cell, world)| cell_gases(world, cell))
    {
        let eco = eco.as_deref();
        let amount = |id: LayerId| {
            gases
                .iter()
                .find(|&&(layer, _)| layer == id)
                .map_or(0, |&(_, c)| c)
        };
        next.gas_tint = eco.and_then(|e| gas_overlay(&e.gas_tint, amount));
        next.lantern = eco.map_or(LanternState::Steady, |e| {
            lantern_state(&e.flammable, amount)
        });
        next.gases = gases
            .iter()
            .map(|&(layer, amount)| HudGas {
                layer,
                name: eco.map_or_else(|| unnamed(layer), |e| e.layer_name(layer)),
                amount,
            })
            .collect();
    }
    let aimed = target.0.filter(|_| crosshair && !flying);
    if let (Some(hit), Some(world)) = (aimed, world)
        && let Some(id) = world.get_cell(hit.cell)
    {
        next.target = Some(hit.cell);
        next.material = Some(material_name(materials, id));
        next.spoil = eco
            .as_deref()
            .is_some_and(|eco| eco.player_tox.is_spoil(world, hit.cell));
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

/// The temperature of `cell` in 0.1 °C, if its chunk is resident.
fn cell_temperature(world: &World, cell: CellPos) -> Option<i16> {
    let (pos, local) = cell.split();
    world
        .chunk(pos)
        .map(|chunk| chunk.layers.get_i16(LayerId::Temperature, local))
}

/// The nonzero gas layers of `cell`, in layer order, if its chunk is
/// resident.
fn cell_gases(world: &World, cell: CellPos) -> Option<Vec<(LayerId, u8)>> {
    let (pos, local) = cell.split();
    let layers = &world.chunk(pos)?.layers;
    Some(
        (0..N_GASES)
            .filter_map(|i| u8::try_from(i).ok())
            .map(|i| (LayerId::Gas(i), layers.get_u8(LayerId::Gas(i), local)))
            .filter(|&(_, c)| c > 0)
            .collect(),
    )
}

/// The material key of `id`, or `#<id>` if the table does not have it.
fn material_name(materials: Option<&MaterialTable>, id: MaterialId) -> String {
    materials
        .and_then(|m| m.get(id))
        .map_or_else(|| format!("#{}", id.0), |d| d.key.clone())
}

/// The HUD's nodes, under one full-screen root.
/// A HUD group: with the UI kit's theme, a chip on the `surface` token
/// (`docs/UI.md` rule 3: text over the world sits on a surface); without
/// it, a bare stack as before the kit.
fn chip(theme: Option<&UiTheme>, align_items: AlignItems) -> impl Bundle {
    let (padding, gap, border, radius, fill, edge) = match theme {
        Some(t) => (
            UiRect::axes(Val::Px(t.space.md), Val::Px(t.space.sm)),
            t.space.xs,
            t.border,
            t.radius.md,
            t.colors.surface,
            t.colors.border,
        ),
        None => (UiRect::ZERO, 4.0, 0.0, 0.0, Color::NONE, Color::NONE),
    };
    (
        Node {
            flex_direction: FlexDirection::Column,
            align_items,
            padding,
            row_gap: Val::Px(gap),
            border: UiRect::all(Val::Px(border)),
            border_radius: BorderRadius::all(Val::Px(radius)),
            ..default()
        },
        BackgroundColor(fill),
        BorderColor::all(edge),
    )
}

/// A HUD text saying `value` in the theme's `role` and `color`, or in the
/// pre-kit look: [`HUD_FONT_SIZE`] in `plain`, with a drop shadow.
fn hud_text(
    theme: Option<&UiTheme>,
    (role, color): (TextRole, ColorToken),
    plain: Color,
    value: &str,
) -> impl Bundle {
    let (size, fill, shadow) = match theme {
        Some(t) => (t.font_size(role), t.colors.get(color), Color::NONE),
        None => (HUD_FONT_SIZE, plain, Color::BLACK),
    };
    (
        UiText { role, color },
        Text::new(value),
        TextFont::from_font_size(size),
        TextColor(fill),
        TextShadow {
            offset: Vec2::splat(1.0),
            color: shadow,
        },
    )
}

/// A column pinned to the top or bottom of the safe area, and to its left
/// (`Some(true)`), right (`Some(false)`) or across its width (`None`).
fn column(align_items: AlignItems, top: bool, left: Option<bool>) -> Node {
    let mut node = Node {
        position_type: PositionType::Absolute,
        flex_direction: FlexDirection::Column,
        align_items,
        ..default()
    };
    if top {
        node.top = Val::ZERO;
    } else {
        node.bottom = Val::ZERO;
    }
    match left {
        Some(true) => node.left = Val::ZERO,
        Some(false) => node.right = Val::ZERO,
        None => {
            node.left = Val::ZERO;
            node.right = Val::ZERO;
        }
    }
    node
}

fn spawn_hud(mut commands: Commands, theme: Option<Res<UiTheme>>) {
    let theme = theme.as_deref();
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
    // The kit's safe area, or the pre-kit 8 px margin.
    let inset = theme.map_or(Val::Px(8.0), |t| Val::VMin(t.safe_area * 100.0));
    let (bar_size, radius, track, bar_fill) = match theme {
        Some(t) => (
            Vec2::new(t.button.width / 2.0, t.space.sm),
            t.radius.sm,
            t.colors.surface_raised,
            t.colors.accent,
        ),
        None => (
            DAMAGE_BAR_SIZE,
            0.0,
            Color::srgba(0.0, 0.0, 0.0, 0.6),
            Color::srgb(0.95, 0.75, 0.2),
        ),
    };
    let edge = theme.map_or(OUTLINE_COLOR, |t| t.colors.border);

    let root = commands
        .spawn((
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            children![
                (
                    GasTintOverlay,
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                    Visibility::Hidden,
                ),
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
            ],
        ))
        .id();
    let safe = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: inset,
                right: inset,
                top: inset,
                bottom: inset,
                ..default()
            },
            ChildOf(root),
        ))
        .id();

    // Top left: the inventory.
    let top_left = commands
        .spawn((column(AlignItems::Start, true, Some(true)), ChildOf(safe)))
        .id();
    let inventory = commands
        .spawn((
            HudChip::Inventory,
            chip(theme, AlignItems::Start),
            ChildOf(top_left),
        ))
        .id();
    if theme.is_some() {
        commands.spawn((
            hud_text(
                theme,
                (TextRole::Caption, ColorToken::TextDim),
                Color::WHITE,
                INVENTORY_HEADING,
            ),
            ChildOf(inventory),
        ));
    }
    commands.spawn((
        InventoryText,
        hud_text(theme, (TextRole::Body, ColorToken::Text), Color::WHITE, ""),
        ChildOf(inventory),
    ));

    // Top right: the temperature and the lantern, then the gases.
    let top_right = commands
        .spawn((column(AlignItems::End, true, Some(false)), ChildOf(safe)))
        .id();
    let readout = commands
        .spawn((
            HudChip::Readout,
            chip(theme, AlignItems::End),
            ChildOf(top_right),
        ))
        .id();
    let gap = theme.map_or(4.0, |t| t.space.sm);
    commands.spawn((
        Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(gap),
            ..default()
        },
        ChildOf(readout),
        children![
            (
                TemperatureText,
                hud_text(theme, (TextRole::Label, ColorToken::Text), Color::WHITE, "")
            ),
            (
                LanternIcon,
                Node {
                    width: Val::Px(LANTERN_ICON_SIZE.x),
                    height: Val::Px(LANTERN_ICON_SIZE.y),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(radius)),
                    ..default()
                },
                BorderColor::all(edge),
                BackgroundColor(LanternState::Steady.color()),
            ),
        ],
    ));
    commands.spawn((
        GasText,
        hud_text(
            theme,
            (TextRole::Body, ColorToken::TextDim),
            Color::WHITE,
            "",
        ),
        TextLayout::justify(Justify::Right),
        ChildOf(readout),
    ));

    // Bottom center: what the pickaxe targets.
    let bottom = commands
        .spawn((column(AlignItems::Center, false, None), ChildOf(safe)))
        .id();
    let target = commands
        .spawn((
            HudChip::Target,
            chip(theme, AlignItems::Center),
            ChildOf(bottom),
        ))
        .id();
    commands.spawn((
        TargetText,
        hud_text(theme, (TextRole::Label, ColorToken::Text), Color::WHITE, ""),
        ChildOf(target),
    ));
    commands.spawn((
        SpoilIndicator,
        hud_text(
            theme,
            (TextRole::Caption, ColorToken::Warning),
            SPOIL_COLOR,
            SPOIL_LABEL,
        ),
        Visibility::Hidden,
        ChildOf(target),
    ));
    commands.spawn((
        DamageBar,
        Node {
            width: Val::Px(bar_size.x),
            height: Val::Px(bar_size.y),
            border_radius: BorderRadius::all(Val::Px(radius)),
            ..default()
        },
        BackgroundColor(track),
        Visibility::Hidden,
        ChildOf(target),
        children![(
            DamageFill,
            Node {
                width: Val::Percent(0.0),
                height: Val::Percent(100.0),
                border_radius: BorderRadius::all(Val::Px(radius)),
                ..default()
            },
            BackgroundColor(bar_fill),
        )],
    ));
}

/// The parts of the target chip that hide: the damage bar and the spoil
/// label.
type ChipParts = (
    Or<(With<DamageBar>, With<SpoilIndicator>)>,
    Without<HudChip>,
);

/// Shows a HUD chip only while it has something to show, so no empty
/// surface sits over the world. A hidden spoil label or damage bar also
/// leaves the layout, so the target chip does not keep their room.
fn show_chips(
    hud: Res<HudState>,
    mut chips: Query<(&HudChip, &mut Visibility)>,
    mut parts: Query<(&Visibility, &mut Node), ChipParts>,
) {
    for (v, mut node) in &mut parts {
        let display = if *v == Visibility::Hidden {
            Display::None
        } else {
            Display::Flex
        };
        if node.display != display {
            node.display = display;
        }
    }
    for (chip, mut v) in &mut chips {
        let on = match chip {
            HudChip::Inventory => !hud.inventory.is_empty(),
            HudChip::Readout => hud.temperature.is_some() || !hud.gases.is_empty(),
            HudChip::Target => hud.material.is_some() || hud.damage.is_some() || hud.spoil,
        };
        v.set_if_neq(if on {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
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
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn show_state(
    hud: Res<HudState>,
    mut crosshair: Query<
        &mut Visibility,
        (
            With<CrosshairNode>,
            Without<DamageBar>,
            Without<SpoilIndicator>,
        ),
    >,
    mut bar: Query<
        &mut Visibility,
        (
            With<DamageBar>,
            Without<CrosshairNode>,
            Without<SpoilIndicator>,
        ),
    >,
    mut spoil: Query<
        &mut Visibility,
        (
            With<SpoilIndicator>,
            Without<CrosshairNode>,
            Without<DamageBar>,
        ),
    >,
    mut fill: Query<&mut Node, With<DamageFill>>,
    mut arms: Query<(&CrosshairArm, &mut Node, &mut BackgroundColor), Without<DamageFill>>,
    mut target_text: Query<
        &mut Text,
        (
            With<TargetText>,
            Without<InventoryText>,
            Without<TemperatureText>,
        ),
    >,
    mut inventory_text: Query<
        &mut Text,
        (
            With<InventoryText>,
            Without<TargetText>,
            Without<TemperatureText>,
        ),
    >,
    mut temperature_text: Query<
        &mut Text,
        (
            With<TemperatureText>,
            Without<TargetText>,
            Without<InventoryText>,
        ),
    >,
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
    for mut v in &mut spoil {
        v.set_if_neq(shown(hud.spoil));
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
    let temperature = hud
        .temperature
        .map(|t| format_felt(t, hud.felt_temperature))
        .unwrap_or_default();
    for mut text in &mut temperature_text {
        text.0.clone_from(&temperature);
    }
}

/// `base` blended toward [`SWING_PULSE_COLOR`] by `pulse` (0.0..=1.0).
/// Puts [`HudState`]'s gas fields on screen: the tint, the lantern icon
/// and the readout.
#[allow(clippy::type_complexity)]
fn show_gas(
    hud: Res<HudState>,
    mut overlay: Query<(&mut BackgroundColor, &mut Visibility), With<GasTintOverlay>>,
    mut lantern: Query<&mut BackgroundColor, (With<LanternIcon>, Without<GasTintOverlay>)>,
    mut text: Query<&mut Text, With<GasText>>,
) {
    for (mut background, mut visibility) in &mut overlay {
        background.set_if_neq(BackgroundColor(hud.gas_tint.unwrap_or(Color::NONE)));
        visibility.set_if_neq(if hud.gas_tint.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    for mut background in &mut lantern {
        background.set_if_neq(BackgroundColor(hud.lantern.color()));
    }
    let lines: Vec<String> = hud
        .gases
        .iter()
        .map(|g| format!("{} {}", g.name, g.amount))
        .collect();
    for mut text in &mut text {
        text.0 = lines.join("\n");
    }
}

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
    use rmc_world::layers::LayerId;
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
            Self::build(world, false, None)
        }

        /// As [`Rig::new`], with what a rendering app gives the HUD: mesh
        /// and material assets, and Bevy's gizmos.
        fn rendering(world: World) -> Self {
            Self::build(world, true, None)
        }

        /// As [`Rig::new`], with the UI kit's theme, as the game has it.
        fn themed(world: World, theme: UiTheme) -> Self {
            Self::build(world, false, Some(theme))
        }

        fn build(world: World, rendering: bool, theme: Option<UiTheme>) -> Self {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins);
            if let Some(theme) = theme {
                app.insert_resource(theme);
            }
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
                temperature: Some(0),
                felt_temperature: None,
                spoil: false,
                gases: vec![],
                gas_tint: None,
                lantern: LanternState::Steady,
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

    /// Eco tables shaped like the shipped ones: substances `cu, fe, acid`;
    /// the player's limits are cu 80 and acid 128, fe has none.
    fn client_eco() -> crate::eco::ClientEco {
        use rmc_world::layers::{N_SUBSTANCES, SubstanceLayers};
        use rmc_world::query::{PhRule, ToxProfile};
        let mut player = [None; N_SUBSTANCES];
        player[0] = Some(80);
        player[2] = Some(128);
        let mut plant = [None; N_SUBSTANCES];
        plant[0] = Some(40);
        crate::eco::ClientEco {
            substances: SubstanceLayers::new(["cu", "fe", "acid"]).unwrap(),
            gases: vec!["steam".into()],
            ph: PhRule {
                acid: Some(LayerId::Substance(2)),
                lime: None,
                ph_shift: 2,
            },
            player_tox: ToxProfile::new(player),
            plant_tox: ToxProfile::new(plant),
            tint: Vec::new(),
            gas_tint: Vec::new(),
            flammable: Vec::new(),
            plants: Vec::new(),
        }
    }

    /// What the host answers to the local player setting `layer` of `cell`
    /// to `value`.
    fn set_layer(host: &mut World, cell: CellPos, layer: LayerId, value: i32) -> ServerMsg {
        let result = host.apply(&EditCommand {
            id: CommandId::default(),
            issuer: PlayerId(1),
            op: EditOp::SetLayer { layer, value },
            shape: Shape::Cell(cell),
        });
        assert_eq!(result.status, EditStatus::Applied);
        ServerMsg::Applied(result)
    }

    #[test]
    fn spoil_indicator_follows_target() {
        let (cu, fe, acid) = (
            LayerId::Substance(0),
            LayerId::Substance(1),
            LayerId::Substance(2),
        );
        let mut host = world("granite", "stone");
        let mut rig = Rig::new(world("granite", "stone"));
        rig.app.insert_resource(client_eco());
        rig.frames(1);
        let indicator = |rig: &mut Rig| {
            let mut q = rig
                .app
                .world_mut()
                .query_filtered::<(&Visibility, &Text), With<SpoilIndicator>>();
            let (v, t) = q.single(rig.app.world()).expect("one spoil indicator");
            assert_eq!(t.0, SPOIL_LABEL);
            *v == Visibility::Inherited
        };

        // A clean wall is not spoil.
        let hud = rig.hud();
        assert_eq!(hud.target, Some(WALL_CELL));
        assert!(!hud.spoil);
        assert!(!indicator(&mut rig));

        // One below the player's limit is not spoil; at the limit it is,
        // and the material name stays.
        rig.deliver([set_layer(&mut host, WALL_CELL, cu, 79)]);
        assert!(!rig.hud().spoil);
        rig.deliver([set_layer(&mut host, WALL_CELL, cu, 80)]);
        let hud = rig.hud();
        assert!(hud.spoil);
        assert_eq!(hud.material.as_deref(), Some("stone"));
        assert!(indicator(&mut rig), "the indicator shows");

        // The floor below is clean; a substance without a player limit
        // leaves it clean; acid at its limit spoils it.
        rig.player_mut().pitch = -1.0;
        rig.frames(1);
        let floor = rig.hud().target.expect("the floor is in reach");
        assert_eq!(floor.0.y, 3);
        assert!(!rig.hud().spoil);
        assert!(!indicator(&mut rig), "the indicator hides");
        rig.deliver([set_layer(&mut host, floor, fe, 255)]);
        assert!(!rig.hud().spoil, "fe has no player limit");
        rig.deliver([set_layer(&mut host, floor, acid, 128)]);
        assert!(rig.hud().spoil);

        // At the sky nothing is targeted, so nothing is spoil.
        rig.player_mut().pitch = 1.2;
        rig.frames(1);
        assert_eq!((rig.hud().target, rig.hud().spoil), (None, false));

        // Back at the wall: spoil until the host clears the copper.
        rig.player_mut().pitch = 0.0;
        rig.frames(1);
        assert!(rig.hud().spoil);
        rig.deliver([set_layer(&mut host, WALL_CELL, cu, 0)]);
        assert_eq!(rig.hud().target, Some(WALL_CELL));
        assert!(!rig.hud().spoil);
        assert!(!indicator(&mut rig));

        // Without the eco tables there is no profile and no indicator.
        rig.deliver([set_layer(&mut host, WALL_CELL, cu, 255)]);
        assert!(rig.hud().spoil);
        rig.app
            .world_mut()
            .remove_resource::<crate::eco::ClientEco>();
        rig.frames(1);
        assert!(!rig.hud().spoil);
        assert!(!indicator(&mut rig));
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

    /// With the UI kit's theme, the HUD's groups are chips on the
    /// `surface` token inside the safe area, their texts take the type
    /// ramp, and a chip shows only while it has something to show; without
    /// it, the pre-kit look.
    #[test]
    fn hud_takes_the_ui_kit_look() {
        let theme = UiTheme::load(crate::ui::theme::tests::THEME_PATH).unwrap();
        let mut rig = Rig::themed(world("stone", "stone"), theme.clone());
        let chip = |rig: &mut Rig, which: HudChip| {
            let world = rig.app.world_mut();
            let mut q = world.query::<(&HudChip, &BackgroundColor, &BorderColor, &Visibility)>();
            let (_, bg, border, v) = q
                .iter(world)
                .find(|(c, ..)| **c == which)
                .expect("the chip");
            (bg.0, border.top, *v == Visibility::Inherited)
        };
        for which in [HudChip::Inventory, HudChip::Readout, HudChip::Target] {
            let (bg, border, _) = chip(&mut rig, which);
            assert_eq!(bg, theme.colors.surface, "{which:?}");
            assert_eq!(border, theme.colors.border, "{which:?}");
        }
        assert!(!chip(&mut rig, HudChip::Inventory).2, "empty: hidden");
        let world = rig.app.world_mut();
        let (ui, font, color) = world
            .query_filtered::<(&UiText, &TextFont, &TextColor), With<InventoryText>>()
            .single(world)
            .unwrap();
        assert_eq!(
            *ui,
            UiText {
                role: TextRole::Body,
                color: ColorToken::Text
            }
        );
        assert_eq!(font.font_size, FontSize::Px(theme.text.body));
        assert_eq!(color.0, theme.colors.text);
        let heading: Vec<String> = world
            .query::<&Text>()
            .iter(world)
            .map(|t| t.0.clone())
            .filter(|t| t == INVENTORY_HEADING)
            .collect();
        assert_eq!(heading.len(), 1, "the inventory has a heading");
        let fill = world
            .query_filtered::<&BackgroundColor, With<DamageFill>>()
            .single(world)
            .unwrap()
            .0;
        assert_eq!(fill, theme.colors.accent, "the damage bar is the accent");
        for display in world
            .query_filtered::<&Node, Or<(With<DamageBar>, With<SpoilIndicator>)>>()
            .iter(world)
            .map(|n| n.display)
        {
            assert_eq!(display, Display::None, "hidden parts take no room");
        }

        let stone = rig.replica_world().materials().id("stone").unwrap();
        rig.deliver([ServerMsg::Inventory {
            items: vec![(stone, 3)],
        }]);
        assert!(chip(&mut rig, HudChip::Inventory).2, "held: shown");

        // Without the kit: no surface, the old font size.
        let mut rig = Rig::new(super::tests::world("stone", "stone"));
        assert_eq!(chip(&mut rig, HudChip::Target).0, Color::NONE);
        let world = rig.app.world_mut();
        let font = world
            .query_filtered::<&TextFont, With<InventoryText>>()
            .single(world)
            .unwrap();
        assert_eq!(font.font_size, FontSize::Px(HUD_FONT_SIZE));
    }

    #[test]
    fn client_bag_follows_the_host() {
        let items = rmc_world::ItemTable::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/items.ron"
        ))
        .expect("assets/items.ron loads");
        let mut rig = Rig::new(world("stone", "stone"));
        let bag = |rig: &Rig| rig.app.world().resource::<ClientBag>().0.clone();
        assert_eq!(bag(&rig), None, "no bag before the host sends one");

        let kit = Bag::starter(&items);
        rig.deliver([ServerMsg::Bag(Box::new(kit.clone()))]);
        assert_eq!(bag(&rig), Some(kit.clone()));

        let mut moved = kit.clone();
        moved
            .move_stack(
                &items,
                rmc_world::items::SlotRef::Hotbar(0),
                rmc_world::items::SlotRef::Pack(0),
                None,
            )
            .unwrap();
        rig.deliver([ServerMsg::Bag(Box::new(moved.clone()))]);
        assert_eq!(bag(&rig), Some(moved));

        // A new Welcome (a loaded world) forgets it until the host resends.
        rig.deliver([ServerMsg::Welcome {
            player: rmc_world::edit::PlayerId(1),
            seed: 0,
            bounds: rig.replica_world().bounds(),
            gen_params_hash: 0,
        }]);
        assert_eq!(bag(&rig), None);
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
            m.id("sulfide_ore").unwrap(),
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
        assert_eq!(inventory_text(&mut rig), "dirt 2\nstone 1\nsulfide_ore 4");

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
        assert_eq!(inventory_text(&mut rig), "dirt 2\n> stone 1\nsulfide_ore 4");

        // The last stone spent: still selected (a place of it is refused by
        // the host), no line marked, and the next step skips it.
        rig.deliver([ServerMsg::Inventory {
            items: vec![(dirt, 2), (copper, 4)],
        }]);
        assert_eq!(rig.hud().selected, Some(stone));
        assert_eq!(inventory_text(&mut rig), "dirt 2\nsulfide_ore 4");
        wheel(&mut rig, -1.0);
        assert_eq!(rig.hud().selected, Some(copper));
        assert_eq!(inventory_text(&mut rig), "dirt 2\n> sulfide_ore 4");

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

    /// The text of the HUD's temperature line.
    fn temperature_text(rig: &mut Rig) -> String {
        let mut q = rig
            .app
            .world_mut()
            .query_filtered::<&Text, With<TemperatureText>>();
        let texts: Vec<String> = q.iter(rig.app.world()).map(|t| t.0.clone()).collect();
        assert_eq!(texts.len(), 1, "one temperature line");
        texts[0].clone()
    }

    #[test]
    fn worn_warmth_adds_to_the_felt_temperature() {
        use rmc_world::ToolTable;
        use rmc_world::items::{EquipSlot, ItemTable, SlotRef};

        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/");
        let items = ClientItems {
            items: ItemTable::load(format!("{root}items.ron")).unwrap(),
            tools: ToolTable::load(format!("{root}tools.ron")).unwrap(),
        };
        let mut w = world("granite", "stone");
        let feet = CellPos::from_meters(FEET);
        let r = w.apply(&EditCommand {
            id: CommandId::default(),
            issuer: PlayerId(0),
            op: EditOp::SetLayer {
                layer: LayerId::Temperature,
                value: 123,
            },
            shape: Shape::Cell(feet),
        });
        assert_eq!(r.status, EditStatus::Applied);
        let mut rig = Rig::new(w);
        rig.frames(1);
        // Without a bag: the air only.
        assert_eq!(rig.hud().felt_temperature, None);
        assert_eq!(temperature_text(&mut rig), "12.3 C");

        let mut bag = Bag::starter(&items.items);
        let warmth = bag.warmth(&items.items);
        assert!(warmth > 0, "the starting clothes are warm");
        rig.app.insert_resource(items.clone());
        rig.app.insert_resource(ClientBag(Some(bag.clone())));
        rig.frames(1);
        let felt = 123 + warmth as i16;
        assert_eq!(rig.hud().felt_temperature, Some(felt));
        assert_eq!(
            temperature_text(&mut rig),
            format!("12.3 C, feels {}", format_temperature(felt))
        );

        // Taking the cap off is colder by the cap's warmth.
        let cap = bag.worn(EquipSlot::Head).unwrap().thing;
        let rmc_world::items::Thing::Item(cap_id) = cap else {
            panic!("an item");
        };
        let cap_warmth = items.items.get(cap_id).unwrap().warmth;
        bag.move_stack(
            &items.items,
            SlotRef::Equip(EquipSlot::Head),
            SlotRef::Pack(0),
            None,
        )
        .unwrap();
        rig.app.insert_resource(ClientBag(Some(bag)));
        rig.frames(1);
        assert_eq!(rig.hud().felt_temperature, Some(felt - cap_warmth));
        assert_eq!(format_felt(-5, Some(85)), "-0.5 C, feels 8.5 C");
    }

    #[test]
    fn hud_shows_player_cell_temperature() {
        // The player's cell is the one its feet stand in: (32, 4, 32), on
        // the floor's top. Its neighbours get other temperatures, so a HUD
        // that read the floor, the eye's cell or a neighbour would show them.
        let feet_cell = CellPos::from_meters(FEET);
        assert_eq!(feet_cell, CellPos::new(32, 4, 32));
        let east = CellPos::new(33, 4, 32);
        let set_temp = |w: &mut World, cell: CellPos, t: i16| {
            let cmd = EditCommand {
                id: CommandId::default(),
                issuer: PlayerId(0),
                op: EditOp::SetLayer {
                    layer: LayerId::Temperature,
                    value: i32::from(t),
                },
                shape: Shape::Cell(cell),
            };
            let result = w.apply(&cmd);
            assert_eq!(result.status, EditStatus::Applied);
            result
        };
        let warm = || {
            let mut w = world("granite", "stone");
            for (cell, t) in [
                (feet_cell, 123),
                (CellPos::new(32, 3, 32), -7),
                (CellPos::new(32, 5, 32), 456),
                (CellPos::new(32, 7, 32), 789),
                (CellPos::new(31, 4, 32), 11),
                (east, -55),
            ] {
                set_temp(&mut w, cell, t);
            }
            w
        };
        let mut host = warm();
        let mut rig = Rig::new(warm());
        rig.frames(1);
        assert_eq!(rig.hud().temperature, Some(123));
        assert_eq!(temperature_text(&mut rig), "12.3 C");

        // Stepping into the next cell column follows the feet.
        rig.player_mut().feet.x += CELL_SIZE_M;
        rig.frames(1);
        assert_eq!(rig.hud().temperature, Some(-55));
        assert_eq!(temperature_text(&mut rig), "-5.5 C");

        // A layers delta from the host moves it.
        let result = set_temp(&mut host, east, 250);
        rig.deliver([ServerMsg::Applied(result)]);
        assert_eq!(rig.hud().temperature, Some(250));
        assert_eq!(temperature_text(&mut rig), "25.0 C");

        // So does a layer snapshot (the end of a season).
        let pos = east.chunk();
        let chunk = rig.replica_world().chunk(pos).unwrap();
        let mut layers = chunk.layers.clone();
        let eco_revision = chunk.eco_revision + 1;
        layers.set_i16(LayerId::Temperature, east.split().1, i16::MIN);
        rig.deliver([ServerMsg::LayerSnapshot {
            pos,
            eco_revision,
            layers,
            plants: BTreeMap::new(),
        }]);
        assert_eq!(rig.hud().temperature, Some(i16::MIN));
        assert_eq!(temperature_text(&mut rig), "-3276.8 C");

        // Outside the replica's chunks, and without a player, nothing.
        rig.player_mut().feet = Vec3::new(16.25, 2.0, -40.0);
        rig.frames(1);
        assert_eq!(rig.hud().temperature, None);
        assert_eq!(temperature_text(&mut rig), "");
        rig.player_mut().feet = FEET;
        rig.frames(1);
        assert_eq!(rig.hud().temperature, Some(123));
        let player = rig.player;
        rig.app.world_mut().despawn(player);
        rig.frames(1);
        assert_eq!(rig.hud().temperature, None);
        assert_eq!(temperature_text(&mut rig), "");

        // The format: tenths of a degree, the sign on the whole value.
        for (t, text) in [
            (0, "0.0 C"),
            (-5, "-0.5 C"),
            (9, "0.9 C"),
            (-10, "-1.0 C"),
            (12000, "1200.0 C"),
            (i16::MAX, "3276.7 C"),
        ] {
            assert_eq!(format_temperature(t), text, "{t}");
        }
    }

    fn gas_text(rig: &mut Rig) -> String {
        let mut q = rig.app.world_mut().query_filtered::<&Text, With<GasText>>();
        let texts: Vec<String> = q.iter(rig.app.world()).map(|t| t.0.clone()).collect();
        assert_eq!(texts.len(), 1, "one gas readout");
        texts[0].clone()
    }

    /// The tint overlay's color and whether it shows.
    fn overlay(rig: &mut Rig) -> (Color, bool) {
        let mut q = rig
            .app
            .world_mut()
            .query_filtered::<(&BackgroundColor, &Visibility, &Node), With<GasTintOverlay>>();
        let (bg, v, node) = q.single(rig.app.world()).expect("one gas tint overlay");
        assert_eq!(
            (node.width, node.height, node.position_type),
            (
                Val::Percent(100.0),
                Val::Percent(100.0),
                PositionType::Absolute
            ),
            "the overlay covers the screen"
        );
        (bg.0, *v == Visibility::Inherited)
    }

    fn lantern_icon(rig: &mut Rig) -> Color {
        let mut q = rig
            .app
            .world_mut()
            .query_filtered::<&BackgroundColor, With<LanternIcon>>();
        q.single(rig.app.world()).expect("one lantern icon").0
    }

    /// Whether two colors match to within float rounding.
    fn near(a: Color, b: Color) -> bool {
        let (a, b) = (a.to_srgba(), b.to_srgba());
        [
            a.red - b.red,
            a.green - b.green,
            a.blue - b.blue,
            a.alpha - b.alpha,
        ]
        .iter()
        .all(|d| d.abs() < 1e-4)
    }

    #[test]
    fn gas_readout_follows_camera_cell() {
        use rmc_world::query::SubstanceTint;

        let (steam, firedamp, chokedamp, smoke) = (
            LayerId::Gas(0),
            LayerId::Gas(1),
            LayerId::Gas(2),
            LayerId::Gas(3),
        );
        let steam_tint = SubstanceTint {
            layer: steam,
            color: [226, 230, 236],
            full: 160,
            strength: 150,
        };
        let smoke_tint = SubstanceTint {
            layer: smoke,
            color: [62, 56, 50],
            full: 128,
            strength: 190,
        };
        let mut eco = client_eco();
        eco.gases = ["steam", "firedamp", "chokedamp", "smoke"]
            .map(String::from)
            .to_vec();
        eco.gas_tint = vec![steam_tint, smoke_tint];
        eco.flammable = vec![(firedamp, 64)];

        // The pure parts first. One row: its own color at its share.
        let only = |id: LayerId, c: u8| move |l: LayerId| if l == id { c } else { 0 };
        assert_eq!(gas_overlay(&eco.gas_tint, |_| 0), None);
        let one = gas_overlay(&eco.gas_tint, only(steam, 80)).unwrap();
        // share = 80 * 150 / 160 = 75.
        assert!(near(one, Color::srgba_u8(226, 230, 236, 75)), "{one:?}");
        assert_eq!(steam_tint.share(255), 150, "full strength from `full` up");
        // Two rows: smoke (share 64 * 190 / 128 = 95) over steam (75).
        let (a1, a2) = (75.0 / 255.0, 95.0 / 255.0);
        let alpha = a2 + a1 * (1.0 - a2);
        let mix = |c1: u8, c2: u8| {
            (f32::from(c2) / 255.0 * a2 + f32::from(c1) / 255.0 * a1 * (1.0 - a2)) / alpha
        };
        let both = gas_overlay(&eco.gas_tint, |l| match l {
            LayerId::Gas(0) => 80,
            LayerId::Gas(3) => 64,
            _ => 0,
        })
        .unwrap();
        let want = Color::srgba(mix(226, 62), mix(230, 56), mix(236, 50), alpha);
        assert!(near(both, want), "{both:?} != {want:?}");
        // Gases without a row (firedamp, chokedamp) do not tint.
        assert!(gas_overlay(&eco.gas_tint, |l| if l == steam { 0 } else { 255 }).is_some());
        assert_eq!(
            gas_overlay(&eco.gas_tint, |l| if l == firedamp || l == chokedamp {
                255
            } else {
                0
            }),
            None
        );
        for (c, state) in [
            (0, LanternState::Steady),
            (1, LanternState::Flaring),
            (63, LanternState::Flaring),
            (64, LanternState::Danger),
            (255, LanternState::Danger),
        ] {
            assert_eq!(
                lantern_state(&eco.flammable, only(firedamp, c)),
                state,
                "{c}"
            );
        }
        assert_eq!(
            lantern_state(&eco.flammable, only(smoke, 255)),
            LanternState::Steady,
            "smoke is not flammable"
        );

        // The camera follows the player's eye at (16.25, 3.55, 16.0) m: cell
        // (32, 7, 32), in the air. Its neighbours, the player's feet cell and
        // the targeted wall cell get other gases, so a readout of any of
        // them would show. (Gases live on air cells only, so the wall has
        // none.)
        let eye_cell = CellPos::new(32, 7, 32);
        let east = CellPos::new(33, 7, 32);
        let mut host = world("granite", "stone");
        let mut rig = Rig::new(world("granite", "stone"));
        rig.app.insert_resource(eco.clone());
        let camera = rig
            .app
            .world_mut()
            .spawn((GameCamera, Transform::default()))
            .id();
        rig.frames(3);
        let camera_cell = |rig: &Rig| {
            CellPos::from_meters(
                rig.app
                    .world()
                    .get::<Transform>(camera)
                    .unwrap()
                    .translation,
            )
        };
        assert_eq!(camera_cell(&rig), eye_cell);
        assert_eq!(CellPos::from_meters(FEET), CellPos::new(32, 4, 32));
        let clean = rig.hud();
        assert_eq!(
            (clean.gases.clone(), clean.gas_tint, clean.lantern),
            (vec![], None, LanternState::Steady)
        );
        assert_eq!(gas_text(&mut rig), "");
        assert_eq!(overlay(&mut rig), (Color::NONE, false));
        assert_eq!(lantern_icon(&mut rig), LANTERN_STEADY_COLOR);

        rig.deliver([
            set_layer(&mut host, CellPos::new(32, 6, 32), steam, 200),
            set_layer(&mut host, CellPos::new(32, 8, 32), smoke, 255),
            set_layer(&mut host, CellPos::new(31, 7, 32), smoke, 255),
            set_layer(&mut host, CellPos::new(32, 7, 31), firedamp, 255),
            set_layer(&mut host, CellPos::new(32, 7, 33), firedamp, 255),
            set_layer(&mut host, CellPos::new(32, 4, 32), chokedamp, 99),
            set_layer(&mut host, east, firedamp, 200),
            set_layer(&mut host, east, chokedamp, 30),
        ]);
        assert_eq!(
            rig.hud().gases,
            vec![],
            "the neighbours are not the camera's cell"
        );
        assert_eq!(gas_text(&mut rig), "");

        // Steam at the camera: the readout, the tint, a steady lantern.
        rig.deliver([set_layer(&mut host, eye_cell, steam, 80)]);
        let hud = rig.hud();
        let gas = |layer: LayerId, name: &str, amount: u8| HudGas {
            layer,
            name: name.to_string(),
            amount,
        };
        assert_eq!(hud.gases, vec![gas(steam, "steam", 80)]);
        assert_eq!(hud.gas_tint, Some(one));
        assert_eq!(hud.lantern, LanternState::Steady);
        assert_eq!(gas_text(&mut rig), "steam 80");
        assert_eq!(overlay(&mut rig), (one, true));

        // Smoke too: both lines in layer order and the composite tint.
        rig.deliver([set_layer(&mut host, eye_cell, smoke, 64)]);
        assert_eq!(rig.hud().gas_tint, Some(both));
        assert_eq!(gas_text(&mut rig), "steam 80\nsmoke 64");
        assert_eq!(overlay(&mut rig), (both, true));

        // Firedamp below its ignite_min flares the lantern and leaves the
        // tint alone (it is not visible); at ignite_min it is danger.
        rig.deliver([set_layer(&mut host, eye_cell, firedamp, 10)]);
        let hud = rig.hud();
        assert_eq!(
            hud.gases,
            vec![
                gas(steam, "steam", 80),
                gas(firedamp, "firedamp", 10),
                gas(smoke, "smoke", 64)
            ]
        );
        assert_eq!(
            (hud.gas_tint, hud.lantern),
            (Some(both), LanternState::Flaring)
        );
        assert_eq!(gas_text(&mut rig), "steam 80\nfiredamp 10\nsmoke 64");
        assert_eq!(lantern_icon(&mut rig), LANTERN_FLARING_COLOR);
        rig.deliver([set_layer(&mut host, eye_cell, firedamp, 63)]);
        assert_eq!(rig.hud().lantern, LanternState::Flaring);
        rig.deliver([set_layer(&mut host, eye_cell, firedamp, 64)]);
        assert_eq!(rig.hud().lantern, LanternState::Danger);
        assert_eq!(lantern_icon(&mut rig), LANTERN_DANGER_COLOR);

        // Clearing the visible gases clears the tint; the readout stays.
        rig.deliver([
            set_layer(&mut host, eye_cell, steam, 0),
            set_layer(&mut host, eye_cell, smoke, 0),
        ]);
        let hud = rig.hud();
        assert_eq!(hud.gases, vec![gas(firedamp, "firedamp", 64)]);
        assert_eq!((hud.gas_tint, hud.lantern), (None, LanternState::Danger));
        assert_eq!(overlay(&mut rig), (Color::NONE, false));

        // Stepping one cell east moves the eye, and the readout with it.
        rig.player_mut().feet.x += CELL_SIZE_M;
        rig.frames(3);
        assert_eq!(camera_cell(&rig), east);
        let hud = rig.hud();
        assert_eq!(
            hud.gases,
            vec![
                gas(firedamp, "firedamp", 200),
                gas(chokedamp, "chokedamp", 30)
            ]
        );
        assert_eq!(gas_text(&mut rig), "firedamp 200\nchokedamp 30");
        rig.player_mut().feet = FEET;
        rig.frames(3);
        assert_eq!(camera_cell(&rig), eye_cell);

        // The fly camera: the player stays, the camera's own cell is read.
        rig.app.world_mut().resource_mut::<FlyCam>().enabled = true;
        let above = CellPos::new(32, 8, 32);
        rig.app
            .world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = above.min_corner_m() + Vec3::splat(CELL_SIZE_M / 2.0);
        rig.frames(1);
        assert_eq!(camera_cell(&rig), above);
        let smoke_only = gas_overlay(&eco.gas_tint, only(smoke, 255));
        let hud = rig.hud();
        assert_eq!(hud.gases, vec![gas(smoke, "smoke", 255)]);
        assert_eq!(
            (hud.gas_tint, hud.lantern),
            (smoke_only, LanternState::Steady)
        );
        assert_eq!(lantern_icon(&mut rig), LANTERN_STEADY_COLOR);

        // A layer snapshot (the end of a season) moves it: the host's chunk
        // after changes the replica has not seen as deltas.
        set_layer(&mut host, above, smoke, 0);
        set_layer(&mut host, above, chokedamp, 7);
        let pos = above.chunk();
        let chunk = host.chunk(pos).unwrap();
        assert!(chunk.eco_revision > rig.replica_world().chunk(pos).unwrap().eco_revision);
        rig.deliver([ServerMsg::LayerSnapshot {
            pos,
            eco_revision: chunk.eco_revision,
            layers: chunk.layers.clone(),
            plants: BTreeMap::new(),
        }]);
        let hud = rig.hud();
        assert_eq!(hud.gases, vec![gas(chokedamp, "chokedamp", 7)]);
        assert_eq!(hud.gas_tint, None);
        assert_eq!(gas_text(&mut rig), "chokedamp 7");

        // Outside the replica's chunks there is nothing to read.
        rig.app
            .world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = Vec3::new(16.0, 3.5, -40.0);
        rig.frames(1);
        let hud = rig.hud();
        assert_eq!(
            (hud.gases, hud.gas_tint, hud.lantern),
            (vec![], None, LanternState::Steady)
        );
        assert_eq!(gas_text(&mut rig), "");

        // Without the eco tables: layer indices, no tint, a steady lantern.
        rig.app.world_mut().resource_mut::<FlyCam>().enabled = false;
        rig.deliver([set_layer(&mut host, eye_cell, steam, 90)]);
        assert_eq!(camera_cell(&rig), eye_cell);
        assert_eq!(rig.hud().lantern, LanternState::Danger);
        rig.app
            .world_mut()
            .remove_resource::<crate::eco::ClientEco>();
        rig.frames(1);
        let hud = rig.hud();
        assert_eq!(
            hud.gases,
            vec![gas(steam, "#0", 90), gas(firedamp, "#1", 64)]
        );
        assert_eq!((hud.gas_tint, hud.lantern), (None, LanternState::Steady));
        assert_eq!(gas_text(&mut rig), "#0 90\n#1 64");
        assert_eq!(overlay(&mut rig), (Color::NONE, false));

        // Without a camera, nothing.
        rig.app.insert_resource(eco);
        rig.frames(1);
        assert_eq!(rig.hud().lantern, LanternState::Danger);
        rig.app.world_mut().despawn(camera);
        rig.frames(1);
        let hud = rig.hud();
        assert_eq!(
            (hud.gases, hud.gas_tint, hud.lantern),
            (vec![], None, LanternState::Steady)
        );
        assert_eq!(gas_text(&mut rig), "");
        assert_eq!(lantern_icon(&mut rig), LANTERN_STEADY_COLOR);
    }
}
