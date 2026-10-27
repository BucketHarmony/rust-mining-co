//! The pickaxe in hand (owner extension): a small boxy pickaxe at the lower
//! right of the view that swings with every swing the pickaxe sends.
//!
//! - [`ViewModel`] is a child of the [`GameCamera`], so it follows the view.
//!   It is one vertex-colored mesh ([`pickaxe_mesh`]): a wooden handle, a
//!   brass collar and an iron head, each box face shaded by its facing
//!   ([`FACE_SHADE`]), drawn unlit so the lantern a hand's width away does
//!   not wash it out. It casts no shadow.
//! - No part gets farther than [`MODEL_REACH_M`] from the eye along any
//!   view axis, less than half the player's width, so the player's box
//!   keeps it out of a wall it faces; and no nearer than [`MODEL_NEAR_M`]
//!   in front, so the near plane does not cut it.
//! - A swing ([`SwingState`] count up) plays [`SWING_S`] of animation: the
//!   head chops forward and down by up to [`SWING_ANGLE`] and back. A place
//!   ([`PlaceState`] count up) pushes the pickaxe forward for [`PLACE_S`].
//!   Walking on the ground bobs it.
//! - With the hotbar ([`Held`], `docs/ITEMS.md` I2) it shows what is held:
//!   each tool's own box model ([`model_mesh`]), a material as a small
//!   block of its color ([`block_mesh`]), and nothing for an empty hand or
//!   an item that is not a tool ([`InHand`]). Without the hotbar it is the
//!   pickaxe.
//! - It shows only while a [`Player`] exists, the fly camera is off and the
//!   editor is closed.
//! - Making the mesh needs mesh and material assets; an app without them
//!   gets no view model.

use std::f32::consts::PI;

use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use rmc_world::items::{HeldModel, Thing};
use rmc_world::materials::MaterialId;

use crate::controller::Player;
use crate::editor::EditorState;
use crate::flycam::FlyCam;
use crate::hotbar::Held;
use crate::pickaxe::{PlaceState, SwingState};
use crate::render::GameCamera;
use crate::replica::Replica;

/// Seconds a swing's chop takes, down and back.
pub const SWING_S: f32 = 0.28;
/// Radians the head turns forward at the bottom of a chop.
pub const SWING_ANGLE: f32 = 0.8;
/// How far the grip moves at the bottom of a chop.
const CHOP_SHIFT_M: Vec3 = Vec3::new(-0.006, -0.01, -0.008);
/// Seconds of the push forward after a place.
pub const PLACE_S: f32 = 0.18;
/// Metres the pickaxe moves forward at the peak of a place.
const PLACE_PUSH_M: f32 = 0.012;
/// The rest pose relative to the camera: right, down, forward (-Z).
const REST_OFFSET_M: Vec3 = Vec3::new(0.072, -0.056, -0.12);
/// The model's scale: small and near, so it fits between the eye and a
/// wall (see [`MODEL_REACH_M`]).
const REST_SCALE: f32 = 0.5;
/// The rest pose's tilt: the head leans forward (about X) and left (Z).
const REST_PITCH: f32 = -0.4;
const REST_ROLL: f32 = 0.35;
const REST_YAW: f32 = 0.2;
/// Farthest any part of the pickaxe gets from the eye along a view axis,
/// in metres.
pub const MODEL_REACH_M: f32 = 0.22;
/// Nearest any part gets in front of the eye, in metres.
pub const MODEL_NEAR_M: f32 = 0.105;
const _: () = assert!(MODEL_REACH_M < rmc_protocol::PLAYER_WIDTH_M / 2.0);
/// Walking bob: metres of travel per bob cycle and its height.
const BOB_STRIDE_M: f32 = 1.5;
const BOB_M: f32 = 0.003;
/// Shade by face: +Y, ±X, ±Z, -Y.
pub const FACE_SHADE: [f32; 4] = [1.0, 0.8, 0.68, 0.5];

/// What the view model shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InHand {
    Tool(HeldModel),
    Block(MaterialId),
    /// Nothing: the model is hidden.
    #[default]
    Empty,
}

impl InHand {
    /// What `held` shows; the pickaxe without the hotbar.
    pub fn of(held: Option<&Held>) -> Self {
        let Some(held) = held else {
            return Self::Tool(HeldModel::Pickaxe);
        };
        match (held.model, held.thing) {
            (Some(model), _) => Self::Tool(model),
            (None, Some(Thing::Material(id))) => Self::Block(id),
            _ => Self::Empty,
        }
    }
}

/// The pickaxe in hand, and its animation state.
#[derive(Component, Debug, Default)]
pub struct ViewModel {
    /// What the mesh shows.
    pub in_hand: InHand,
    seen_swings: u64,
    seen_places: u64,
    /// Seconds into the current swing, if one is playing.
    pub swing: Option<f32>,
    /// Seconds into the current place push, if one is playing.
    pub place: Option<f32>,
    /// Metres walked, for the bob.
    walked: f32,
}

/// [`ViewModelPlugin`]'s system set, in `PostUpdate` before transforms
/// propagate.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewModelSystems;

/// The pickaxe in hand (see the module docs).
pub struct ViewModelPlugin;

impl Plugin for ViewModelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            (spawn_view_model, follow_held, animate, show)
                .chain()
                .in_set(ViewModelSystems)
                .before(TransformSystems::Propagate),
        );
    }
}

/// One box of the model: centre, half extents (metres) and sRGB color.
struct Part {
    center: Vec3,
    half: Vec3,
    color: [f32; 3],
}

/// The model, its grip at the origin and the handle along +Y. The head
/// lies along Z, square to the chop axis (X), so a chop drives the front
/// pick down point first.
const PARTS: [Part; 5] = [
    // Handle.
    Part {
        center: Vec3::new(0.0, 0.055, 0.0),
        half: Vec3::new(0.006, 0.065, 0.006),
        color: [0.46, 0.30, 0.16],
    },
    // Brass collar.
    Part {
        center: Vec3::new(0.0, 0.112, 0.0),
        half: Vec3::new(0.0085, 0.008, 0.0085),
        color: [0.80, 0.60, 0.26],
    },
    // Head.
    Part {
        center: Vec3::new(0.0, 0.122, 0.0),
        half: Vec3::new(0.009, 0.0075, 0.045),
        color: [0.56, 0.57, 0.61],
    },
    // Picks, drooping at both ends.
    Part {
        center: Vec3::new(0.0, 0.115, -0.052),
        half: Vec3::new(0.007, 0.006, 0.011),
        color: [0.66, 0.67, 0.71],
    },
    Part {
        center: Vec3::new(0.0, 0.115, 0.052),
        half: Vec3::new(0.007, 0.006, 0.011),
        color: [0.66, 0.67, 0.71],
    },
];

/// A shovel: the handle with a D grip at the butt, and a flat blade
/// continuing the handle.
const SHOVEL_PARTS: [Part; 5] = [
    // Handle.
    Part {
        center: Vec3::new(0.0, 0.055, 0.0),
        half: Vec3::new(0.006, 0.065, 0.006),
        color: [0.46, 0.30, 0.16],
    },
    // Grip.
    Part {
        center: Vec3::new(0.0, -0.012, 0.0),
        half: Vec3::new(0.014, 0.004, 0.005),
        color: [0.40, 0.26, 0.14],
    },
    // Socket.
    Part {
        center: Vec3::new(0.0, 0.118, 0.0),
        half: Vec3::new(0.008, 0.008, 0.006),
        color: [0.50, 0.51, 0.55],
    },
    // Blade, flat across X.
    Part {
        center: Vec3::new(0.0, 0.142, 0.0),
        half: Vec3::new(0.019, 0.018, 0.0025),
        color: [0.56, 0.57, 0.61],
    },
    // Rounded tip.
    Part {
        center: Vec3::new(0.0, 0.163, 0.0),
        half: Vec3::new(0.012, 0.004, 0.0025),
        color: [0.62, 0.63, 0.67],
    },
];

/// An axe: the handle, a head with its bit forward (-Z), square to the
/// chop like the pickaxe's, and a short poll behind.
const AXE_PARTS: [Part; 4] = [
    // Handle.
    Part {
        center: Vec3::new(0.0, 0.055, 0.0),
        half: Vec3::new(0.006, 0.065, 0.006),
        color: [0.50, 0.33, 0.18],
    },
    // Head.
    Part {
        center: Vec3::new(0.0, 0.115, -0.012),
        half: Vec3::new(0.007, 0.011, 0.02),
        color: [0.50, 0.51, 0.55],
    },
    // Bit, taller than the head.
    Part {
        center: Vec3::new(0.0, 0.115, -0.036),
        half: Vec3::new(0.004, 0.019, 0.005),
        color: [0.68, 0.69, 0.73],
    },
    // Poll.
    Part {
        center: Vec3::new(0.0, 0.115, 0.012),
        half: Vec3::new(0.007, 0.008, 0.005),
        color: [0.44, 0.45, 0.49],
    },
];

/// Half the edge of a held block, model units.
const BLOCK_HALF: f32 = 0.024;

/// The pickaxe mesh: positions, normals and shaded vertex colors.
pub fn pickaxe_mesh() -> Mesh {
    box_mesh(&PARTS)
}

/// The mesh of a tool model.
pub fn model_mesh(model: HeldModel) -> Mesh {
    match model {
        HeldModel::Pickaxe => pickaxe_mesh(),
        HeldModel::Shovel => box_mesh(&SHOVEL_PARTS),
        HeldModel::Axe => box_mesh(&AXE_PARTS),
    }
}

/// A held block of sRGB `color`, at the grip.
pub fn block_mesh(color: [f32; 3]) -> Mesh {
    box_mesh(&[Part {
        center: Vec3::new(0.0, BLOCK_HALF, 0.0),
        half: Vec3::splat(BLOCK_HALF),
        color,
    }])
}

/// What `in_hand` looks like; `None` for an empty hand.
fn in_hand_mesh(in_hand: InHand, replica: Option<&Replica>) -> Option<Mesh> {
    match in_hand {
        InHand::Tool(model) => Some(model_mesh(model)),
        InHand::Block(id) => {
            let rgba = replica
                .and_then(|r| r.world().materials().get(id))
                .map_or([128, 128, 128, 255], |d| d.color);
            let c = |i: usize| f32::from(rgba[i]) / f32::from(u8::MAX);
            Some(block_mesh([c(0), c(1), c(2)]))
        }
        InHand::Empty => None,
    }
}

/// Boxes as one mesh: positions, normals and shaded vertex colors.
fn box_mesh(parts: &[Part]) -> Mesh {
    // Each face: normal, shade index, and its four corners as signs.
    let faces: [(Vec3, usize, [Vec3; 4]); 6] = [
        (
            Vec3::Y,
            0,
            [
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::NEG_Y,
            3,
            [
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(-1.0, -1.0, 1.0),
            ],
        ),
        (
            Vec3::X,
            1,
            [
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::NEG_X,
            1,
            [
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::Z,
            2,
            [
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::NEG_Z,
            2,
            [
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
            ],
        ),
    ];
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();
    for part in parts {
        let [r, g, b] = part.color;
        let base = LinearRgba::from(Srgba::rgb(r, g, b));
        for (normal, shade, corners) in &faces {
            let first = positions.len() as u32;
            let s = FACE_SHADE[*shade];
            for corner in corners {
                positions.push((part.center + *corner * part.half).to_array());
                normals.push(normal.to_array());
                colors.push([base.red * s, base.green * s, base.blue * s, 1.0]);
            }
            indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U32(indices))
}

/// The rest pose relative to the camera.
pub fn rest_transform() -> Transform {
    Transform::from_translation(REST_OFFSET_M)
        .with_rotation(Quat::from_euler(
            EulerRot::YXZ,
            REST_YAW,
            REST_PITCH,
            REST_ROLL,
        ))
        .with_scale(Vec3::splat(REST_SCALE))
}

/// The pose `t` seconds into a swing, a place push, and after `walked`
/// metres of walking.
fn pose(swing: Option<f32>, place: Option<f32>, walked: f32) -> Transform {
    let mut pose = rest_transform();
    if let Some(t) = swing {
        let chop = (PI * (t / SWING_S).clamp(0.0, 1.0)).sin();
        // Chop about the grip's local X: the head comes forward and down.
        pose.rotation *= Quat::from_rotation_x(-SWING_ANGLE * chop);
        pose.translation += CHOP_SHIFT_M * chop;
    }
    if let Some(t) = place {
        let push = (PI * (t / PLACE_S).clamp(0.0, 1.0)).sin();
        pose.translation.z -= PLACE_PUSH_M * push;
    }
    let phase = 2.0 * PI * walked / BOB_STRIDE_M;
    pose.translation += Vec3::new(phase.sin() * BOB_M / 2.0, phase.sin().abs() * BOB_M, 0.0);
    pose
}

/// Gives the game camera its pickaxe, once, where there are mesh assets.
fn spawn_view_model(
    cameras: Query<Entity, With<GameCamera>>,
    existing: Query<(), With<ViewModel>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    swings: Option<Res<SwingState>>,
    places: Option<Res<PlaceState>>,
    mut commands: Commands,
) {
    let (Ok(camera), Some(mut meshes), Some(mut materials)) = (cameras.single(), meshes, materials)
    else {
        return;
    };
    if !existing.is_empty() {
        return;
    }
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        ..default()
    });
    let model = commands
        .spawn((
            ViewModel {
                in_hand: InHand::Tool(HeldModel::Pickaxe),
                // Swings before it existed are not played.
                seen_swings: swings.map_or(0, |s| s.swings()),
                seen_places: places.map_or(0, |p| p.places()),
                ..default()
            },
            Mesh3d(meshes.add(pickaxe_mesh())),
            MeshMaterial3d(material),
            rest_transform(),
            Visibility::Hidden,
            NotShadowCaster,
        ))
        .id();
    commands.entity(camera).add_child(model);
}

/// Swaps the mesh when what is held changes.
fn follow_held(
    held: Option<Res<Held>>,
    replica: Option<Res<Replica>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    mut models: Query<(&mut ViewModel, &mut Mesh3d)>,
) {
    let Some(mut meshes) = meshes else {
        return;
    };
    let want = InHand::of(held.as_deref());
    for (mut model, mut mesh) in &mut models {
        if model.in_hand == want {
            continue;
        }
        model.in_hand = want;
        if let Some(new) = in_hand_mesh(want, replica.as_deref()) {
            mesh.0 = meshes.add(new);
        }
    }
}

/// Starts and plays the swing and place animations, and the walking bob.
fn animate(
    time: Res<Time>,
    swings: Option<Res<SwingState>>,
    places: Option<Res<PlaceState>>,
    players: Query<&Player>,
    mut models: Query<(&mut ViewModel, &mut Transform)>,
) {
    let dt = time.delta_secs();
    let player = players.single().ok();
    for (mut model, mut transform) in &mut models {
        if let Some(swings) = &swings
            && swings.swings() > model.seen_swings
        {
            model.seen_swings = swings.swings();
            model.swing = Some(0.0);
        } else if let Some(t) = model.swing {
            model.swing = Some(t + dt).filter(|t| *t < SWING_S);
        }
        if let Some(places) = &places
            && places.places() > model.seen_places
        {
            model.seen_places = places.places();
            model.place = Some(0.0);
        } else if let Some(t) = model.place {
            model.place = Some(t + dt).filter(|t| *t < PLACE_S);
        }
        match player {
            Some(p) if p.grounded => model.walked += p.velocity.with_y(0.0).length() * dt,
            _ => model.walked = 0.0,
        }
        let next = pose(model.swing, model.place, model.walked);
        transform.set_if_neq(next);
    }
}

/// Shows the pickaxe while walking as a player, outside the editor.
fn show(
    players: Query<(), With<Player>>,
    flycam: Option<Res<FlyCam>>,
    editor: Option<Res<EditorState>>,
    mut models: Query<(&ViewModel, &mut Visibility)>,
) {
    let shown = !players.is_empty()
        && !flycam.is_some_and(|f| f.enabled)
        && !editor.is_some_and(|e| e.open);
    for (model, mut visibility) in &mut models {
        let want = if shown && model.in_hand != InHand::Empty {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        visibility.set_if_neq(want);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;

    const FRAME: Duration = Duration::from_millis(20);

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), TransformPlugin))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(FRAME))
            .init_resource::<SwingState>()
            .init_resource::<PlaceState>()
            .init_resource::<FlyCam>()
            .add_plugins(ViewModelPlugin);
        app.world_mut().spawn((GameCamera, Transform::default()));
        app
    }

    fn model(app: &mut App) -> (Entity, Transform, Visibility) {
        let world = app.world_mut();
        let (e, t, v) = world
            .query_filtered::<(Entity, &Transform, &Visibility), With<ViewModel>>()
            .single(world)
            .expect("one view model");
        (e, *t, *v)
    }

    #[test]
    fn pickaxe_head_lies_square_to_the_chop() {
        // The chop turns about local X; a head along X would only spin.
        let head = &PARTS[2];
        assert!(head.half.z > head.half.x * 4.0, "head along Z");
        // Mid-chop the front pick (-Z) has come forward and down.
        let tip = Vec3::new(0.0, 0.115, -0.063);
        let rest = pose(None, None, 0.0).transform_point(tip);
        let chop = pose(Some(SWING_S / 2.0), None, 0.0).transform_point(tip);
        assert!(chop.y < rest.y, "the pick comes down: {rest} -> {chop}");
    }

    #[test]
    fn pickaxe_mesh_is_shaded_vertex_colored_boxes_within_reach() {
        let mesh = pickaxe_mesh();
        let Some(VertexAttributeValues::Float32x4(colors)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("vertex colors");
        };
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        assert_eq!(positions.len(), PARTS.len() * 24);
        assert_eq!(colors.len(), positions.len());
        // Faces of one box differ in shade.
        assert_ne!(colors[0], colors[4]);
        // Every corner, in the rest pose and through a whole chop, stays
        // within reach of the eye (the camera's origin) and in front of the
        // near plane.
        let (mut far, mut near) = (0.0f32, f32::MAX);
        for step in 0..=10 {
            let t = SWING_S * step as f32 / 10.0;
            for walked in [0.0, 0.4] {
                let pose = pose(Some(t), Some(t.min(PLACE_S)), walked);
                for p in positions {
                    let v = pose.transform_point(Vec3::from_array(*p));
                    far = far.max(v.abs().max_element());
                    near = near.min(-v.z);
                }
            }
        }
        assert!(far <= MODEL_REACH_M, "reaches {far} m");
        assert!(near >= MODEL_NEAR_M, "comes within {near} m");
    }

    #[test]
    fn every_held_model_stays_within_reach() {
        let meshes = [
            ("pickaxe", model_mesh(HeldModel::Pickaxe)),
            ("shovel", model_mesh(HeldModel::Shovel)),
            ("axe", model_mesh(HeldModel::Axe)),
            ("block", block_mesh([0.5, 0.4, 0.3])),
        ];
        for (name, mesh) in &meshes {
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("{name}: positions");
            };
            assert!(mesh.attribute(Mesh::ATTRIBUTE_COLOR).is_some(), "{name}");
            let (mut far, mut near) = (0.0f32, f32::MAX);
            for step in 0..=10 {
                let t = SWING_S * step as f32 / 10.0;
                for walked in [0.0, 0.4] {
                    let pose = pose(Some(t), Some(t.min(PLACE_S)), walked);
                    for p in positions {
                        let v = pose.transform_point(Vec3::from_array(*p));
                        far = far.max(v.abs().max_element());
                        near = near.min(-v.z);
                    }
                }
            }
            assert!(far <= MODEL_REACH_M, "{name} reaches {far} m");
            assert!(near >= MODEL_NEAR_M, "{name} comes within {near} m");
        }
        // The tools differ.
        let shape = |m: &Mesh| format!("{:?}", m.attribute(Mesh::ATTRIBUTE_POSITION));
        assert_ne!(shape(&meshes[0].1), shape(&meshes[1].1));
        assert_ne!(shape(&meshes[0].1), shape(&meshes[2].1));
        assert_ne!(shape(&meshes[1].1), shape(&meshes[2].1));
    }

    #[test]
    fn view_model_follows_what_is_held() {
        let mut app = app();
        app.world_mut().spawn(Player::at(Vec3::ZERO));
        app.update();
        app.update();
        let in_hand = |app: &mut App| {
            let world = app.world_mut();
            let mut q = world.query::<(&ViewModel, &Mesh3d, &Visibility)>();
            let (m, mesh, v) = q.single(world).unwrap();
            (m.in_hand, mesh.0.clone(), *v)
        };
        // Without the hotbar: the pickaxe.
        let (first, pick_mesh, vis) = in_hand(&mut app);
        assert_eq!(first, InHand::Tool(HeldModel::Pickaxe));
        assert_eq!(vis, Visibility::Inherited);

        app.insert_resource(Held {
            model: Some(HeldModel::Shovel),
            ..default()
        });
        app.update();
        let (now, mesh, vis) = in_hand(&mut app);
        assert_eq!(now, InHand::Tool(HeldModel::Shovel));
        assert_ne!(mesh, pick_mesh, "a new mesh");
        assert_eq!(vis, Visibility::Inherited);

        app.insert_resource(Held {
            thing: Some(Thing::Material(MaterialId(2))),
            ..default()
        });
        app.update();
        assert_eq!(in_hand(&mut app).0, InHand::Block(MaterialId(2)));

        // An empty hand hides it.
        app.insert_resource(Held::default());
        app.update();
        let (now, _, vis) = in_hand(&mut app);
        assert_eq!(now, InHand::Empty);
        assert_eq!(vis, Visibility::Hidden);
    }

    #[test]
    fn view_model_rides_the_camera_unlit_and_casts_no_shadow() {
        let mut app = app();
        app.update();
        app.update();
        let (entity, transform, _) = model(&mut app);
        let world = app.world();
        let parent = world.get::<ChildOf>(entity).expect("a child").parent();
        assert!(world.get::<GameCamera>(parent).is_some());
        assert!(world.get::<NotShadowCaster>(entity).is_some());
        let handle = &world
            .get::<MeshMaterial3d<StandardMaterial>>(entity)
            .unwrap()
            .0;
        let material = world
            .resource::<Assets<StandardMaterial>>()
            .get(handle)
            .unwrap();
        assert!(material.unlit);
        assert_eq!(transform, rest_transform());
        // Only one, however many frames run.
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&ViewModel>().iter(world).count(), 1);
    }

    #[test]
    fn a_swing_chops_and_returns_to_rest() {
        let mut app = app();
        app.world_mut().spawn(Player::at(Vec3::ZERO));
        app.update();
        app.update();
        app.world_mut().resource_mut::<SwingState>().swings = 1;
        app.update();
        let mut deepest = 0.0f32;
        for _ in 0..(SWING_S / FRAME.as_secs_f32()) as usize + 2 {
            app.update();
            let (_, t, _) = model(&mut app);
            deepest = deepest.max(t.rotation.angle_between(rest_transform().rotation));
        }
        assert!(deepest > SWING_ANGLE * 0.8, "chopped {deepest} rad");
        let (_, t, _) = model(&mut app);
        assert_eq!(t, rest_transform(), "back at rest");
    }

    #[test]
    fn view_model_shows_only_for_a_walking_player() {
        let mut app = app();
        app.update();
        assert_eq!(model(&mut app).2, Visibility::Hidden, "no player");
        app.world_mut().spawn(Player::at(Vec3::ZERO));
        app.update();
        assert_eq!(model(&mut app).2, Visibility::Inherited);
        app.world_mut().resource_mut::<FlyCam>().enabled = true;
        app.update();
        assert_eq!(model(&mut app).2, Visibility::Hidden, "fly camera");
        app.world_mut().resource_mut::<FlyCam>().enabled = false;
        app.world_mut().insert_resource(EditorState {
            open: true,
            ..default()
        });
        app.update();
        assert_eq!(model(&mut app).2, Visibility::Hidden, "editor open");
    }
}
