//! The host on its own thread (SAD §8.5).
//!
//! [`spawn_host_thread`] moves a [`Host`] onto a thread that calls
//! [`Host::tick`] at a fixed rate, and also as soon as a message arrives if
//! the transport can signal arrivals ([`Transport::wake_signal`]), so edit
//! latency is bounded by processing time, not by the tick interval. The app
//! uses this; tests call `Host::tick` directly.
//!
//! [`Transport::wake_signal`]: crate::transport::Transport::wake_signal

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, select};
use tracing::info;

use crate::host::Host;

/// A running host thread. [`HostHandle::stop`] ends it and gives the host
/// back; dropping the handle ends it too.
pub struct HostHandle {
    stop: Sender<()>,
    thread: Option<JoinHandle<Host>>,
}

impl HostHandle {
    /// Stops the thread, waiting for any tick in progress to finish, and
    /// returns the host.
    ///
    /// # Panics
    /// If the host thread panicked.
    pub fn stop(mut self) -> Host {
        let thread = self.thread.take().expect("thread present until stopped");
        let _ = self.stop.send(());
        thread.join().expect("host thread panicked")
    }
}

impl Drop for HostHandle {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.stop.send(());
            let _ = thread.join();
        }
    }
}

/// Runs `host` on a new thread that ticks `hz` times a second, and also
/// right after a message arrives (whichever comes first).
///
/// # Panics
/// If `hz` is 0.
pub fn spawn_host_thread(host: Host, hz: u32) -> HostHandle {
    assert!(hz > 0, "host tick rate must be above 0 Hz");
    let period = Duration::from_secs(1) / hz;
    let (stop, stop_rx) = crossbeam_channel::bounded::<()>(1);
    let thread = std::thread::Builder::new()
        .name("rmc-host".into())
        .spawn(move || {
            let mut host = host;
            let mut wake = host.wake_signal().unwrap_or_else(crossbeam_channel::never);
            info!(hz, "host thread started");
            let mut deadline = Instant::now() + period;
            loop {
                let timeout = deadline.saturating_duration_since(Instant::now());
                select! {
                    // A stop, or the handle dropped.
                    recv(stop_rx) -> _ => break,
                    recv(wake) -> token => {
                        if token.is_err() {
                            // Nothing can signal any more: timer only.
                            wake = crossbeam_channel::never();
                        }
                    }
                    default(timeout) => {}
                }
                // Tokens are drained before the tick reads the messages, so a
                // message that lands after this has a token left for the next
                // wake; it is never stranded until the timer.
                while wake.try_recv().is_ok() {}
                host.tick();
                deadline = Instant::now() + period;
            }
            info!("host thread stopped");
            host
        })
        .expect("spawn host thread");
    HostHandle {
        stop,
        thread: Some(thread),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crossbeam_channel::Receiver;
    use rmc_protocol::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, CellPos, ChunkPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::materials::MaterialTable;
    use rmc_world::shape::Shape;
    use rmc_world::world::World;

    use crate::host::HostConfig;
    use crate::transport::{ChannelTransport, ClientEndpoint, Recipient, Transport};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// The latency bound the SAD sets for a woken host (§14).
    const BOUND: Duration = Duration::from_millis(50);

    /// One resident stone chunk at the origin.
    fn world() -> World {
        let materials =
            Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
        let stone = materials.id("stone").expect("stone");
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut world = World::new(materials, bounds);
        world
            .insert_chunk(ChunkPos::new(0, 0, 0), Chunk::uniform(stone))
            .unwrap();
        world
    }

    fn hello(world: &World) -> ClientMsg {
        ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            material_hash: world.materials().hash(),
        }
    }

    /// The `n`th edit of a fixed sequence: alternately sets cells to air and
    /// to granite, so every edit changes something.
    fn nth_edit(world: &World, player: PlayerId, n: u32) -> EditCommand {
        let key = if n.is_multiple_of(2) {
            "air"
        } else {
            "granite"
        };
        let id = world.materials().id(key).expect("material");
        let i = (n / 2) as i32;
        EditCommand {
            id: CommandId {
                issuer: player,
                seq: n,
            },
            issuer: player,
            op: EditOp::Set(id),
            shape: Shape::Cell(CellPos::new(i % 32, (i / 32) % 32, 7)),
        }
    }

    /// Waits up to `limit` for a message, then checks it came within `BOUND`.
    fn recv_within_bound(ep: &ClientEndpoint, limit: Duration, what: &str) -> ServerMsg {
        let start = Instant::now();
        let msg = ep
            .recv_timeout(limit)
            .unwrap_or_else(|| panic!("{what}: no reply within {limit:?}"));
        let took = start.elapsed();
        assert!(took < BOUND, "{what}: reply took {took:?}, bound {BOUND:?}");
        msg
    }

    /// A transport that cannot signal arrivals, so its host thread ticks on
    /// the timer only.
    struct Silent(ChannelTransport);

    impl Transport for Silent {
        fn recv(&mut self) -> Vec<(PlayerId, ClientMsg)> {
            self.0.recv()
        }
        fn send(&mut self, to: Recipient, msg: ServerMsg) {
            self.0.send(to, msg)
        }
    }

    #[test]
    fn wakes_on_message_under_50ms() {
        let world = world();
        let mut reference = world.clone();
        let hello_msg = hello(&world);
        let mut transport = ChannelTransport::new();
        let a = transport.connect();
        let b = transport.connect();
        assert!(transport.wake_signal().is_some());
        let host = Host::new(world.clone(), HostConfig::default(), Box::new(transport));
        // 1 Hz: a reply inside 50 ms can only come from a wake, never from
        // the timer, and the round trips below all fit inside one second, so
        // at most one of them can even share a moment with a timer tick.
        let handle = spawn_host_thread(host, 1);

        // Hello: Welcome and the one snapshot, woken at once.
        a.send(hello_msg.clone()).unwrap();
        let welcome = recv_within_bound(&a, Duration::from_secs(5), "Welcome");
        assert!(matches!(welcome, ServerMsg::Welcome { player, .. } if player == a.player()));
        assert!(matches!(
            a.recv_timeout(Duration::from_secs(5)),
            Some(ServerMsg::ChunkSnapshot { .. })
        ));

        // Edits, each one a separate wake, with idle gaps in between so the
        // host is parked when each one lands.
        for n in 0..10 {
            std::thread::sleep(Duration::from_millis(15));
            let cmd = nth_edit(&world, a.player(), n);
            let want = reference.apply(&cmd);
            assert_eq!(want.status, EditStatus::Applied);
            a.send(ClientMsg::Edit(cmd)).unwrap();
            let got = recv_within_bound(&a, Duration::from_secs(5), &format!("edit {n}"));
            assert_eq!(got, ServerMsg::Applied(want), "edit {n}");
        }
        assert!(a.try_recv().is_none());

        // A burst from two clients is handled in arrival order: tokens
        // coalesce, but no message is left behind waiting for the timer.
        b.send(hello_msg).unwrap();
        let mut sent = Vec::new();
        for n in 10..60 {
            let (ep, player) = if n % 3 == 0 {
                (&b, b.player())
            } else {
                (&a, a.player())
            };
            let cmd = nth_edit(&world, player, n);
            sent.push(cmd);
            ep.send(ClientMsg::Edit(cmd)).unwrap();
        }
        let start = Instant::now();
        let mut results = Vec::new();
        while results.len() < sent.len() && start.elapsed() < Duration::from_secs(5) {
            if let Some(msg) = a.recv_timeout(Duration::from_millis(5)) {
                results.push(msg);
            }
        }
        let took = start.elapsed();
        assert_eq!(results.len(), sent.len(), "every burst result reached a");
        assert!(took < BOUND, "burst took {took:?}, bound {BOUND:?}");
        let want: Vec<ServerMsg> = sent
            .iter()
            .map(|cmd| ServerMsg::Applied(reference.apply(cmd)))
            .collect();
        assert_eq!(results, want, "results in arrival order");
        // b joined first in that burst, so it has its join and every result.
        // The host sends to a before b, so b's last copies may still be on
        // their way.
        let to_b: Vec<ServerMsg> = (0..want.len() + 2)
            .map_while(|_| b.recv_timeout(Duration::from_secs(5)))
            .collect();
        assert_eq!(to_b.len(), want.len() + 2);
        assert!(matches!(to_b[0], ServerMsg::Welcome { player, .. } if player == b.player()));
        assert_eq!(&to_b[2..], &want[..]);

        // Stopping does not wait for the 1 s timer, and gives the host back
        // with every edit applied.
        let start = Instant::now();
        let host = handle.stop();
        let took = start.elapsed();
        assert!(took < Duration::from_millis(200), "stop took {took:?}");
        assert!(host.is_joined(a.player()) && host.is_joined(b.player()));
        assert_eq!(host.world().hash(), reference.hash());
        assert_ne!(host.world().hash(), world.hash());
    }

    #[test]
    fn ticks_on_timer_without_wake_signal() {
        let world = world();
        let mut transport = ChannelTransport::new();
        let a = transport.connect();
        let silent = Silent(transport);
        assert!(silent.wake_signal().is_none());
        let host = Host::new(world.clone(), HostConfig::default(), Box::new(silent));
        let handle = spawn_host_thread(host, 20);
        a.send(hello(&world)).unwrap();
        let welcome = a
            .recv_timeout(Duration::from_secs(5))
            .expect("the 20 Hz timer tick answers Hello");
        assert!(matches!(welcome, ServerMsg::Welcome { .. }));
        drop(handle);
        // Dropping the handle stopped the thread and dropped the transport.
        let _ = a.drain();
        assert!(a.send(hello(&world)).is_err());
    }

    #[test]
    fn idle_host_thread_sends_nothing() {
        let world = world();
        let mut transport = ChannelTransport::new();
        let a = transport.connect();
        let wake: Receiver<()> = transport.wake_signal().unwrap();
        let handle = spawn_host_thread(
            Host::new(world, HostConfig::default(), Box::new(transport)),
            200,
        );
        assert!(a.recv_timeout(Duration::from_millis(60)).is_none());
        // The host thread drains wake tokens; nothing is pending here.
        a.send(ClientMsg::SaveRequest).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            wake.try_recv().is_err(),
            "token consumed by the host thread"
        );
        // An unjoined player's message gets no reply.
        assert!(a.try_recv().is_none());
        let host = handle.stop();
        assert!(!host.is_joined(a.player()));
    }
}
