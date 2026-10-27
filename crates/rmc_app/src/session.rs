//! The world's life in the app (SAD §10): load or generate it on start, run
//! the host on its own thread, load the save again when the editor asks,
//! and save when the app quits.
//!
//! - [`start_world`]: if the save directory holds a save, generate the world
//!   from the save's seed and overwrite it with the saved chunks; any header
//!   mismatch fails loudly. Otherwise generate from the seed.
//! - [`GameHost`]: the running host thread, as a resource of the client app.
//!   [`SessionPlugin`] hands the editor's Load action
//!   ([`EditorState::load_requested`]) to the host thread, which loads the
//!   save and replaces its world ([`Host::replace_world`]), so every client
//!   rebuilds its replica from the new `Welcome` and snapshots. The
//!   editor's Regenerate ([`EditorState::regenerate_requested`]) goes the
//!   same way, with a world generated from [`EditorState::seed`]. On
//!   [`AppExit`] it stops the host and saves ([`quit_save`]); dropping a
//!   [`GameHost`] that still runs does the same.
//!
//! The editor's Save and the autosave timer are `SaveRequest`s from the
//! client (`rmc_client::SavePlugin`); the host answers them itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use bevy::prelude::{
    App, AppExit, IntoScheduleConfigs, Last, MessageReader, Plugin, Res, ResMut, Resource,
    SystemSet, Update,
};
use rmc_client::{ClientEco, ClientSeasons, EditorState, EditorSystems, HostLink, SeasonView};
use rmc_eco::{EcoTables, ToxKind};
use rmc_host::{ClientEndpoint, Host, HostConfig, HostHandle, SaveConfig};
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::ToolTable;
use rmc_world::coords::CellAabb;
use rmc_world::items::ItemTable;
use rmc_world::materials::MaterialTable;
use rmc_world::save::{self, HEADER_FILE, SaveHeader};
use rmc_world::world::World as CellWorld;
use rmc_worldgen::params::WorldBounds;
use rmc_worldgen::{
    GenParams, Generator, LightInit, MagmaInit, NOISE_CRATE_VERSION, PlantInit, TemperatureInit,
    WaterTableInit,
};
use tracing::{error, info, warn};

/// The seed when there is neither a `--seed` nor a save to take one from.
pub const DEFAULT_SEED: u64 = 1;

/// Host tick rate for single player (SAD §8.5).
pub const HOST_HZ: u32 = 60;

/// The data tables the game is built from (`assets/`).
#[derive(Clone, Debug)]
pub struct GameAssets {
    pub materials: Arc<MaterialTable>,
    pub tools: ToolTable,
    /// Items, the bag and the starting kit (`items.ron`, `docs/ITEMS.md`).
    pub items: ItemTable,
    pub worldgen: GenParams,
    /// The Phase 2 tables, built on `materials`: the host's seasons and the
    /// generated temperature layer.
    pub eco: Arc<EcoTables>,
    /// [`EcoTables::hash`] of the Phase 2 tables, for the save header.
    pub tables_hash: u64,
}

impl GameAssets {
    /// Loads `materials.ron`, `tools.ron`, `items.ron`, `worldgen.ron` and
    /// the Phase 2 tables from `dir`.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let materials = MaterialTable::load(dir.join("materials.ron"))
            .with_context(|| format!("loading {}", dir.join("materials.ron").display()))?;
        let tools = ToolTable::load(dir.join("tools.ron"))
            .with_context(|| format!("loading {}", dir.join("tools.ron").display()))?;
        let items = ItemTable::load(dir.join("items.ron"))
            .with_context(|| format!("loading {}", dir.join("items.ron").display()))?;
        items
            .check_tools(&tools)
            .with_context(|| format!("checking {}", dir.join("items.ron").display()))?;
        let worldgen = GenParams::load(dir.join("worldgen.ron"))
            .with_context(|| format!("loading {}", dir.join("worldgen.ron").display()))?;
        let materials = Arc::new(materials);
        let eco = EcoTables::load_with_materials(materials.clone(), dir)
            .with_context(|| format!("loading the Phase 2 tables in {}", dir.display()))?;
        Ok(Self {
            materials,
            tools,
            items,
            worldgen,
            tables_hash: eco.hash(),
            eco: Arc::new(eco),
        })
    }

    /// These tables for a world of `bounds`: `worldgen.ron` fitted to them
    /// ([`GenParams::fit_to`]: the surface keeps its share of the height).
    pub fn with_world_bounds(mut self, bounds: CellAabb) -> anyhow::Result<Self> {
        let bounds = WorldBounds {
            min: bounds.min.0.to_array(),
            max: bounds.max.0.to_array(),
        };
        self.worldgen = self
            .worldgen
            .fit_to(bounds)
            .with_context(|| format!("worldgen params for the world {bounds:?}"))?;
        Ok(self)
    }

    /// What `Welcome` reports for a world generated from `seed`.
    pub fn host_config(&self, seed: u64) -> HostConfig {
        HostConfig {
            seed,
            gen_params_hash: self.worldgen.hash(),
        }
    }

    /// The header a save of a world generated from `seed` carries.
    pub fn save_header(&self, seed: u64) -> SaveHeader {
        SaveHeader::new(
            seed,
            self.worldgen.bounds.aabb(),
            self.materials.hash(),
            self.tables_hash,
            self.worldgen.hash(),
            NOISE_CRATE_VERSION,
        )
    }
}

/// `assets/` under the working directory if it holds the tables (as under
/// `cargo run` from the workspace), else the workspace's `assets/`.
pub fn assets_dir() -> PathBuf {
    let local = PathBuf::from("assets");
    if local.join("materials.ron").is_file() {
        local
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets")
    }
}

/// Every chunk in the bounds of `assets.worldgen`, generated from `seed`,
/// with its temperature layer, water table, magma chambers, light layer
/// and surface plants (SAD-P2 §8).
pub fn generate_world(assets: &GameAssets, seed: u64) -> anyhow::Result<CellWorld> {
    let generator = Generator::new(seed, &assets.worldgen, &assets.materials)
        .context("worldgen params do not fit the material table")?
        .with_temperature(TemperatureInit::new(&assets.eco))
        .with_water_table(WaterTableInit::new(&assets.eco))
        .with_magma(MagmaInit::new(&assets.eco))
        .with_light(LightInit::new(&assets.eco))
        .with_plants(PlantInit::new(&assets.eco));
    let mut world = CellWorld::new(assets.materials.clone(), generator.bounds());
    for (pos, chunk) in generator.generate_world() {
        world
            .insert_chunk(pos, chunk)
            .context("generated chunk outside the bounds")?;
    }
    Ok(world)
}

/// Whether `dir` holds a save (its header file).
pub fn has_save(dir: &Path) -> bool {
    dir.join(HEADER_FILE).is_file()
}

/// Loads the save in `dir` into a world generated from `seed`. The save's
/// header must match what `assets` and `seed` build, field for field.
pub fn load_world(assets: &GameAssets, seed: u64, dir: &Path) -> anyhow::Result<CellWorld> {
    let mut generated: Option<anyhow::Error> = None;
    let world = save::load(dir, &assets.save_header(seed), || {
        generate_world(assets, seed).unwrap_or_else(|e| {
            generated = Some(e);
            CellWorld::new(assets.materials.clone(), assets.worldgen.bounds.aabb())
        })
    });
    if let Some(e) = generated {
        return Err(e);
    }
    world.with_context(|| format!("loading the save in {}", dir.display()))
}

/// Where the starting world came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldOrigin {
    Generated,
    Loaded,
}

/// The world the host starts with, and what it needs besides.
pub struct StartWorld {
    pub world: CellWorld,
    pub host: HostConfig,
    pub save: SaveConfig,
    pub origin: WorldOrigin,
    /// The tables the host runs seasons with.
    pub eco: Arc<EcoTables>,
}

impl StartWorld {
    /// A host over this world that saves to its save directory and runs
    /// seasons.
    pub fn into_host(self, transport: Box<dyn rmc_host::Transport>) -> Host {
        Host::new(self.world, self.host, transport)
            .with_save(self.save)
            .with_eco(self.eco)
    }
}

/// The world to start with: the save in `save_dir` if there is one, else a
/// world generated from `seed`. The seed defaults to the save's, else
/// [`DEFAULT_SEED`]; a seed that differs from the save's fails.
pub fn start_world(
    assets: &GameAssets,
    seed: Option<u64>,
    save_dir: &Path,
) -> anyhow::Result<StartWorld> {
    let save = SaveConfig {
        dir: save_dir.to_path_buf(),
        noise_crate_version: NOISE_CRATE_VERSION.to_string(),
        tables_hash: assets.tables_hash,
    };
    let (world, seed, origin) = if has_save(save_dir) {
        let found = save::read_header(save_dir)
            .with_context(|| format!("reading the save in {}", save_dir.display()))?;
        let seed = seed.unwrap_or(found.seed);
        (
            load_world(assets, seed, save_dir)?,
            seed,
            WorldOrigin::Loaded,
        )
    } else {
        let seed = seed.unwrap_or(DEFAULT_SEED);
        (generate_world(assets, seed)?, seed, WorldOrigin::Generated)
    };
    info!(
        seed,
        ?origin,
        dir = %save_dir.display(),
        chunks = world.chunk_count(),
        "world ready"
    );
    Ok(StartWorld {
        world,
        host: assets.host_config(seed),
        save,
        origin,
        eco: assets.eco.clone(),
    })
}

/// Stops the host thread and saves the world as the host left it. Returns
/// how many chunks were written.
pub fn quit_save(handle: HostHandle) -> anyhow::Result<usize> {
    let host = handle.stop();
    let chunks = host.save().context("saving on quit")?;
    info!(chunks, "saved on quit");
    Ok(chunks)
}

/// The client's end of a `ChannelTransport`.
pub struct EndpointLink(pub ClientEndpoint);

impl HostLink for EndpointLink {
    fn send(&self, msg: ClientMsg) -> bool {
        self.0.send(msg).is_ok()
    }

    fn drain(&self) -> Vec<ServerMsg> {
        self.0.drain()
    }
}

/// The running host thread and what loading a save needs.
#[derive(Resource)]
pub struct GameHost {
    handle: Option<HostHandle>,
    assets: Arc<GameAssets>,
    save_dir: PathBuf,
}

impl GameHost {
    pub fn new(handle: HostHandle, assets: Arc<GameAssets>, save_dir: PathBuf) -> Self {
        Self {
            handle: Some(handle),
            assets,
            save_dir,
        }
    }

    /// Whether the host thread still runs (it stops on quit).
    pub fn is_running(&self) -> bool {
        self.handle.is_some()
    }

    /// Asks the host thread to load the save and replace its world with it.
    /// A missing or mismatched save is logged and changes nothing. Returns
    /// false if the host has stopped.
    pub fn request_load(&self) -> bool {
        let Some(handle) = &self.handle else {
            return false;
        };
        let assets = self.assets.clone();
        let dir = self.save_dir.clone();
        handle.submit(move |host: &mut Host| {
            let seed = host.config().seed;
            match load_world(&assets, seed, &dir) {
                Ok(world) => {
                    info!(dir = %dir.display(), seed, "save loaded on request");
                    host.replace_world(world, assets.host_config(seed));
                }
                Err(e) => warn!("load refused: {e:#}"),
            }
        })
    }

    /// Asks the host thread to generate the world from `seed` and replace
    /// its world with it; later saves carry `seed`. A failed generation is
    /// logged and changes nothing. Returns false if the host has stopped.
    pub fn request_regenerate(&self, seed: u64) -> bool {
        let Some(handle) = &self.handle else {
            return false;
        };
        let assets = self.assets.clone();
        handle.submit(move |host: &mut Host| match generate_world(&assets, seed) {
            Ok(world) => {
                info!(seed, "world regenerated on request");
                host.replace_world(world, assets.host_config(seed));
            }
            Err(e) => warn!("regenerate refused: {e:#}"),
        })
    }

    /// Runs `job` on the host thread. Returns false if the host has
    /// stopped.
    pub fn submit(&self, job: impl FnOnce(&mut Host) + Send + 'static) -> bool {
        self.handle.as_ref().is_some_and(|h| h.submit(job))
    }

    /// Stops the host and saves, once; later calls do nothing.
    pub fn quit_save(&mut self) -> Option<anyhow::Result<usize>> {
        self.handle.take().map(quit_save)
    }
}

impl Drop for GameHost {
    fn drop(&mut self) {
        if let Some(Err(e)) = self.quit_save() {
            error!("{e:#}");
        }
    }
}

/// [`SessionPlugin`]'s system set.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionSystems;

/// Hands the editor's Load and Regenerate to the host thread and saves on
/// quit.
pub struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (load_on_request, regenerate_on_request)
                .in_set(SessionSystems)
                .after(EditorSystems),
        )
        .add_systems(Last, save_on_exit.in_set(SessionSystems));
    }
}

/// Takes the editor's Load request and passes it to the host thread.
fn load_on_request(editor: Option<ResMut<EditorState>>, host: Option<Res<GameHost>>) {
    let Some(mut editor) = editor else {
        return;
    };
    if !std::mem::take(&mut editor.load_requested) {
        return;
    }
    match host {
        Some(host) if host.request_load() => info!("load requested"),
        _ => warn!("load requested, but no host is running"),
    }
}

/// Takes the editor's Regenerate request and passes it, with the editor's
/// seed, to the host thread.
fn regenerate_on_request(editor: Option<ResMut<EditorState>>, host: Option<Res<GameHost>>) {
    let Some(mut editor) = editor else {
        return;
    };
    if !std::mem::take(&mut editor.regenerate_requested) {
        return;
    }
    let seed = editor.seed;
    match host {
        Some(host) if host.request_regenerate(seed) => info!(seed, "regenerate requested"),
        _ => warn!("regenerate requested, but no host is running"),
    }
}

/// On [`AppExit`], stops the host and saves.
fn save_on_exit(mut exit: MessageReader<AppExit>, host: Option<ResMut<GameHost>>) {
    if exit.is_empty() {
        return;
    }
    exit.clear();
    if let Some(Err(e)) = host.and_then(|mut h| h.quit_save()) {
        error!("{e:#}");
    }
}

/// What the client reads of the eco tables (SAD-P2 §10): substance and gas
/// keys, the pH rule, the toxicity profiles, the water tint, the gas
/// tint, the flammable gases and the plant species keys.
pub fn client_eco(eco: &EcoTables) -> ClientEco {
    ClientEco {
        substances: eco.substance_layers().clone(),
        gases: eco.gases().iter().map(|g| g.key.clone()).collect(),
        ph: eco.ph_rule(),
        player_tox: eco.tox_profile(ToxKind::Player),
        plant_tox: eco.tox_profile(ToxKind::Plant),
        tint: eco.substance_tints(),
        gas_tint: eco.gas_tints(),
        flammable: eco.flammable_gases(),
        plants: eco.plants().iter().map(|p| p.key.clone()).collect(),
    }
}

/// The season rows the client's weather reads (`rmc_client::weather`).
pub fn client_seasons(eco: &EcoTables) -> ClientSeasons {
    ClientSeasons {
        rows: eco
            .seasons()
            .seasons
            .iter()
            .map(|s| SeasonView {
                key: s.key.clone(),
                air_temp: s.air_temp,
                rain: s.rain,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use bevy::prelude::MinimalPlugins;

    use rmc_client::{
        ClientMaterials, HostConnection, NetPlugin, Replica, ReplicaPlugin, SaveOutcome,
        SavePlugin, SaveStatus, ToHost,
    };
    use rmc_host::{ChannelTransport, spawn_host_thread};
    use rmc_world::coords::CellPos;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, PlayerId};
    use rmc_world::materials::MaterialId;
    use rmc_world::shape::Shape;
    use rmc_worldgen::params::WorldBounds;

    /// A save directory under the system temp dir, removed on drop. It is
    /// not created: an absent directory holds no save.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rmc_app_session_{name}_{}", std::process::id()));
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
    fn small_assets() -> GameAssets {
        let mut assets = GameAssets::load(&assets_dir()).expect("assets load");
        assets.worldgen.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [63, 127, 63],
        };
        assets
    }

    fn set(world: &CellWorld, seq: u32, cell: CellPos, key: &str) -> EditCommand {
        EditCommand {
            id: CommandId {
                issuer: PlayerId(0),
                seq,
            },
            issuer: PlayerId(0),
            op: EditOp::Set(world.materials().id(key).expect("material")),
            shape: Shape::Cell(cell),
        }
    }

    /// Steps `app` until `done` holds, sleeping a little between frames so
    /// the host thread can answer. Panics after 20 s.
    fn step_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
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

    fn replica_cell(app: &App, cell: CellPos) -> Option<MaterialId> {
        app.world()
            .get_resource::<Replica>()
            .and_then(|r| r.world().get_cell(cell))
    }

    fn replica_hash(app: &App) -> Option<u64> {
        app.world()
            .get_resource::<Replica>()
            .map(|r| r.world().hash())
    }

    fn send_edit(app: &mut App, cmd: EditCommand) {
        app.world_mut().write_message(ToHost(ClientMsg::Edit(cmd)));
    }

    #[test]
    fn load_request_reloads_saved_world() {
        let assets = small_assets();
        let dir = TempDir::new("load_request");

        // No save: generated from the default seed.
        let start = start_world(&assets, None, &dir.0).unwrap();
        assert_eq!(start.origin, WorldOrigin::Generated);
        assert_eq!(start.host, assets.host_config(DEFAULT_SEED));
        assert_eq!(start.save.dir, dir.0);
        assert_eq!(start.world.chunk_count(), 16);
        let generated = start.world.clone();
        assert_eq!(
            generated.hash(),
            generate_world(&assets, DEFAULT_SEED).unwrap().hash()
        );

        let mut transport = ChannelTransport::new();
        let endpoint = transport.connect();
        let handle = spawn_host_thread(start.into_host(Box::new(transport)), HOST_HZ);

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            NetPlugin,
            ReplicaPlugin,
            SavePlugin,
            SessionPlugin,
        ))
        .init_resource::<EditorState>()
        .insert_resource(ClientMaterials(assets.materials.clone()))
        .insert_resource(HostConnection::new(EndpointLink(endpoint)))
        .insert_resource(GameHost::new(
            handle,
            Arc::new(assets.clone()),
            dir.0.clone(),
        ));
        app.update();
        let want = generated.hash();
        step_until(&mut app, "the join", |app| replica_hash(app) == Some(want));

        // Three cells in the air above the surface.
        let (a, b, c) = (
            CellPos::new(20, 120, 20),
            CellPos::new(40, 121, 40),
            CellPos::new(21, 122, 41),
        );
        let granite = generated.materials().id("granite").unwrap();
        send_edit(&mut app, set(&generated, 0, a, "granite"));
        step_until(&mut app, "edit A", |app| {
            replica_cell(app, a) == Some(granite)
        });

        // Load with no save: logged, nothing replaced. The job runs before
        // the host's next tick, so once edit B is applied it has run.
        app.world_mut().resource_mut::<EditorState>().load_requested = true;
        app.update();
        assert!(
            !app.world().resource::<EditorState>().load_requested,
            "taken"
        );
        send_edit(&mut app, set(&generated, 1, b, "granite"));
        step_until(&mut app, "edit B", |app| {
            replica_cell(app, b) == Some(granite)
        });
        assert_eq!(
            replica_cell(&app, a),
            Some(granite),
            "no reload without a save"
        );
        // Undo B so the save holds A alone.
        send_edit(&mut app, set(&generated, 2, b, "air"));
        step_until(&mut app, "B undone", |app| {
            replica_cell(app, b) == Some(MaterialId::AIR)
        });

        // Save through the editor: the host writes the directory.
        app.world_mut().resource_mut::<EditorState>().save_requested = true;
        let path = dir.0.display().to_string();
        step_until(&mut app, "Saved", |app| {
            app.world().resource::<SaveStatus>().last
                == Some(SaveOutcome {
                    ok: true,
                    path: path.clone(),
                })
        });
        assert!(has_save(&dir.0));
        let saved = load_world(&assets, DEFAULT_SEED, &dir.0).unwrap();
        assert_eq!(saved.get_cell(a), Some(granite));
        assert_eq!(saved.get_cell(b), Some(MaterialId::AIR));

        // B again, then Load: the host replaces its world with the save and
        // the replica is rebuilt from it.
        send_edit(&mut app, set(&generated, 3, b, "granite"));
        step_until(&mut app, "edit B again", |app| {
            replica_cell(app, b) == Some(granite)
        });
        app.world_mut().resource_mut::<EditorState>().load_requested = true;
        let saved_hash = saved.hash();
        step_until(&mut app, "the reload", |app| {
            replica_hash(app) == Some(saved_hash)
        });
        assert!(!app.world().resource::<EditorState>().load_requested);
        assert_eq!(replica_cell(&app, b), Some(MaterialId::AIR));
        assert_eq!(replica_cell(&app, a), Some(granite));

        // C, then quit: the host stops and saves.
        send_edit(&mut app, set(&generated, 4, c, "granite"));
        step_until(&mut app, "edit C", |app| {
            replica_cell(app, c) == Some(granite)
        });
        assert!(app.world().resource::<GameHost>().is_running());
        app.world_mut().write_message(AppExit::Success);
        app.update();
        assert!(
            !app.world().resource::<GameHost>().is_running(),
            "stopped on exit"
        );
        assert!(
            app.world_mut()
                .resource_mut::<GameHost>()
                .quit_save()
                .is_none()
        );
        assert!(!app.world().resource::<GameHost>().request_load());

        // The next start loads the save: A and C, not B, at the save's seed.
        let again = start_world(&assets, None, &dir.0).unwrap();
        assert_eq!(again.origin, WorldOrigin::Loaded);
        assert_eq!(again.host, assets.host_config(DEFAULT_SEED));
        assert_eq!(again.world.get_cell(a), Some(granite));
        assert_eq!(again.world.get_cell(b), Some(MaterialId::AIR));
        assert_eq!(again.world.get_cell(c), Some(granite));
        assert_eq!(Some(again.world.hash()), replica_hash(&app));
        // A seed other than the save's fails loudly.
        let err = start_world(&assets, Some(DEFAULT_SEED + 1), &dir.0)
            .err()
            .expect("seed mismatch refused");
        let err = format!("{err:#}");
        assert!(err.to_lowercase().contains("seed mismatch"), "{err}");
        // So do other worldgen params.
        let mut other = assets.clone();
        other.worldgen.surface.base_height += 1;
        assert!(start_world(&other, None, &dir.0).is_err());
    }

    #[test]
    fn regenerate_request_replaces_world() {
        let assets = small_assets();
        let dir = TempDir::new("regenerate");
        let start = start_world(&assets, None, &dir.0).unwrap();
        let mut transport = ChannelTransport::new();
        let endpoint = transport.connect();
        let handle = spawn_host_thread(start.into_host(Box::new(transport)), HOST_HZ);

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            NetPlugin,
            ReplicaPlugin,
            SavePlugin,
            SessionPlugin,
        ))
        .init_resource::<EditorState>()
        .insert_resource(ClientMaterials(assets.materials.clone()))
        .insert_resource(HostConnection::new(EndpointLink(endpoint)))
        .insert_resource(GameHost::new(
            handle,
            Arc::new(assets.clone()),
            dir.0.clone(),
        ));
        app.update();
        let first = generate_world(&assets, DEFAULT_SEED).unwrap().hash();
        step_until(&mut app, "the join", |app| replica_hash(app) == Some(first));

        // Regenerate from the editor's seed: the replica gets that world.
        let seed = 5;
        let regenerated = generate_world(&assets, seed).unwrap().hash();
        assert_ne!(regenerated, first);
        {
            let mut editor = app.world_mut().resource_mut::<EditorState>();
            editor.seed = seed;
            editor.regenerate_requested = true;
        }
        step_until(&mut app, "the regenerated world", |app| {
            replica_hash(app) == Some(regenerated)
        });
        assert!(
            !app.world().resource::<EditorState>().regenerate_requested,
            "taken"
        );

        // A save now carries the new seed, and the next start loads it.
        app.world_mut().resource_mut::<EditorState>().save_requested = true;
        step_until(&mut app, "Saved", |app| {
            app.world()
                .resource::<SaveStatus>()
                .last
                .as_ref()
                .is_some_and(|s| s.ok)
        });
        assert_eq!(save::read_header(&dir.0).unwrap().seed, seed);
        app.world_mut().write_message(AppExit::Success);
        app.update();
        assert!(!app.world().resource::<GameHost>().request_regenerate(seed));
        let again = start_world(&assets, None, &dir.0).unwrap();
        assert_eq!(again.origin, WorldOrigin::Loaded);
        assert_eq!(again.host, assets.host_config(seed));
        assert_eq!(again.world.hash(), regenerated);
    }

    #[test]
    fn dropping_a_running_game_host_saves() {
        let assets = small_assets();
        let dir = TempDir::new("drop");
        let start = start_world(&assets, Some(9), &dir.0).unwrap();
        let mut world = start.world.clone();
        let edit = set(&world, 0, CellPos::new(30, 120, 30), "granite");
        world.apply(&edit);
        let mut transport = ChannelTransport::new();
        let endpoint = transport.connect();
        let handle = spawn_host_thread(start.into_host(Box::new(transport)), HOST_HZ);
        endpoint
            .send(ClientMsg::Hello {
                protocol: rmc_protocol::PROTOCOL_VERSION,
                material_hash: assets.materials.hash(),
            })
            .unwrap();
        endpoint.send(ClientMsg::Edit(edit)).unwrap();
        // Wait for the result of the edit, then drop the host without AppExit.
        let deadline = Instant::now() + Duration::from_secs(20);
        while !matches!(
            endpoint.recv_timeout(Duration::from_millis(50)),
            Some(ServerMsg::Applied(_))
        ) {
            assert!(Instant::now() < deadline, "no Applied");
        }
        drop(GameHost::new(
            handle,
            Arc::new(assets.clone()),
            dir.0.clone(),
        ));
        let loaded = start_world(&assets, None, &dir.0).unwrap();
        assert_eq!(loaded.origin, WorldOrigin::Loaded);
        assert_eq!(loaded.host.seed, 9);
        assert_eq!(loaded.world.hash(), world.hash());
    }
}
