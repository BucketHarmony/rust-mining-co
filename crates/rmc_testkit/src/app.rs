//! The headless game app (SAD §12.1).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bevy::app::PluginsState;
use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::diagnostic::FrameCount;
use bevy::input::InputPlugin;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::error_handler::{RenderErrorHandler, RenderErrorPolicy};
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::time::TimeUpdateStrategy;
use bevy::transform::TransformPlugin;
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use rmc_client::{
    CameraStart, ClientInventory, ClientMaterials, ControllerPlugin, EditorPlugin, EditorState,
    FlyCamPlugin, GameCamera, GameRenderPlugin, HostConnection, HostLink, HudPlugin, HudState,
    MeshPipelinePlugin, NetPlugin, Pickaxe, PickaxePlugin, ReplicaPlugin, SaveOutcome, SavePlugin,
    SaveStatus, ToHost,
};
use rmc_eco::EcoTables;
use rmc_host::{ChannelTransport, ClientEndpoint, Host, HostConfig, SaveConfig};
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::ToolTable;
use rmc_world::coords::CellAabb;
use rmc_world::edit::PlayerId;
use rmc_world::inventory::Inventory;
use rmc_world::materials::MaterialTable;
use rmc_world::save::{self, SaveError, SaveHeader};
use rmc_world::world::World as CellWorld;
use rmc_worldgen::GenParams;
use rmc_worldgen::params::WorldBounds;
use rmc_worldgen::{
    Generator, LightInit, MagmaInit, NOISE_CRATE_VERSION, TemperatureInit, WaterTableInit,
};

use crate::eco::SeasonLogPlugin;

/// How many steps [`TestApp::save`] waits for the host's answer. It
/// normally arrives two steps after the request.
pub const MAX_SAVE_STEPS: u32 = 10;

/// Frames per second of [`TestApp::step`].
pub const FRAME_HZ: f64 = 60.0;

/// How far one [`TestApp::step`] advances time.
pub fn frame_duration() -> Duration {
    Duration::from_secs_f64(1.0 / FRAME_HZ)
}

/// What [`TestApp::new`] builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestConfig {
    pub seed: u64,
    pub world_bounds: CellAabb,
    pub render: bool,
    pub size: (u32, u32),
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            seed: 1,
            world_bounds: CellAabb::PHASE1_DEFAULT,
            render: true,
            size: (640, 360),
        }
    }
}

/// The image the test camera renders into.
#[derive(Resource, Clone, Debug)]
pub struct TestRenderTarget(pub Handle<Image>);

/// The first render error, if any.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct RenderFailure(pub String);

/// The real game with no window, driven one frame at a time, with its host
/// in-process.
///
/// The host runs seasons with the Phase 2 tables in `assets/` (SAD-P2 §11),
/// and worldgen writes the temperature layer, as in the game. See
/// [`crate::eco`] for the season, layer and fixture helpers.
///
/// The host is ticked synchronously at the start of every [`TestApp::step`],
/// before the app's update, so a message the client sent during frame N is
/// handled at the start of step N + 1 and its result reaches the client in
/// that same step. Client and host talk over the same `ChannelTransport` and
/// messages the app uses.
pub struct TestApp {
    app: App,
    host: Host,
    player: PlayerId,
    config: TestConfig,
    host_ticks: u64,
    /// The Phase 2 tables from `assets/`, before any season override.
    eco: Arc<EcoTables>,
}

impl TestApp {
    /// Generates the world from `assets/` with `config.seed` and
    /// `config.world_bounds`, builds the host and the client app, and runs
    /// one boot frame (startup systems; the client's `Hello` goes out at its
    /// end). From here on every [`TestApp::step`] advances time by exactly
    /// [`frame_duration`].
    ///
    /// Panics if the assets do not load or no render adapter is found.
    pub fn new(config: TestConfig) -> Self {
        let materials = load_materials();
        let eco = load_eco(&materials);
        let params = gen_params(&config);
        let world = generate_world(&config, &params, &eco);
        Self::with_world(config, world, eco, host_config(&config, &params), None)
    }

    /// Like [`TestApp::new`], but the host starts with the save in `dir`
    /// (SAD §10 load: the world generated from `config.seed` and
    /// `config.world_bounds`, then overwritten by the saved chunks), and
    /// later saves go back to `dir`.
    ///
    /// Panics if the save does not load; see [`TestApp::try_load`].
    pub fn load(dir: impl AsRef<Path>, config: TestConfig) -> Self {
        let dir = dir.as_ref();
        Self::try_load(dir, config)
            .unwrap_or_else(|e| panic!("loading the save in {}: {e}", dir.display()))
    }

    /// [`TestApp::load`], failing if the save's header does not match what
    /// `config` and `assets/` build (seed, bounds, material table, worldgen
    /// parameters, noise crate) or a saved chunk is bad.
    pub fn try_load(dir: impl AsRef<Path>, config: TestConfig) -> Result<Self, SaveError> {
        let dir = dir.as_ref();
        let materials = load_materials();
        let eco = load_eco(&materials);
        let params = gen_params(&config);
        let host_config = host_config(&config, &params);
        let header = SaveHeader::new(
            config.seed,
            config.world_bounds,
            materials.hash(),
            tables_hash(),
            host_config.gen_params_hash,
            NOISE_CRATE_VERSION,
        );
        let world = save::load(dir, &header, || generate_world(&config, &params, &eco))?;
        Ok(Self::with_world(
            config,
            world,
            eco,
            host_config,
            Some(save_config(dir)),
        ))
    }

    /// Saves the host's world into `dir` the way the game does: the host's
    /// save directory becomes `dir`, the editor's Save action is raised
    /// (as F5 or the panel's button do), and the app steps until the
    /// client has the host's `Saved` answer, which is returned. Every edit
    /// sent before the call is in the save.
    ///
    /// Panics if no answer arrives within [`MAX_SAVE_STEPS`] steps.
    pub fn save(&mut self, dir: impl AsRef<Path>) -> SaveOutcome {
        self.host.set_save(Some(save_config(dir.as_ref())));
        let requested = self.app.world().resource::<SaveStatus>().requested;
        self.editor().save_requested = true;
        for _ in 0..MAX_SAVE_STEPS {
            self.step();
            let status = self.app.world().resource::<SaveStatus>();
            if status.requested > requested
                && status.pending == 0
                && let Some(outcome) = status.last.clone()
            {
                return outcome;
            }
        }
        panic!("no Saved answer within {MAX_SAVE_STEPS} steps");
    }

    /// The host and client app for `world`, booted (see [`TestApp::new`]).
    fn with_world(
        config: TestConfig,
        world: CellWorld,
        eco: Arc<EcoTables>,
        host_config: HostConfig,
        save: Option<SaveConfig>,
    ) -> Self {
        let materials = world.materials().clone();
        let tools = ToolTable::load(asset_path("tools.ron")).expect("assets/tools.ron loads");
        let mut transport = ChannelTransport::new();
        let endpoint = transport.connect();
        let player = endpoint.player();
        let mut host = Host::new(world, host_config, Box::new(transport)).with_eco(eco.clone());
        host.set_save(save);

        let mut app = App::new();
        if config.render {
            add_render_plugins(&mut app);
        } else {
            // TransformPlugin so GlobalTransform follows Transform as in the
            // game (the mesh pipeline meshes nearest the camera's).
            app.add_plugins((MinimalPlugins, InputPlugin, TransformPlugin));
        }
        app.insert_resource(TimeUpdateStrategy::ManualDuration(frame_duration()))
            .insert_resource(CameraStart::overview(config.world_bounds))
            .add_plugins((
                NetPlugin,
                ReplicaPlugin,
                MeshPipelinePlugin,
                GameRenderPlugin,
                FlyCamPlugin,
                // Inert until a scenario spawns a `Player`.
                ControllerPlugin,
                PickaxePlugin,
                HudPlugin,
                EditorPlugin,
                SavePlugin,
                SeasonLogPlugin,
            ))
            .insert_resource(ClientMaterials(materials))
            .insert_resource(crate::eco::client_eco(&eco))
            .insert_resource(Pickaxe(tools.pickaxe))
            .insert_resource(HostConnection::new(EndpointLink(endpoint)));

        // What App::run does before its first update; we never call run.
        while app.plugins_state() == PluginsState::Adding {
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();

        if config.render {
            spawn_render_target(&mut app, config.size);
        }

        let mut test_app = Self {
            app,
            host,
            player,
            config,
            host_ticks: 0,
            eco,
        };
        test_app.step();
        test_app
    }

    /// One frame: the host ticks, then the app updates. When rendering,
    /// the step returns once the GPU has finished the frame, so a long
    /// scenario never queues up GPU work faster than a software adapter
    /// draws it.
    pub fn step(&mut self) {
        self.host.tick();
        self.host_ticks += 1;
        self.app.update();
        if self.config.render && self.render_failure().is_none() {
            self.wait_for_gpu();
        }
    }

    pub fn steps(&mut self, n: u32) {
        for _ in 0..n {
            self.step();
        }
    }

    /// Queues `msg` as the client's: it goes to the host at the end of the
    /// next step's update and is handled by the host tick of the step after.
    pub fn send(&mut self, msg: ClientMsg) {
        self.app.world_mut().write_message(ToHost(msg));
    }

    pub fn config(&self) -> &TestConfig {
        &self.config
    }

    /// The first render error, if one occurred. Rendering stops after it,
    /// but the app keeps running.
    pub fn render_failure(&self) -> Option<&RenderFailure> {
        self.app.world().get_resource::<RenderFailure>()
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub(crate) fn host_mut(&mut self) -> &mut Host {
        &mut self.host
    }

    /// The Phase 2 tables from `assets/`, without any season override.
    pub fn eco_tables(&self) -> &Arc<EcoTables> {
        &self.eco
    }

    pub fn host_world(&self) -> &CellWorld {
        self.host.world()
    }

    /// `water_table_y` of column `(x, z)` as worldgen laid it down from
    /// `assets/worldgen.ron` and `config.seed` (SAD-P2 §8): cells with a
    /// lower y started below the table. `None` when the params have no
    /// water table.
    pub fn water_table_y(&self, x: i32, z: i32) -> Option<i32> {
        let params = gen_params(&self.config);
        Generator::new(self.config.seed, &params, self.host_world().materials())
            .expect("worldgen params fit the table")
            .water_table_y(x, z)
    }

    pub fn player_id(&self) -> PlayerId {
        self.player
    }

    /// How many times the host has ticked (once per step, boot included).
    pub fn host_ticks(&self) -> u64 {
        self.host_ticks
    }

    /// The app's frame count (one per step, boot included).
    pub fn frame(&self) -> u32 {
        self.app.world().resource::<FrameCount>().0
    }

    /// What the HUD shows after the last step.
    pub fn hud(&self) -> HudState {
        self.app.world().resource::<HudState>().clone()
    }

    /// The debug editor's state, as the keys and the panel set it. Changes
    /// take effect on the next step.
    pub fn editor(&mut self) -> &mut EditorState {
        self.app
            .world_mut()
            .resource_mut::<EditorState>()
            .into_inner()
    }

    /// The local player's inventory as the client knows it (the host's
    /// last `Inventory` message).
    pub fn inventory(&self) -> Inventory {
        self.app.world().resource::<ClientInventory>().0.clone()
    }
}

fn asset_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn load_materials() -> Arc<MaterialTable> {
    Arc::new(MaterialTable::load(asset_path("materials.ron")).expect("assets/materials.ron loads"))
}

/// The Phase 2 tables in `assets/`, on `materials`.
fn load_eco(materials: &Arc<MaterialTable>) -> Arc<EcoTables> {
    Arc::new(
        EcoTables::load_with_materials(materials.clone(), asset_path(""))
            .expect("assets/ Phase 2 tables load"),
    )
}

/// [`EcoTables::hash`] of the Phase 2 tables in `assets/`, for the save
/// header.
fn tables_hash() -> u64 {
    EcoTables::load(asset_path(""))
        .expect("assets/ Phase 2 tables load")
        .hash()
}

/// `assets/worldgen.ron` with its bounds replaced by `config.world_bounds`
/// (so the bedrock shell follows them).
fn gen_params(config: &TestConfig) -> GenParams {
    let mut params =
        GenParams::load(asset_path("worldgen.ron")).expect("assets/worldgen.ron loads");
    let bounds = config.world_bounds;
    params.bounds = WorldBounds {
        min: bounds.min.0.to_array(),
        max: bounds.max.0.to_array(),
    };
    params
}

/// The host's world: every chunk in the bounds of `params`, generated from
/// `config.seed` with its temperature layer, water table, magma
/// chambers and light layer (SAD-P2 §8)
/// and inserted.
fn generate_world(config: &TestConfig, params: &GenParams, eco: &EcoTables) -> CellWorld {
    let materials = eco.materials();
    let generator = Generator::new(config.seed, params, materials)
        .expect("worldgen params fit the table")
        .with_temperature(TemperatureInit::new(eco))
        .with_water_table(WaterTableInit::new(eco))
        .with_magma(MagmaInit::new(eco))
        .with_light(LightInit::new(eco));
    let mut world = CellWorld::new(materials.clone(), config.world_bounds);
    for (pos, chunk) in generator.generate_world() {
        world
            .insert_chunk(pos, chunk)
            .expect("generated chunks lie in the bounds");
    }
    world
}

/// The facts `Welcome` reports for the world `params` generates.
fn host_config(config: &TestConfig, params: &GenParams) -> HostConfig {
    HostConfig {
        seed: config.seed,
        gen_params_hash: params.hash(),
    }
}

/// Saves into `dir`, with the header's noise crate as worldgen's.
fn save_config(dir: &Path) -> SaveConfig {
    SaveConfig {
        dir: dir.to_path_buf(),
        noise_crate_version: NOISE_CRATE_VERSION.to_string(),
        tables_hash: tables_hash(),
    }
}

/// `DefaultPlugins` without a window or winit, rendering synchronously.
fn add_render_plugins(app: &mut App) {
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .set(ImagePlugin::default_nearest())
            .disable::<WinitPlugin>()
            // Render each frame inside its own update, not one frame behind
            // on a render thread, so a step's output is ready when it returns.
            .disable::<PipelinedRenderingPlugin>(),
    )
    // The default handler exits the app; keep it alive and record the error.
    .insert_resource(RenderErrorHandler(|error, main_world, _| {
        error!(?error.ty, description = %error.description, "render error; rendering stopped");
        if !main_world.contains_resource::<RenderFailure>() {
            main_world.insert_resource(RenderFailure(format!(
                "{:?}: {}",
                error.ty, error.description
            )));
        }
        RenderErrorPolicy::StopRendering
    }));
}

/// The image the game camera renders into (`Rgba8UnormSrgb`, readable by
/// copy). The game camera, spawned at startup, is pointed at it and marked
/// [`TestCamera`].
fn spawn_render_target(app: &mut App, (width, height): (u32, u32)) {
    let mut image = Image::new_target_texture(width, height, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    let world = app.world_mut();
    let handle = world.resource_mut::<Assets<Image>>().add(image);
    world.insert_resource(TestRenderTarget(handle.clone()));
    world.add_observer(move |add: On<Add<GameCamera>>, mut commands: Commands| {
        commands
            .entity(add.entity)
            .insert((TestCamera, RenderTarget::Image(handle.clone().into())));
    });
}

/// Marks the game camera, which renders into [`TestRenderTarget`].
#[derive(Component)]
pub struct TestCamera;

/// The client side of a `ChannelTransport` connection.
struct EndpointLink(ClientEndpoint);

impl HostLink for EndpointLink {
    fn send(&self, msg: ClientMsg) -> bool {
        self.0.send(msg).is_ok()
    }

    fn drain(&self) -> Vec<ServerMsg> {
        self.0.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::RenderApp;
    use bevy::render::render_resource::{TextureFormat, TextureUsages};
    use bevy::render::renderer::{RenderAdapterInfo, RenderDevice};
    use bevy::window::Window;
    use rmc_client::{FromHost, Replica};
    use rmc_protocol::ServerMsg;
    use rmc_world::coords::CellPos;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus};
    use rmc_world::materials::MaterialId;
    use rmc_world::shape::Shape;

    /// 2 × 4 × 2 chunks: full height, so it has surface, caves and bedrock.
    fn small_bounds() -> CellAabb {
        CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63))
    }

    fn logic_config() -> TestConfig {
        TestConfig {
            seed: 11,
            world_bounds: small_bounds(),
            render: false,
            ..TestConfig::default()
        }
    }

    fn frame(t: &TestApp) -> u32 {
        t.app().world().resource::<FrameCount>().0
    }

    fn window_count(t: &mut TestApp) -> usize {
        let world = t.app_mut().world_mut();
        world.query::<&Window>().iter(world).count()
    }

    /// What the client app received from the host, tagged with the frame.
    #[derive(Resource, Default)]
    struct Inbox(Vec<(u32, ServerMsg)>);

    fn record(mut inbox: MessageReader<FromHost>, frame: Res<FrameCount>, mut log: ResMut<Inbox>) {
        log.0
            .extend(inbox.read().map(|FromHost(msg)| (frame.0, msg.clone())));
    }

    #[test]
    fn boots_without_window() {
        let mut t = TestApp::new(TestConfig {
            world_bounds: small_bounds(),
            ..TestConfig::default()
        });
        let start = frame(&t);
        let virt0 = t.app().world().resource::<Time<Virtual>>().elapsed();
        t.steps(5);
        assert_eq!(frame(&t), start + 5, "every step renders one frame");
        assert_eq!(
            t.app().world().resource::<Time<Virtual>>().elapsed() - virt0,
            frame_duration() * 5,
            "manual time with the renderer too"
        );

        assert_eq!(window_count(&mut t), 0, "no window entity");
        assert!(t.app().get_sub_app(RenderApp).is_some(), "render sub-app");
        let world = t.app().world();
        assert!(world.contains_resource::<RenderDevice>(), "render device");
        let adapter = world.resource::<RenderAdapterInfo>();
        eprintln!(
            "adapter: {} ({:?}, {:?})",
            adapter.name, adapter.backend, adapter.device_type
        );
        assert_eq!(world.get_resource::<RenderFailure>(), None);
        assert!(t.app().should_exit().is_none(), "app keeps running");

        let target = t.app().world().resource::<TestRenderTarget>().0.clone();
        let images = t.app().world().resource::<Assets<Image>>();
        let image = images.get(&target).expect("render target image");
        let desc = &image.texture_descriptor;
        assert_eq!(desc.format, TextureFormat::Rgba8UnormSrgb);
        assert_eq!((desc.size.width, desc.size.height), (640, 360));
        assert!(
            desc.usage
                .contains(TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT)
        );
        let world = t.app_mut().world_mut();
        let cameras = world
            .query::<(&Camera, &bevy::camera::RenderTarget)>()
            .iter(world)
            .filter(|(_, rt)| rt.as_image() == Some(&target))
            .count();
        assert_eq!(cameras, 1, "one camera renders into the target");

        // render: false is logic only: no renderer, no window, input still works.
        let mut t = TestApp::new(logic_config());
        t.steps(2);
        assert_eq!(window_count(&mut t), 0);
        assert!(t.app().get_sub_app(RenderApp).is_none());
        assert!(t.app().world().contains_resource::<ButtonInput<KeyCode>>());
        assert!(
            t.app()
                .world()
                .contains_resource::<ButtonInput<MouseButton>>()
        );
        assert!(!t.app().world().contains_resource::<TestRenderTarget>());
    }

    #[test]
    fn logic_mode_camera_starts_at_camera_start() {
        let mut t = TestApp::new(logic_config());
        t.step();
        let expected = CameraStart::overview(small_bounds()).0;
        let world = t.app_mut().world_mut();
        let (transform, global) = world
            .query_filtered::<(&Transform, &GlobalTransform), With<GameCamera>>()
            .single(world)
            .expect("one game camera in logic mode too");
        assert_eq!(*transform, expected);
        assert_eq!(global.translation(), expected.translation, "propagated");
    }

    #[test]
    fn step_advances_one_frame() {
        let mut t = TestApp::new(logic_config());
        let dt = frame_duration();
        assert_eq!(dt, Duration::from_secs_f64(1.0 / 60.0));
        for i in 0..6 {
            let f0 = frame(&t);
            let real0 = t.app().world().resource::<Time<Real>>().elapsed();
            let virt0 = t.app().world().resource::<Time<Virtual>>().elapsed();
            if i == 3 {
                // Wall-clock time does not leak in.
                std::thread::sleep(Duration::from_millis(40));
            }
            t.step();
            let world = t.app().world();
            assert_eq!(frame(&t), f0 + 1, "step {i}: one frame");
            assert_eq!(world.resource::<Time<Real>>().elapsed() - real0, dt);
            assert_eq!(world.resource::<Time<Virtual>>().elapsed() - virt0, dt);
            assert_eq!(world.resource::<Time<Virtual>>().delta(), dt);
            assert_eq!(world.resource::<Time>().delta(), dt);
        }
        t.steps(10);
        assert_eq!(
            t.app().world().resource::<Time<Virtual>>().elapsed(),
            dt * 16,
            "boot frame has no delta; every step after it adds one frame"
        );
    }

    #[test]
    fn host_tick_runs_each_step() {
        let mut t = TestApp::new(logic_config());
        t.app_mut()
            .init_resource::<Inbox>()
            .add_systems(Update, record);
        let p = t.player_id();
        let ticks0 = t.host_ticks();

        // Boot sent Hello; the next step's host tick joins the player, and
        // the same step's update delivers Welcome and every chunk.
        // FrameCount advances in `Last`, so `Update` sees the count from
        // before the step.
        assert!(!t.host().is_joined(p));
        let f = frame(&t);
        t.step();
        assert!(t.host().is_joined(p), "host ticked before the update");
        let inbox = std::mem::take(&mut t.app_mut().world_mut().resource_mut::<Inbox>().0);
        let chunks = t.host_world().chunk_count();
        assert_eq!(chunks, 16);
        assert_eq!(
            inbox.len(),
            1 + chunks,
            "Welcome and one snapshot per chunk"
        );
        assert!(inbox.iter().all(|(frame, _)| *frame == f));
        assert!(matches!(inbox[0].1, ServerMsg::Welcome { player, .. } if player == p));
        assert!(
            inbox[1..]
                .iter()
                .all(|(_, m)| matches!(m, ServerMsg::ChunkSnapshot { .. }))
        );
        let replica = t.app().world().resource::<Replica>();
        assert_eq!(replica.player(), p);
        assert_eq!(
            replica.world().hash(),
            t.host_world().hash(),
            "replica joined"
        );

        // One edit per frame: each step's tick applies exactly the edit the
        // client sent during the frame before, and that step's update
        // receives its result.
        let world = t.host_world();
        let mat: MaterialId = world
            .materials()
            .with_tag("stone")
            .next()
            .expect("a stone material")
            .id;
        let top = world.bounds().max.0.y;
        let cells: Vec<CellPos> = (0..5).map(|x| CellPos::new(10 + x, top, 20)).collect();
        for &c in &cells {
            assert_eq!(
                world.get_cell(c),
                Some(MaterialId::AIR),
                "{c:?} starts as air"
            );
        }
        let edit = |seq: u32, cell: CellPos| EditCommand {
            id: CommandId { issuer: p, seq },
            issuer: p,
            op: EditOp::Set(mat),
            shape: Shape::Cell(cell),
        };
        for i in 0..=cells.len() {
            if i < cells.len() {
                t.send(ClientMsg::Edit(edit(i as u32, cells[i])));
            }
            let f = frame(&t);
            t.step();
            for (j, &c) in cells.iter().enumerate() {
                let want = if j < i { mat } else { MaterialId::AIR };
                assert_eq!(t.host_world().get_cell(c), Some(want), "step {i}, cell {j}");
            }
            let inbox = std::mem::take(&mut t.app_mut().world_mut().resource_mut::<Inbox>().0);
            let applied: Vec<u32> = inbox
                .iter()
                .filter_map(|(frame, m)| match m {
                    ServerMsg::Applied(r) => {
                        assert_eq!(*frame, f, "result arrives in the step that applied it");
                        assert_eq!(r.status, EditStatus::Applied);
                        assert_eq!(r.deltas.len(), 1);
                        Some(r.command.seq)
                    }
                    _ => None,
                })
                .collect();
            let want: Vec<u32> = if i > 0 { vec![i as u32 - 1] } else { vec![] };
            assert_eq!(applied, want, "step {i}");
        }
        assert_eq!(t.host_ticks() - ticks0, 1 + cells.len() as u64 + 1);
        let replica = t.app().world().resource::<Replica>();
        assert_eq!(
            replica.world().hash(),
            t.host_world().hash(),
            "replica follows edits"
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        use rmc_client::SaveStatus;
        use rmc_world::save::{HEADER_FILE, HeaderField, SaveError};

        let dir = std::env::temp_dir().join(format!("rmc_testkit_save_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut t = TestApp::new(logic_config());
        t.step();
        let p = t.player_id();
        let world = t.host_world();
        let stone = world
            .materials()
            .with_tag("stone")
            .next()
            .expect("a stone material")
            .id;
        let cell = CellPos::new(5, world.bounds().max.0.y, 7);
        assert_eq!(world.get_cell(cell), Some(MaterialId::AIR));
        let generated = world.hash();
        t.send(ClientMsg::Edit(EditCommand {
            id: CommandId { issuer: p, seq: 0 },
            issuer: p,
            op: EditOp::Set(stone),
            // Sent before the save, so the save holds it.
            shape: Shape::Cell(cell),
        }));

        // Saved through the client's SaveRequest; the edit is in it.
        let requested = t.app().world().resource::<SaveStatus>().requested;
        let outcome = t.save(&dir);
        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.path, dir.display().to_string());
        assert_eq!(
            t.app().world().resource::<SaveStatus>().requested,
            requested + 1,
            "one request"
        );
        assert!(dir.join(HEADER_FILE).is_file());
        assert_eq!(t.host_world().get_cell(cell), Some(stone));
        let hash = t.host_world().hash();
        assert_ne!(hash, generated);

        // The loaded app starts from the save, and its replica joins it.
        let mut u = TestApp::load(&dir, logic_config());
        assert_eq!(u.host_world().hash(), hash);
        assert_eq!(u.host_world().get_cell(cell), Some(stone));
        u.step();
        let replica = u.app().world().resource::<Replica>();
        assert_eq!(replica.world().hash(), hash, "replica of the loaded world");

        // Its F5 saves back to the same directory.
        u.press(KeyCode::F1);
        u.step();
        u.release(KeyCode::F1);
        u.press(KeyCode::F5);
        u.steps(3);
        let status = u.app().world().resource::<SaveStatus>();
        assert_eq!(
            status.last.as_ref().map(|o| (o.ok, o.path.clone())),
            Some((true, dir.display().to_string())),
            "{status:?}"
        );

        // A save loads only into what it was made from.
        let mismatch = |config: TestConfig| match TestApp::try_load(&dir, config) {
            Err(SaveError::HeaderMismatch { field, .. }) => Some(field),
            Err(e) => panic!("not a header mismatch: {e}"),
            Ok(_) => None,
        };
        let other_seed = TestConfig {
            seed: 12,
            ..logic_config()
        };
        assert_eq!(mismatch(other_seed), Some(HeaderField::Seed));
        let other_bounds = TestConfig {
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 127, 63)),
            ..logic_config()
        };
        assert_eq!(mismatch(other_bounds), Some(HeaderField::Bounds));
        let missing = dir.join("missing");
        assert!(matches!(
            TestApp::try_load(&missing, logic_config()),
            Err(SaveError::Io { .. })
        ));

        // A TestApp::new host has no save directory and refuses.
        let mut v = TestApp::new(logic_config());
        v.step();
        let outcome = v.save(&dir);
        assert!(outcome.ok, "save() points the host at the directory");
        let mut w = TestApp::new(logic_config());
        w.step();
        w.editor().save_requested = true;
        w.steps(3);
        let status = w.app().world().resource::<SaveStatus>();
        assert_eq!(status.last.as_ref().map(|o| o.ok), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
