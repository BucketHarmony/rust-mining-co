//! Saving from the client (SAD §10).
//!
//! The client never writes the world itself: it asks the host with
//! `SaveRequest`, and the host saves its own world and answers
//! `Saved { ok, path }`. [`SavePlugin`] sends that request
//!
//! - when the editor's Save action ([`EditorState::save_requested`]) is
//!   raised; the plugin takes the flag;
//! - when the [`Autosave`] timer runs out, every [`AUTOSAVE_INTERVAL`] of
//!   game time.
//!
//! At most one request goes out per frame, and any request restarts the
//! autosave timer. Each `Saved` answer is recorded in [`SaveStatus`].
//!
//! The save on quit and loading are the host owner's work (the app stops
//! the host thread, saves, and loads on start), since the client has no
//! access to the host's world.

use std::time::Duration;

use bevy::prelude::*;
use rmc_protocol::{ClientMsg, ServerMsg};

use crate::editor::{EditorState, EditorSystems};
use crate::net::{FromHost, ToHost};

/// How often the client asks the host to save (SAD §10: every 5 minutes).
pub const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The autosave timer, counted in game time ([`Time`]).
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct Autosave {
    /// Time between automatic saves; `None` turns autosave off.
    pub interval: Option<Duration>,
    /// Game time since the last save request.
    pub elapsed: Duration,
}

impl Default for Autosave {
    fn default() -> Self {
        Self {
            interval: Some(AUTOSAVE_INTERVAL),
            elapsed: Duration::ZERO,
        }
    }
}

/// What raised a save request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveCause {
    /// The editor's Save action.
    Editor,
    /// The [`Autosave`] timer.
    Autosave,
}

/// The host's answer to a save request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveOutcome {
    pub ok: bool,
    /// The save directory the host wrote (empty if it has none).
    pub path: String,
}

/// The client's view of its save requests.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveStatus {
    /// Requests sent so far.
    pub requested: u32,
    /// Requests sent that the host has not answered yet.
    pub pending: u32,
    /// What raised the most recent request.
    pub last_cause: Option<SaveCause>,
    /// The host's most recent answer.
    pub last: Option<SaveOutcome>,
}

/// [`SavePlugin`]'s system set (`Update`, after [`EditorSystems`]).
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SaveSystems;

/// Sends `SaveRequest` for the editor and the autosave timer, and records
/// the host's `Saved` answers.
pub struct SavePlugin;

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ToHost>()
            .add_message::<FromHost>()
            .init_resource::<Autosave>()
            .init_resource::<SaveStatus>()
            .add_systems(
                Update,
                (request_saves, record_saved)
                    .chain()
                    .in_set(SaveSystems)
                    .after(EditorSystems),
            );
    }
}

/// Sends one `SaveRequest` if the editor asked for a save or the autosave
/// timer ran out this frame.
fn request_saves(
    time: Res<Time>,
    editor: Option<ResMut<EditorState>>,
    mut autosave: ResMut<Autosave>,
    mut status: ResMut<SaveStatus>,
    mut out: MessageWriter<ToHost>,
) {
    let editor_asked = editor.is_some_and(|mut e| std::mem::take(&mut e.save_requested));
    let cause = if editor_asked {
        Some(SaveCause::Editor)
    } else if let Some(interval) = autosave.interval {
        autosave.elapsed += time.delta();
        (autosave.elapsed >= interval).then_some(SaveCause::Autosave)
    } else {
        None
    };
    let Some(cause) = cause else {
        return;
    };
    autosave.elapsed = Duration::ZERO;
    out.write(ToHost(ClientMsg::SaveRequest));
    status.requested += 1;
    status.pending += 1;
    status.last_cause = Some(cause);
    info!(?cause, "save requested");
}

/// Records every `Saved` the host sent this frame.
fn record_saved(mut inbox: MessageReader<FromHost>, mut status: ResMut<SaveStatus>) {
    for FromHost(msg) in inbox.read() {
        if let ServerMsg::Saved { ok, path } = msg {
            status.pending = status.pending.saturating_sub(1);
            status.last = Some(SaveOutcome {
                ok: *ok,
                path: path.clone(),
            });
            if *ok {
                info!(%path, "host saved the world");
            } else {
                warn!(%path, "host could not save the world");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use bevy::time::TimeUpdateStrategy;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, CellPos, ChunkPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::materials::MaterialTable;
    use rmc_world::save::{self, SaveHeader};
    use rmc_world::shape::Shape;
    use rmc_world::world::World;

    use crate::net::{ClientMaterials, HostConnection, HostLink, NetPlugin};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// One frame at 60 fps.
    fn frame() -> Duration {
        Duration::from_secs_f64(1.0 / 60.0)
    }

    /// A link whose host side is the test.
    #[derive(Clone, Default)]
    struct FakeLink {
        sent: Arc<Mutex<Vec<ClientMsg>>>,
        inbound: Arc<Mutex<Vec<ServerMsg>>>,
    }

    impl HostLink for FakeLink {
        fn send(&self, msg: ClientMsg) -> bool {
            self.sent.lock().unwrap().push(msg);
            true
        }
        fn drain(&self) -> Vec<ServerMsg> {
            std::mem::take(&mut *self.inbound.lock().unwrap())
        }
    }

    impl FakeLink {
        /// How many `SaveRequest`s the client has sent; clears the record.
        fn take_save_requests(&self) -> usize {
            std::mem::take(&mut *self.sent.lock().unwrap())
                .into_iter()
                .filter(|m| *m == ClientMsg::SaveRequest)
                .count()
        }

        fn answer(&self, ok: bool, path: &str) {
            self.inbound.lock().unwrap().push(ServerMsg::Saved {
                ok,
                path: path.to_string(),
            });
        }
    }

    /// A save directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rmc_client_save_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn materials() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"))
    }

    /// Two stone chunks side by side, as generated (unmodified).
    fn generated(materials: &Arc<MaterialTable>) -> World {
        let stone = materials.id("stone").expect("stone");
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31));
        let mut world = World::new(materials.clone(), bounds);
        for x in 0..2 {
            world
                .insert_chunk(ChunkPos::new(x, 0, 0), Chunk::uniform(stone))
                .unwrap();
        }
        world
    }

    fn app_with(link: FakeLink, materials: Arc<MaterialTable>) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, NetPlugin, SavePlugin))
            .init_resource::<EditorState>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(frame()))
            .insert_resource(ClientMaterials(materials))
            .insert_resource(HostConnection::new(link));
        app
    }

    /// Steps up to `limit` frames until a `SaveRequest` goes out; returns
    /// the frame it went out in (1-based).
    fn frames_until_save(app: &mut App, link: &FakeLink, limit: u32) -> Option<u32> {
        (1..=limit).find(|_| {
            app.update();
            link.take_save_requests() > 0
        })
    }

    #[test]
    fn save_request_round_trips() {
        let materials = materials();
        let link = FakeLink::default();
        let mut app = app_with(link.clone(), materials.clone());
        app.update();
        assert_eq!(link.take_save_requests(), 0, "no save at startup");
        assert_eq!(*app.world().resource::<SaveStatus>(), SaveStatus::default());
        assert_eq!(
            app.world().resource::<Autosave>().interval,
            Some(Duration::from_secs(300)),
            "SAD §10: autosave every 5 minutes"
        );

        // The editor's Save: one SaveRequest that frame, and the flag is taken.
        app.world_mut().resource_mut::<EditorState>().save_requested = true;
        app.update();
        assert_eq!(
            link.take_save_requests(),
            1,
            "editor Save sends SaveRequest"
        );
        assert!(!app.world().resource::<EditorState>().save_requested);
        let status = app.world().resource::<SaveStatus>().clone();
        assert_eq!(status.requested, 1);
        assert_eq!(status.pending, 1);
        assert_eq!(status.last_cause, Some(SaveCause::Editor));
        assert_eq!(status.last, None, "no answer yet");
        app.update();
        assert_eq!(link.take_save_requests(), 0, "sent once");

        // The host saves a world with one edited chunk and answers.
        let mut host_world = generated(&materials);
        let air = materials.id("air").expect("air");
        let result = host_world.apply(&EditCommand {
            id: CommandId {
                issuer: PlayerId(1),
                seq: 0,
            },
            issuer: PlayerId(1),
            op: EditOp::Set(air),
            shape: Shape::Cell(CellPos::new(40, 5, 6)),
        });
        assert_eq!(result.status, EditStatus::Applied);
        let dir = TempDir::new("round_trip");
        let header = SaveHeader::new(7, host_world.bounds(), materials.hash(), 9, "noise 1");
        assert_eq!(save::save(&dir.0, &header, &host_world).unwrap(), 1);
        let path = dir.0.display().to_string();
        link.answer(true, &path);
        app.update();
        let status = app.world().resource::<SaveStatus>().clone();
        assert_eq!(status.pending, 0, "answered");
        assert_eq!(
            status.last,
            Some(SaveOutcome {
                ok: true,
                path: path.clone()
            })
        );

        // What the host wrote loads back into the same world.
        let loaded = save::load(&dir.0, &header, || generated(&materials)).unwrap();
        assert_eq!(loaded.hash(), host_world.hash());
        assert_eq!(loaded.get_cell(CellPos::new(40, 5, 6)), Some(air));
        assert_ne!(loaded.hash(), generated(&materials).hash());

        // Autosave: nothing before the interval runs out, then exactly one
        // request, and the timer starts over.
        {
            let mut autosave = app.world_mut().resource_mut::<Autosave>();
            autosave.interval = Some(Duration::from_secs(1));
            autosave.elapsed = Duration::ZERO;
        }
        let first = frames_until_save(&mut app, &link, 200).expect("autosave fired");
        assert!(
            (59..=61).contains(&first),
            "1 s at 60 fps autosaves near frame 60, got {first}"
        );
        let status = app.world().resource::<SaveStatus>().clone();
        assert_eq!(status.requested, 2);
        assert_eq!(status.pending, 1);
        assert_eq!(status.last_cause, Some(SaveCause::Autosave));
        assert!(app.world().resource::<Autosave>().elapsed < Duration::from_secs(1));
        let second = frames_until_save(&mut app, &link, 200).expect("second autosave");
        assert!(
            (59..=61).contains(&second),
            "next autosave a full interval later, got {second}"
        );

        // An editor save restarts the timer.
        for _ in 0..30 {
            app.update();
        }
        assert_eq!(link.take_save_requests(), 0);
        app.world_mut().resource_mut::<EditorState>().save_requested = true;
        app.update();
        assert_eq!(link.take_save_requests(), 1);
        assert_eq!(
            app.world().resource::<SaveStatus>().last_cause,
            Some(SaveCause::Editor)
        );
        for _ in 0..40 {
            app.update();
        }
        assert_eq!(
            link.take_save_requests(),
            0,
            "editor save restarted the timer"
        );

        // The editor and the timer in the same frame give one request.
        app.world_mut().resource_mut::<Autosave>().elapsed = Duration::from_secs(1);
        app.world_mut().resource_mut::<EditorState>().save_requested = true;
        app.update();
        assert_eq!(link.take_save_requests(), 1, "one request per frame");
        assert_eq!(app.world().resource::<SaveStatus>().requested, 5);

        // A failed save is recorded too; extra answers never drive pending
        // below 0.
        for _ in 0..4 {
            link.answer(false, "");
        }
        app.update();
        let status = app.world().resource::<SaveStatus>().clone();
        assert_eq!(status.pending, 0);
        assert_eq!(
            status.last,
            Some(SaveOutcome {
                ok: false,
                path: String::new()
            })
        );

        // Autosave off: no requests however long the game runs.
        app.world_mut().resource_mut::<Autosave>().interval = None;
        for _ in 0..200 {
            app.update();
        }
        assert_eq!(link.take_save_requests(), 0, "autosave off");
    }
}
