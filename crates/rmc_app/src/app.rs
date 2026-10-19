//! The game app (SAD §4, §10, §11): the host on its own thread, every
//! client plugin, and the local player.
//!
//! [`build_game_app`] starts the world ([`start_world`]: the save in the
//! save directory, else generated), runs the host on its own thread over a
//! `ChannelTransport`, and builds the client `App` around it. `main` builds
//! it with a window; tests build it without one (CLAUDE.md rule 12) and
//! step it by hand.
//!
//! [`SpawnPlugin`] places the local [`Player`] once the replica holds every
//! chunk of the world: standing on the topmost solid cell of the column at
//! the middle of the world. A new world from the host (the editor's Load or
//! Regenerate) puts it there again.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::input::InputPlugin;
use bevy::prelude::*;
use bevy::transform::TransformPlugin;
use bevy::window::{ExitCondition, WindowPlugin};
use rmc_client::controller::Player;
use rmc_client::{
    CameraStart, ClientMaterials, ControllerPlugin, CursorPlugin, EditorPanelPlugin, EditorPlugin,
    FlyCamPlugin, FromHost, GameRenderPlugin, HostConnection, HudPlugin, MeshPipelinePlugin,
    NetPlugin, Pickaxe, PickaxePlugin, Replica, ReplicaPlugin, SavePlugin,
};
use rmc_host::{ChannelTransport, spawn_host_thread};
use rmc_protocol::ServerMsg;
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::world::World as CellWorld;
use tracing::info;

use crate::cli::Cli;
use crate::session::{
    EndpointLink, GameAssets, GameHost, HOST_HZ, SessionPlugin, assets_dir, start_world,
};

/// The window's title.
pub const GAME_TITLE: &str = "Rust Mining Co";

/// What the game is built from: the tables, the seed and the save directory.
#[derive(Clone, Debug)]
pub struct GameConfig {
    pub assets: GameAssets,
    /// `None`: the save's seed, else [`DEFAULT_SEED`](crate::session::DEFAULT_SEED).
    pub seed: Option<u64>,
    pub save_dir: PathBuf,
}

impl GameConfig {
    /// The command line's seed and save directory, with the tables from
    /// [`assets_dir`].
    pub fn from_cli(cli: &Cli) -> anyhow::Result<Self> {
        Ok(Self {
            assets: GameAssets::load(&assets_dir())?,
            seed: cli.seed,
            save_dir: cli.save.clone(),
        })
    }
}

/// The game: the host thread and the client app. `windowed` builds
/// `DefaultPlugins` with a primary window, the editor's egui panel and the
/// cursor grab; otherwise the app has no window and does not render.
///
/// The app is returned unfinished; `App::run` finishes it, and a caller
/// that updates it by hand finishes it first.
pub fn build_game_app(config: GameConfig, windowed: bool) -> anyhow::Result<App> {
    let GameConfig {
        assets,
        seed,
        save_dir,
    } = config;
    let mut app = App::new();
    if windowed {
        app.add_plugins(DefaultPlugins.set(window_plugin()));
    } else {
        // TransformPlugin so GlobalTransform follows Transform (the mesh
        // pipeline meshes nearest the camera's).
        app.add_plugins((MinimalPlugins, InputPlugin, TransformPlugin));
    }

    // After the plugins, so a windowed app's log has the start.
    let start = start_world(&assets, seed, &save_dir)?;
    let mut transport = ChannelTransport::new();
    let endpoint = transport.connect();
    let handle = spawn_host_thread(start.into_host(Box::new(transport)), HOST_HZ);
    info!(hz = HOST_HZ, windowed, "host thread started");

    app.insert_resource(CameraStart::overview(assets.worldgen.bounds.aabb()))
        .add_plugins((
            NetPlugin,
            ReplicaPlugin,
            MeshPipelinePlugin,
            GameRenderPlugin,
            FlyCamPlugin,
            ControllerPlugin,
            PickaxePlugin,
            HudPlugin,
            EditorPlugin,
            SavePlugin,
            SessionPlugin,
            SpawnPlugin,
        ));
    if windowed {
        app.add_plugins((EditorPanelPlugin, CursorPlugin));
    }
    app.insert_resource(ClientMaterials(assets.materials.clone()))
        .insert_resource(Pickaxe(assets.tools.pickaxe))
        .insert_resource(HostConnection::new(EndpointLink(endpoint)))
        .insert_resource(GameHost::new(handle, Arc::new(assets), save_dir));
    Ok(app)
}

/// The primary window: titled, closed by its close button, which ends the
/// app (and so saves).
pub fn window_plugin() -> WindowPlugin {
    WindowPlugin {
        primary_window: Some(Window {
            title: GAME_TITLE.into(),
            ..default()
        }),
        exit_condition: ExitCondition::OnPrimaryClosed,
        close_when_requested: true,
        ..default()
    }
}

/// Spawns the local player once the world is in, and puts it back at the
/// spawn whenever the host sends a new world (a `Welcome`: the editor's
/// Load or Regenerate).
pub struct SpawnPlugin;

impl Plugin for SpawnPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingSpawn>()
            .add_systems(Update, place_player);
    }
}

/// Whether the player is still to be placed at [`spawn_feet`] in the
/// current world. Set at start and by every `Welcome`; cleared once placed.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSpawn(pub bool);

impl Default for PendingSpawn {
    fn default() -> Self {
        Self(true)
    }
}

/// Places the [`Player`] at [`spawn_feet`] once the world it is waiting
/// for is in: spawned the first time, moved (and turned to look along -Z)
/// after.
fn place_player(
    mut commands: Commands,
    mut inbox: MessageReader<FromHost>,
    mut pending: ResMut<PendingSpawn>,
    replica: Option<Res<Replica>>,
    mut players: Query<&mut Player>,
) {
    if inbox
        .read()
        .any(|FromHost(msg)| matches!(msg, ServerMsg::Welcome { .. }))
    {
        pending.0 = true;
    }
    if !pending.0 {
        return;
    }
    let Some(feet) = replica.and_then(|r| spawn_feet(r.world())) else {
        return;
    };
    pending.0 = false;
    match players.iter_mut().next() {
        Some(mut player) => {
            info!(?feet, "player moved to the spawn of the new world");
            *player = Player::at(feet);
        }
        None => {
            info!(?feet, "player spawned");
            commands.spawn(Player::at(feet));
        }
    }
}

/// Half a cell, in cells: the offset from a cell's corner to its middle.
const HALF: f32 = 0.5;

/// Where the player starts in `world`: on the topmost solid cell of the
/// column at the middle of the bounds, in the middle of that cell (on the
/// bottom of the bounds if the column has no solid cell). `None` until
/// every chunk of the bounds is resident.
pub fn spawn_feet(world: &CellWorld) -> Option<Vec3> {
    let bounds = world.bounds();
    if !bounds.chunks().all(|pos| world.chunk(pos).is_some()) {
        return None;
    }
    let (min, max) = (bounds.min.0, bounds.max.0);
    let x = min.x + (max.x - min.x + 1) / 2;
    let z = min.z + (max.z - min.z + 1) / 2;
    let y = (min.y..=max.y)
        .rev()
        .find(|&y| world.is_solid(CellPos::new(x, y, z)))
        .map_or(min.y, |top| top + 1);
    Some(Vec3::new(
        (x as f32 + HALF) * CELL_SIZE_M,
        y as f32 * CELL_SIZE_M,
        (z as f32 + HALF) * CELL_SIZE_M,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use bevy::app::PluginsState;
    use bevy::winit::WinitPlugin;
    use rmc_client::controller::SKIN_M;
    use rmc_client::{
        ClientMaterials, ControllerPlugin, EditorPlugin, EditorState, FlyCamPlugin,
        GameRenderPlugin, HostConnection, HudPlugin, MeshPipelinePlugin, NetPlugin, Pickaxe,
        PickaxePlugin, SavePlugin, ToHost,
    };
    use rmc_protocol::ClientMsg;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, PlayerId};
    use rmc_world::shape::Shape;
    use rmc_worldgen::params::WorldBounds;

    use crate::session::{DEFAULT_SEED, GameHost, SessionPlugin, generate_world, has_save};

    /// A save directory under the system temp dir, removed on drop. It is
    /// not created: an absent directory holds no save.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rmc_app_app_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `assets/` with a 2 × 4 × 2-chunk world, so generation is quick.
    fn small_config(seed: Option<u64>, dir: &Path) -> GameConfig {
        let mut assets = GameAssets::load(&assets_dir()).expect("assets load");
        assets.worldgen.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [63, 127, 63],
        };
        GameConfig {
            assets,
            seed,
            save_dir: dir.to_path_buf(),
        }
    }

    /// What `App::run` does before its first update.
    fn finish(app: &mut App) {
        while app.plugins_state() == PluginsState::Adding {
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();
    }

    /// Steps `app` until `done` holds, sleeping a little between frames so
    /// the host thread can answer and real time passes. Panics after 20 s.
    fn step_until(app: &mut App, what: &str, done: impl Fn(&mut App) -> bool) {
        let start = Instant::now();
        while !done(app) {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(2));
            app.update();
        }
    }

    fn replica(app: &App) -> Option<&Replica> {
        app.world().get_resource::<Replica>()
    }

    fn player(app: &mut App) -> Option<Player> {
        let world = app.world_mut();
        world.query::<&Player>().iter(world).next().cloned()
    }

    fn player_count(app: &mut App) -> usize {
        let world = app.world_mut();
        world.query::<&Player>().iter(world).count()
    }

    #[test]
    fn game_app_builds_headless() {
        let dir = TempDir::new("builds");
        let config = small_config(Some(7), &dir.0);
        let assets = config.assets.clone();
        let mut app = build_game_app(config, false).expect("the game app builds");

        // Every client plugin, the session and the spawn.
        assert!(app.is_plugin_added::<NetPlugin>());
        assert!(app.is_plugin_added::<ReplicaPlugin>());
        assert!(app.is_plugin_added::<MeshPipelinePlugin>());
        assert!(app.is_plugin_added::<GameRenderPlugin>());
        assert!(app.is_plugin_added::<FlyCamPlugin>());
        assert!(app.is_plugin_added::<ControllerPlugin>());
        assert!(app.is_plugin_added::<PickaxePlugin>());
        assert!(app.is_plugin_added::<HudPlugin>());
        assert!(app.is_plugin_added::<EditorPlugin>());
        assert!(app.is_plugin_added::<SavePlugin>());
        assert!(app.is_plugin_added::<SessionPlugin>());
        assert!(app.is_plugin_added::<SpawnPlugin>());
        // No window (rule 12), no winit.
        assert!(!app.is_plugin_added::<WinitPlugin>());
        assert!(!app.is_plugin_added::<WindowPlugin>());

        // The host thread runs, and the client has what its plugins read.
        assert!(app.world().resource::<GameHost>().is_running());
        assert!(app.world().contains_resource::<HostConnection>());
        assert!(app.world().contains_resource::<EditorState>());
        assert_eq!(
            app.world().resource::<ClientMaterials>().0.hash(),
            assets.materials.hash()
        );
        assert_eq!(
            *app.world().resource::<Pickaxe>(),
            Pickaxe(assets.tools.pickaxe)
        );
        finish(&mut app);
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&Window>().iter(world).count(), 0);

        // The windowed game's window: titled, and closing it ends the app.
        let window = window_plugin();
        let primary = window.primary_window.expect("a primary window");
        assert_eq!(primary.title, GAME_TITLE);
        assert!(matches!(
            window.exit_condition,
            ExitCondition::OnPrimaryClosed
        ));
        assert!(window.close_when_requested);

        // Dropping the app stops the host and saves; a different seed for
        // that save fails the next build loudly.
        drop(app);
        assert!(has_save(&dir.0), "saved when the app was dropped");
        let Err(err) = build_game_app(small_config(Some(8), &dir.0), false) else {
            panic!("seed mismatch refused");
        };
        let err = format!("{err:#}");
        assert!(err.to_lowercase().contains("seed mismatch"), "{err}");
        // So do other worldgen params.
        let mut bad = small_config(Some(7), &dir.0);
        bad.assets.worldgen.surface.base_height += 1;
        assert!(build_game_app(bad, false).is_err());
        let from_cli = GameConfig::from_cli(&Cli {
            seed: None,
            save: dir.0.clone(),
        })
        .expect("assets_dir() holds the tables");
        assert_eq!(from_cli.save_dir, dir.0);
        assert_eq!(from_cli.seed, None);
    }

    #[test]
    fn headless_game_joins_and_receives_chunks() {
        let dir = TempDir::new("joins");
        let config = small_config(None, &dir.0);
        let assets = config.assets.clone();
        let generated = generate_world(&assets, DEFAULT_SEED).unwrap();
        let mut app = build_game_app(config, false).unwrap();
        finish(&mut app);

        // The player joins and the replica receives every chunk.
        let want = generated.hash();
        step_until(&mut app, "every chunk", |app| {
            replica(app).is_some_and(|r| r.world().hash() == want)
        });
        let replica_world = replica(&app).unwrap().world();
        assert_eq!(replica_world.chunk_count(), 16);
        assert_eq!(replica_world.bounds(), generated.bounds());

        // The player is spawned once, on the surface at the middle.
        step_until(&mut app, "the player", |app| player_count(app) == 1);
        let feet = spawn_feet(&generated).expect("a spawn in the full world");
        assert_eq!(spawn_feet(replica(&app).unwrap().world()), Some(feet));
        let (x, z) = (32, 32);
        let top = (0..=127)
            .rev()
            .find(|&y| generated.is_solid(CellPos::new(x, y, z)))
            .expect("a solid cell in the middle column");
        assert_eq!(
            feet,
            Vec3::new(
                (x as f32 + 0.5) * CELL_SIZE_M,
                (top + 1) as f32 * CELL_SIZE_M,
                (z as f32 + 0.5) * CELL_SIZE_M,
            )
        );
        assert_eq!(player(&mut app).unwrap().feet, feet);
        // It stands there, grounded (the controller rests SKIN_M above the
        // floor). The spawn column is free, but the box also reaches into
        // the neighbouring columns, and where those hold rock the controller
        // pushes the player out to the nearest free spot (F086), which is
        // free_feet_near's (the spawn itself when the box is free).
        let rest =
            rmc_client::free_feet_near(&generated, feet).expect("a free spot near the spawn");
        step_until(&mut app, "grounded", |app| {
            player(app).is_some_and(|p| p.grounded)
        });
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(2));
            app.update();
        }
        let p = player(&mut app).unwrap();
        assert!(p.grounded);
        assert!(
            (p.feet - rest).length() <= 2.0 * SKIN_M,
            "{:?} vs {rest:?}",
            p.feet
        );
        assert_eq!(player_count(&mut app), 1, "spawned once");

        // An edit goes through the host thread and comes back.
        let cell = CellPos::new(10, 125, 10);
        let granite = generated.materials().id("granite").unwrap();
        app.world_mut()
            .write_message(ToHost(ClientMsg::Edit(EditCommand {
                id: CommandId {
                    issuer: PlayerId(0),
                    seq: 0,
                },
                issuer: PlayerId(0),
                op: EditOp::Set(granite),
                shape: Shape::Cell(cell),
            })));
        step_until(&mut app, "the edit", |app| {
            replica(app).and_then(|r| r.world().get_cell(cell)) == Some(granite)
        });

        // Quitting stops the host and saves; the next game loads the save.
        app.world_mut().write_message(AppExit::Success);
        app.update();
        assert!(!app.world().resource::<GameHost>().is_running());
        assert!(has_save(&dir.0));
        drop(app);
        let mut again = build_game_app(small_config(None, &dir.0), false).unwrap();
        finish(&mut again);
        step_until(&mut again, "the saved world", |app| {
            replica(app).is_some_and(|r| r.world().chunk_count() == 16)
        });
        assert_eq!(
            replica(&again).unwrap().world().get_cell(cell),
            Some(granite)
        );
    }

    #[test]
    fn new_world_re_places_the_player() {
        let dir = TempDir::new("re_place");
        let config = small_config(None, &dir.0);
        let assets = config.assets.clone();
        let mut app = build_game_app(config, false).unwrap();
        finish(&mut app);
        let first = generate_world(&assets, DEFAULT_SEED).unwrap();
        step_until(&mut app, "the player", |app| player_count(app) == 1);
        let spawn = spawn_feet(&first).unwrap();

        // Walk away (put the player somewhere else, looking elsewhere).
        let away = spawn + Vec3::new(4.0, 6.0, -3.0);
        {
            let world = app.world_mut();
            let mut player = world.query::<&mut Player>().single_mut(world).unwrap();
            player.feet = away;
            player.yaw = 1.0;
        }
        app.update();
        assert_ne!(player(&mut app).unwrap().feet, spawn);

        // Regenerate from another seed: once the new world is in, the
        // same player stands at the new world's spawn.
        let seed = 5;
        let second = generate_world(&assets, seed).unwrap();
        let new_spawn = spawn_feet(&second).unwrap();
        {
            let mut editor = app.world_mut().resource_mut::<EditorState>();
            editor.seed = seed;
            editor.regenerate_requested = true;
        }
        let want = second.hash();
        step_until(&mut app, "the new world", |app| {
            replica(app).is_some_and(|r| r.world().hash() == want)
        });
        step_until(&mut app, "the re-placed player", |app| {
            player(app).is_some_and(|p| (p.feet - new_spawn).length() <= 2.0 * SKIN_M)
        });
        assert_eq!(player_count(&mut app), 1, "re-placed, not spawned again");
        assert_eq!(player(&mut app).unwrap().yaw, 0.0);

        // Save, walk away again, then Load: back at the spawn.
        app.world_mut().resource_mut::<EditorState>().save_requested = true;
        step_until(&mut app, "the save", |_| has_save(&dir.0));
        {
            let world = app.world_mut();
            world.query::<&mut Player>().single_mut(world).unwrap().feet = away;
        }
        app.update();
        assert!((player(&mut app).unwrap().feet - new_spawn).length() > 1.0);
        app.world_mut().resource_mut::<EditorState>().load_requested = true;
        step_until(&mut app, "the player back at the spawn", |app| {
            player(app).is_some_and(|p| (p.feet - new_spawn).length() <= 2.0 * SKIN_M)
        });
        assert_eq!(replica(&app).unwrap().world().hash(), want);
        // A player moved after the world is in stays where it was put.
        {
            let world = app.world_mut();
            world.query::<&mut Player>().single_mut(world).unwrap().feet = away;
        }
        for _ in 0..5 {
            app.update();
        }
        assert_ne!(player(&mut app).unwrap().feet, new_spawn);
    }
}
