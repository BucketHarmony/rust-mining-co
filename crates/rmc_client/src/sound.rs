//! Sound (owner extension): short effects for the pickaxe, footsteps and
//! seasons, and a wind loop. [`SoundConfig`] (`assets/sounds.ron`) says
//! which file plays when; the files come from `scripts/gen-sounds.py`.
//!
//! - Cues: systems turn what happened this frame into [`SoundCue`]s:
//!   - a swing ([`SwingState`] count up) plays `swing` and the targeted
//!     cell's `hit`;
//!   - a place ([`PlaceState`] count up) plays `place`;
//!   - a cell the player sees break ([`CellBroke`]) plays its `broken`;
//!   - every `step_m` walked on the ground plays a step: the `step` of
//!     the first `materials` row that matches the cell underfoot (the cell
//!     at the feet when it is a liquid, so wading splashes) and has one,
//!     else the top-level `step`. Each step's speed, and so its pitch,
//!     varies by up to `step_vary` either way;
//!   - the host's `SeasonDone` plays `season`.
//!
//!   A cell's sounds come from the first `materials` row that matches it,
//!   by phase or tag (rule 5: never by material key), else `default`.
//! - Wind: [`WindLevel`] is `wind`'s volume under the open sky and
//!   `wind_covered` with a non-air cell within `roof_cells` above the
//!   camera. The loop's volume eases toward it.
//! - Playback: cues and the loop play only in an app with Bevy's audio
//!   ([`Assets<AudioSource>`]); elsewhere the cues are still written, so
//!   logic-only apps and tests see them. Sound bytes are read from
//!   [`SoundPlugin::dir`] the first time each file plays; a missing file
//!   is warned about once and stays silent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::audio::{AudioSink, AudioSinkPlayback, Volume};
use bevy::prelude::*;
use rmc_protocol::ServerMsg;
use rmc_world::coords::CellPos;
use rmc_world::materials::{MaterialDef, Phase};
use serde::Deserialize;
use thiserror::Error;

use crate::controller::Player;
use crate::flycam::FlyCam;
use crate::hud::CellBroke;
use crate::net::FromHost;
use crate::pickaxe::{PickTarget, PlaceState, SwingState};
use crate::render::GameCamera;
use crate::replica::Replica;

/// How fast the wind loop's volume follows [`WindLevel`], per second.
const WIND_EASE_PER_S: f32 = 2.0;

/// One sound file and the linear volume it plays at.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Sound {
    pub file: String,
    pub volume: f32,
}

/// What hitting and breaking a cell sounds like.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CellSounds {
    pub hit: Sound,
    pub broken: Sound,
}

/// A `materials` row: matches a material of `phase` or with any of `tags`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MaterialSounds {
    #[serde(default)]
    pub phase: Option<Phase>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub hit: Sound,
    pub broken: Sound,
    /// A footstep on a cell of this row; `None`: the top-level `step`.
    #[serde(default)]
    pub step: Option<Sound>,
}

impl MaterialSounds {
    fn matches(&self, def: &MaterialDef) -> bool {
        self.phase == Some(def.phase) || self.tags.iter().any(|t| def.tags.contains(t))
    }
}

/// `assets/sounds.ron`.
#[derive(Resource, Clone, Debug, Deserialize, PartialEq)]
pub struct SoundConfig {
    pub swing: Sound,
    pub place: Sound,
    pub step: Sound,
    pub step_m: f32,
    /// The largest share by which a step's speed (and pitch) varies.
    #[serde(default)]
    pub step_vary: f32,
    pub season: Sound,
    pub wind: Sound,
    pub wind_covered: f32,
    pub roof_cells: u16,
    pub materials: Vec<MaterialSounds>,
    pub default: CellSounds,
}

#[derive(Debug, Error)]
pub enum SoundConfigError {
    #[error("reading {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("parsing {0}: {1}")]
    Parse(PathBuf, Box<ron::error::SpannedError>),
}

impl SoundConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SoundConfigError> {
        let path = path.as_ref();
        let text =
            std::fs::read_to_string(path).map_err(|e| SoundConfigError::Io(path.into(), e))?;
        ron::from_str(&text).map_err(|e| SoundConfigError::Parse(path.into(), Box::new(e)))
    }

    /// The sounds of a cell of `def`: the first matching row, else
    /// `default`.
    pub fn cell(&self, def: &MaterialDef) -> CellSounds {
        self.materials
            .iter()
            .find(|row| row.matches(def))
            .map(|row| CellSounds {
                hit: row.hit.clone(),
                broken: row.broken.clone(),
            })
            .unwrap_or_else(|| self.default.clone())
    }

    /// A footstep on a cell of `def` (`None`: nothing underfoot): the step
    /// of the first matching row, if it has one, else `step`.
    pub fn step_on(&self, def: Option<&MaterialDef>) -> Sound {
        def.and_then(|def| self.materials.iter().find(|row| row.matches(def)))
            .and_then(|row| row.step.clone())
            .unwrap_or_else(|| self.step.clone())
    }

    /// Every file the config names.
    pub fn files(&self) -> Vec<&str> {
        let mut files = vec![
            &self.swing,
            &self.place,
            &self.step,
            &self.season,
            &self.wind,
            &self.default.hit,
            &self.default.broken,
        ];
        files.extend(self.materials.iter().flat_map(|r| [&r.hit, &r.broken]));
        files.extend(self.materials.iter().filter_map(|r| r.step.as_ref()));
        let mut names: Vec<&str> = files.into_iter().map(|s| s.file.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

/// A sound to play now, and its playback speed (1: as recorded).
#[derive(Message, Clone, Debug, PartialEq)]
pub struct SoundCue(pub Sound, pub f32);

impl SoundCue {
    /// `sound` at its recorded speed.
    pub fn new(sound: Sound) -> Self {
        Self(sound, 1.0)
    }
}

/// The wind loop's target volume (see the module docs).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct WindLevel(pub f32);

/// Marks the wind loop's entity.
#[derive(Component, Debug)]
pub struct WindLoop;

/// [`SoundPlugin`]'s system set, in `Update`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SoundSystems;

/// Cues, wind and playback (see the module docs).
pub struct SoundPlugin {
    pub config: SoundConfig,
    /// The directory holding the sound files.
    pub dir: PathBuf,
}

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SoundCue>()
            .add_message::<CellBroke>()
            .add_message::<FromHost>()
            .insert_resource(self.config.clone())
            .insert_resource(SoundBank {
                dir: self.dir.clone(),
                handles: BTreeMap::new(),
            })
            .init_resource::<WindLevel>()
            .add_systems(
                Update,
                (
                    (cue_tool, cue_breaks, cue_steps, cue_seasons, wind_level),
                    (play_cues, play_wind),
                )
                    .chain()
                    .in_set(SoundSystems),
            );
    }
}

/// Sound files read so far; `None` for one that failed to read.
#[derive(Resource, Debug)]
struct SoundBank {
    dir: PathBuf,
    handles: BTreeMap<String, Option<Handle<AudioSource>>>,
}

impl SoundBank {
    fn handle(
        &mut self,
        file: &str,
        sources: &mut Assets<AudioSource>,
    ) -> Option<Handle<AudioSource>> {
        if let Some(handle) = self.handles.get(file) {
            return handle.clone();
        }
        let path = self.dir.join(file);
        let handle = match std::fs::read(&path) {
            Ok(bytes) => Some(sources.add(AudioSource {
                bytes: Arc::from(bytes),
            })),
            Err(e) => {
                warn!(path = %path.display(), "sound file not read: {e}");
                None
            }
        };
        self.handles.insert(file.to_owned(), handle.clone());
        handle
    }
}

fn cell_def(replica: Option<&Replica>, cell: CellPos) -> Option<MaterialDef> {
    let world = replica?.world();
    let id = world.get_cell(cell)?;
    world.materials().get(id).cloned()
}

/// Swing and place cues, from the pickaxe's counters.
fn cue_tool(
    swings: Option<Res<SwingState>>,
    places: Option<Res<PlaceState>>,
    target: Option<Res<PickTarget>>,
    replica: Option<Res<Replica>>,
    config: Res<SoundConfig>,
    mut seen: Local<(u64, u64)>,
    mut out: MessageWriter<SoundCue>,
) {
    if let Some(swings) = swings
        && swings.swings() > seen.0
    {
        seen.0 = swings.swings();
        out.write(SoundCue::new(config.swing.clone()));
        let aimed = target.and_then(|t| t.0).map(|hit| hit.cell);
        if let Some(def) = aimed.and_then(|cell| cell_def(replica.as_deref(), cell)) {
            out.write(SoundCue::new(config.cell(&def).hit));
        }
    }
    if let Some(places) = places
        && places.places() > seen.1
    {
        seen.1 = places.places();
        out.write(SoundCue::new(config.place.clone()));
    }
}

/// Break cues, from the HUD's [`CellBroke`].
fn cue_breaks(
    mut broke: MessageReader<CellBroke>,
    replica: Option<Res<Replica>>,
    config: Res<SoundConfig>,
    mut out: MessageWriter<SoundCue>,
) {
    for event in broke.read() {
        let def = replica
            .as_deref()
            .and_then(|r| r.world().materials().get(event.material));
        if let Some(def) = def {
            out.write(SoundCue::new(config.cell(def).broken));
        }
    }
}

/// A step every `step_m` walked on the ground, sounding like the ground
/// underfoot. The fly camera walks no one.
#[allow(clippy::too_many_arguments)]
fn cue_steps(
    players: Query<&Player>,
    flycam: Option<Res<FlyCam>>,
    replica: Option<Res<Replica>>,
    time: Res<Time>,
    config: Res<SoundConfig>,
    mut walked: Local<f32>,
    mut count: Local<u32>,
    mut out: MessageWriter<SoundCue>,
) {
    let flying = flycam.is_some_and(|f| f.enabled);
    let Ok(player) = players.single() else {
        return;
    };
    if flying || !player.grounded {
        // The first step after landing comes half a stride in.
        *walked = config.step_m / 2.0;
        return;
    }
    *walked += player.velocity.with_y(0.0).length() * time.delta_secs();
    if *walked >= config.step_m {
        *walked -= config.step_m;
        let ground = underfoot(replica.as_deref(), player.feet);
        *count = count.wrapping_add(1);
        let speed = 1.0 + config.step_vary * step_jitter(*count);
        out.write(SoundCue(config.step_on(ground.as_ref()), speed));
    }
}

/// How far above and below the feet the ground cell is looked for, metres.
const UNDERFOOT_M: f32 = 0.05;

/// The material a step lands on: the cell at the feet when it is a liquid
/// (wading), else the cell just below them; `None` for air.
fn underfoot(replica: Option<&Replica>, feet: Vec3) -> Option<MaterialDef> {
    let at = cell_def(replica, CellPos::from_meters(feet + Vec3::Y * UNDERFOOT_M));
    if let Some(def) = at.filter(|d| d.phase == Phase::Liquid) {
        return Some(def);
    }
    cell_def(replica, CellPos::from_meters(feet - Vec3::Y * UNDERFOOT_M))
        .filter(|d| d.phase != Phase::Air)
}

/// A value in -1..=1 that differs from step to step and is the same every
/// run (an integer hash of the step count).
fn step_jitter(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9);
    x ^= x >> 15;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    (x % 2001) as f32 / 1000.0 - 1.0
}

/// The season chime on `SeasonDone`.
fn cue_seasons(
    mut inbox: MessageReader<FromHost>,
    config: Res<SoundConfig>,
    mut out: MessageWriter<SoundCue>,
) {
    for FromHost(msg) in inbox.read() {
        if matches!(msg, ServerMsg::SeasonDone { .. }) {
            out.write(SoundCue::new(config.season.clone()));
        }
    }
}

/// [`WindLevel`] from what lies above the camera.
fn wind_level(
    camera: Query<&GlobalTransform, With<GameCamera>>,
    replica: Option<Res<Replica>>,
    config: Res<SoundConfig>,
    mut level: ResMut<WindLevel>,
) {
    let (Ok(camera), Some(replica)) = (camera.single(), replica) else {
        return;
    };
    let world = replica.world();
    let eye = CellPos::from_meters(camera.translation());
    let covered = (1..=i32::from(config.roof_cells)).any(|dy| {
        let cell = CellPos::new(eye.0.x, eye.0.y + dy, eye.0.z);
        world
            .get_cell(cell)
            .and_then(|id| world.materials().get(id))
            .is_some_and(|def| def.phase != Phase::Air)
    });
    let target = if covered {
        config.wind_covered
    } else {
        config.wind.volume
    };
    level.set_if_neq(WindLevel(target));
}

/// Plays this frame's cues, where there is audio.
fn play_cues(
    mut cues: MessageReader<SoundCue>,
    sources: Option<ResMut<Assets<AudioSource>>>,
    mut bank: ResMut<SoundBank>,
    mut commands: Commands,
) {
    let Some(mut sources) = sources else {
        cues.clear();
        return;
    };
    for SoundCue(sound, speed) in cues.read() {
        if let Some(handle) = bank.handle(&sound.file, &mut sources) {
            commands.spawn((
                AudioPlayer::new(handle),
                PlaybackSettings::DESPAWN
                    .with_volume(Volume::Linear(sound.volume))
                    .with_speed(*speed),
            ));
        }
    }
}

/// Starts the wind loop once, then eases its volume toward [`WindLevel`].
fn play_wind(
    sources: Option<ResMut<Assets<AudioSource>>>,
    mut bank: ResMut<SoundBank>,
    config: Res<SoundConfig>,
    level: Res<WindLevel>,
    time: Res<Time>,
    mut sinks: Query<Option<&mut AudioSink>, With<WindLoop>>,
    mut commands: Commands,
) {
    let Some(mut sources) = sources else {
        return;
    };
    if sinks.is_empty() {
        if let Some(handle) = bank.handle(&config.wind.file, &mut sources) {
            commands.spawn((
                WindLoop,
                AudioPlayer::new(handle),
                PlaybackSettings::LOOP.with_volume(Volume::Linear(level.0)),
            ));
        }
        return;
    }
    let ease = (WIND_EASE_PER_S * time.delta_secs()).min(1.0);
    for mut sink in sinks.iter_mut().flatten() {
        let now = sink.volume().to_linear();
        sink.set_volume(Volume::Linear(now + (level.0 - now) * ease));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::CellAabb;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::shape::Shape;
    use rmc_world::world::World;
    use std::time::Duration;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn config() -> SoundConfig {
        SoundConfig::load(Path::new(ASSETS).join("sounds.ron")).expect("assets/sounds.ron loads")
    }

    fn materials() -> Arc<MaterialTable> {
        Arc::new(
            MaterialTable::load(Path::new(ASSETS).join("materials.ron"))
                .expect("assets/materials.ron loads"),
        )
    }

    /// 32³ cells of air with a stone slab at y = 20 over x, z in 0..=7.
    fn world() -> World {
        let materials = materials();
        let stone = materials.id("stone").unwrap();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut world = World::new(materials, bounds);
        for pos in bounds.chunks() {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let cmd = EditCommand {
            id: CommandId::default(),
            issuer: PlayerId(0),
            op: EditOp::Set(stone),
            shape: Shape::Box {
                min: CellPos::new(0, 20, 0),
                max: CellPos::new(7, 20, 7),
            },
        };
        assert_eq!(world.apply(&cmd).status, EditStatus::Applied);
        world
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Replica::from_world(world()))
            .add_plugins(SoundPlugin {
                config: config(),
                dir: Path::new(ASSETS).join("sounds"),
            });
        app
    }

    /// Runs one frame and returns the cues it wrote. Drained, because the
    /// buffers only swap after a fixed step, which these frames may skip.
    fn frame(app: &mut App) -> Vec<Sound> {
        app.update();
        let mut cues = app.world_mut().resource_mut::<Messages<SoundCue>>();
        cues.drain().map(|c| c.0).collect()
    }

    fn def(key: &str) -> MaterialDef {
        let m = materials();
        m.get(m.id(key).unwrap()).unwrap().clone()
    }

    #[test]
    fn sounds_ron_files_exist() {
        let config = config();
        for file in config.files() {
            let path = Path::new(ASSETS).join("sounds").join(file);
            assert!(path.is_file(), "{} is missing", path.display());
        }
    }

    #[test]
    fn cell_sounds_match_by_phase_then_tags() {
        let config = config();
        let hit = |key: &str| config.cell(&def(key)).hit.file;
        assert_eq!(hit("water"), "hit_liquid.ogg");
        assert_eq!(hit("magma"), "hit_liquid.ogg");
        assert_eq!(hit("glowcrystal"), "hit_crystal.ogg");
        assert_eq!(hit("sulfide_ore"), "hit_ore.ogg");
        assert_eq!(hit("grass"), "hit_plant.ogg");
        assert_eq!(hit("loam"), "hit_soil.ogg");
        assert_eq!(hit("granite"), "hit_stone.ogg");
        // No row names bedrock's tags (none): the default.
        assert_eq!(config.cell(&def("bedrock")), config.default);
    }

    #[test]
    fn swing_place_break_and_season_cue_their_sounds() {
        let config = config();
        let mut app = app();
        app.init_resource::<SwingState>()
            .init_resource::<PlaceState>();
        assert!(frame(&mut app).is_empty());

        // A swing at the stone slab: the swing and stone's hit.
        let slab = CellPos::new(3, 20, 3);
        app.insert_resource(PickTarget(Some(rmc_world::raycast::Hit {
            cell: slab,
            face_normal: IVec3::NEG_Y,
            distance: 1.0,
        })));
        app.world_mut().resource_mut::<SwingState>().swings = 1;
        let stone = config.cell(&def("stone"));
        assert_eq!(
            frame(&mut app),
            vec![config.swing.clone(), stone.hit.clone()]
        );
        assert!(frame(&mut app).is_empty(), "one cue per swing");

        app.world_mut().resource_mut::<PlaceState>().places = 1;
        assert_eq!(frame(&mut app), vec![config.place.clone()]);

        app.world_mut().write_message(CellBroke {
            cell: slab,
            material: materials().id("stone").unwrap(),
        });
        assert_eq!(frame(&mut app), vec![stone.broken]);

        app.world_mut()
            .write_message(FromHost(ServerMsg::SeasonDone {
                season: 0,
                report: Default::default(),
                changed_chunks: 0,
            }));
        assert_eq!(frame(&mut app), vec![config.season.clone()]);
    }

    #[test]
    fn footsteps_follow_distance_walked_on_the_ground() {
        let config = config();
        let mut app = app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));
        let mut player = Player::at(Vec3::new(4.0, 1.0, 4.0));
        player.velocity = Vec3::new(config.step_m * 2.0, -3.0, 0.0);
        let id = app.world_mut().spawn(player).id();
        let mut steps = 0;
        // Airborne: no steps.
        for _ in 0..20 {
            steps += frame(&mut app).len();
        }
        assert_eq!(steps, 0);
        app.world_mut().get_mut::<Player>(id).unwrap().grounded = true;
        // 2 strides per second for 2 s, from half a stride in: 4 steps.
        for _ in 0..20 {
            steps += frame(&mut app)
                .iter()
                .filter(|s| **s == config.step)
                .count();
        }
        assert_eq!(steps, 4);
    }

    #[test]
    fn steps_sound_like_the_ground_underfoot() {
        let config = config();
        let soil = config.step_on(Some(&def("topsoil")));
        let stone = config.step_on(Some(&def("stone")));
        let plant = config.step_on(Some(&def("grass")));
        let water = config.step_on(Some(&def("water")));
        assert_eq!(config.step_on(None), config.step);
        assert_eq!(stone, config.step, "stone has no row of its own");
        for other in [&soil, &plant, &water] {
            assert_ne!(other.file, stone.file);
        }
        assert!(config.step_vary > 0.0 && config.step_vary < 0.5);

        // On the stone slab: stone steps, each at a slightly different
        // speed within step_vary.
        let mut app = app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));
        let mut player = Player::at(Vec3::new(4.0, 21.0, 4.0));
        player.velocity = Vec3::new(config.step_m * 2.0, 0.0, 0.0);
        player.grounded = true;
        app.world_mut().spawn(player);
        let mut speeds = Vec::new();
        for _ in 0..30 {
            app.update();
            let mut cues = app.world_mut().resource_mut::<Messages<SoundCue>>();
            for SoundCue(sound, speed) in cues.drain() {
                if sound.file == stone.file {
                    speeds.push(speed);
                }
            }
        }
        assert!(speeds.len() >= 5, "{speeds:?}");
        for s in &speeds {
            assert!((s - 1.0).abs() <= config.step_vary + 1e-6, "{s}");
        }
        assert!(speeds.windows(2).any(|w| w[0] != w[1]), "{speeds:?}");
    }

    #[test]
    fn wind_is_quieter_under_a_roof() {
        let config = config();
        let mut app = app();
        // Under the slab (x, z in 0..=7), then out in the open.
        let camera = app
            .world_mut()
            .spawn((
                GameCamera,
                GlobalTransform::from_translation(Vec3::new(2.0, 5.0, 2.0)),
            ))
            .id();
        app.update();
        assert_eq!(app.world().resource::<WindLevel>().0, config.wind_covered);
        *app.world_mut().get_mut::<GlobalTransform>(camera).unwrap() =
            GlobalTransform::from_translation(Vec3::new(12.0, 5.0, 12.0));
        app.update();
        assert_eq!(app.world().resource::<WindLevel>().0, config.wind.volume);
        assert!(config.wind_covered < config.wind.volume);
    }
}
