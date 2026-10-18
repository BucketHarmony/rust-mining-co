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
    CameraStart, ClientMaterials, ControllerPlugin, FlyCamPlugin, GameCamera, GameRenderPlugin,
    HostConnection, HostLink, MeshPipelinePlugin, NetPlugin, Pickaxe, PickaxePlugin, ReplicaPlugin,
    ToHost,
};
use rmc_host::{ChannelTransport, ClientEndpoint, Host, HostConfig};
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::ToolTable;
use rmc_world::coords::CellAabb;
use rmc_world::edit::PlayerId;
use rmc_world::materials::MaterialTable;
use rmc_world::world::World as CellWorld;
use rmc_worldgen::GenParams;
use rmc_worldgen::Generator;
use rmc_worldgen::params::WorldBounds;

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
        let materials = Arc::new(
            MaterialTable::load(asset_path("materials.ron")).expect("assets/materials.ron loads"),
        );
        let tools = ToolTable::load(asset_path("tools.ron")).expect("assets/tools.ron loads");
        let (world, host_config) = generate_world(&config, &materials);
        let mut transport = ChannelTransport::new();
        let endpoint = transport.connect();
        let player = endpoint.player();
        let host = Host::new(world, host_config, Box::new(transport));

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
            ))
            .insert_resource(ClientMaterials(materials))
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
        };
        test_app.step();
        test_app
    }

    /// One frame: the host ticks, then the app updates.
    pub fn step(&mut self) {
        self.host.tick();
        self.host_ticks += 1;
        self.app.update();
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

    pub fn host_world(&self) -> &CellWorld {
        self.host.world()
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
}

fn asset_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The host's world: `assets/worldgen.ron` with its bounds replaced by
/// `config.world_bounds` (so the bedrock shell follows them), every chunk
/// in the bounds generated and inserted.
fn generate_world(config: &TestConfig, materials: &Arc<MaterialTable>) -> (CellWorld, HostConfig) {
    let mut params =
        GenParams::load(asset_path("worldgen.ron")).expect("assets/worldgen.ron loads");
    let bounds = config.world_bounds;
    params.bounds = WorldBounds {
        min: bounds.min.0.to_array(),
        max: bounds.max.0.to_array(),
    };
    let generator =
        Generator::new(config.seed, &params, materials).expect("worldgen params fit the table");
    let mut world = CellWorld::new(materials.clone(), bounds);
    for (pos, chunk) in generator.generate_world() {
        world
            .insert_chunk(pos, chunk)
            .expect("generated chunks lie in the bounds");
    }
    let host_config = HostConfig {
        seed: config.seed,
        gen_params_hash: params.hash(),
    };
    (world, host_config)
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
}
