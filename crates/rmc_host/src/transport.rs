//! The host's message transport (SAD §8.5).
//!
//! [`Transport`] is what `Host` talks through. [`ChannelTransport`] is the
//! in-process implementation over crossbeam channels: each client holds a
//! [`ClientEndpoint`] and passes typed messages, with no encoding. A network
//! transport implements the same trait later and would use `rmc_protocol`'s
//! codec for framing.

use std::collections::BTreeMap;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::edit::PlayerId;
use tracing::debug;

/// Who receives a [`ServerMsg`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Recipient {
    /// Every connected player.
    All,
    /// One player.
    One(PlayerId),
    /// Every connected player except this one.
    AllExcept(PlayerId),
}

impl Recipient {
    /// Whether `player` is addressed by this recipient.
    pub fn includes(self, player: PlayerId) -> bool {
        match self {
            Recipient::All => true,
            Recipient::One(p) => p == player,
            Recipient::AllExcept(p) => p != player,
        }
    }
}

/// The host side of a connection to its clients.
pub trait Transport: Send {
    /// Every message that has arrived since the last call, in arrival order,
    /// each tagged with the player that sent it. Never blocks.
    fn recv(&mut self) -> Vec<(PlayerId, ClientMsg)>;
    /// Queues `msg` for every connected player that `to` addresses.
    fn send(&mut self, to: Recipient, msg: ServerMsg);
    /// A channel that receives a token after a message arrives, so the host
    /// thread can wake for it instead of waiting for its next tick. Tokens
    /// may be coalesced: one token can stand for several messages, and every
    /// message arrives before its token. `None` (the default) means the
    /// transport cannot signal, and the host thread ticks on its timer only.
    fn wake_signal(&self) -> Option<Receiver<()>> {
        None
    }
}

/// The client's end has been dropped, or the host's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("transport disconnected")]
pub struct Disconnected;

/// In-process [`Transport`] over crossbeam channels.
///
/// All clients share one inbound channel, so [`Transport::recv`] returns
/// messages in global arrival order. Each client has its own outbound
/// channel; a broadcast goes to the clients in ascending [`PlayerId`] order.
/// The transport assigns player ids (1, 2, 3, ...; never reused) and tags each
/// inbound message with its endpoint's id, so a client cannot speak for
/// another. A client whose endpoint was dropped is removed the first time a
/// send to it fails.
pub struct ChannelTransport {
    inbound_tx: Sender<(PlayerId, ClientMsg)>,
    inbound_rx: Receiver<(PlayerId, ClientMsg)>,
    clients: BTreeMap<PlayerId, Sender<ServerMsg>>,
    /// Capacity 1: a token already waiting covers every later message.
    wake_tx: Sender<()>,
    wake_rx: Receiver<()>,
    next_player: u32,
}

impl ChannelTransport {
    pub fn new() -> Self {
        let (inbound_tx, inbound_rx) = crossbeam_channel::unbounded();
        let (wake_tx, wake_rx) = crossbeam_channel::bounded(1);
        Self {
            inbound_tx,
            inbound_rx,
            clients: BTreeMap::new(),
            wake_tx,
            wake_rx,
            next_player: 1,
        }
    }

    /// Connects a new client and returns its end.
    pub fn connect(&mut self) -> ClientEndpoint {
        let player = PlayerId(self.next_player);
        self.next_player += 1;
        let (tx, rx) = crossbeam_channel::unbounded();
        self.clients.insert(player, tx);
        ClientEndpoint {
            player,
            tx: self.inbound_tx.clone(),
            wake: self.wake_tx.clone(),
            rx,
        }
    }

    /// Connected players, ascending. A dropped endpoint stays listed until a
    /// send to it fails.
    pub fn players(&self) -> Vec<PlayerId> {
        self.clients.keys().copied().collect()
    }
}

impl ChannelTransport {
    fn deliver(&mut self, player: PlayerId, msg: ServerMsg) {
        let gone = self
            .clients
            .get(&player)
            .is_some_and(|tx| tx.send(msg).is_err());
        if gone {
            debug!(player = player.0, "client disconnected");
            self.clients.remove(&player);
        }
    }
}

impl Default for ChannelTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for ChannelTransport {
    fn recv(&mut self) -> Vec<(PlayerId, ClientMsg)> {
        self.inbound_rx.try_iter().collect()
    }

    fn send(&mut self, to: Recipient, msg: ServerMsg) {
        let targets: Vec<PlayerId> = match to {
            Recipient::One(p) if self.clients.contains_key(&p) => vec![p],
            Recipient::One(p) => {
                debug!(player = p.0, "send to unknown player dropped");
                Vec::new()
            }
            _ => self
                .clients
                .keys()
                .copied()
                .filter(|&p| to.includes(p))
                .collect(),
        };
        let Some((&last, rest)) = targets.split_last() else {
            return;
        };
        for &p in rest {
            self.deliver(p, msg.clone());
        }
        self.deliver(last, msg);
    }

    fn wake_signal(&self) -> Option<Receiver<()>> {
        Some(self.wake_rx.clone())
    }
}

/// A client's end of a [`ChannelTransport`].
pub struct ClientEndpoint {
    player: PlayerId,
    tx: Sender<(PlayerId, ClientMsg)>,
    wake: Sender<()>,
    rx: Receiver<ServerMsg>,
}

impl ClientEndpoint {
    /// The id the transport gave this client. The host sends it in `Welcome`.
    pub fn player(&self) -> PlayerId {
        self.player
    }

    /// Sends `msg` to the host, tagged with this client's id, and wakes the
    /// host thread if one is waiting.
    pub fn send(&self, msg: ClientMsg) -> Result<(), Disconnected> {
        self.tx.send((self.player, msg)).map_err(|_| Disconnected)?;
        // Full means a token is already waiting; the host will see this
        // message when it wakes for that one.
        let _ = self.wake.try_send(());
        Ok(())
    }

    /// The next message from the host, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<ServerMsg> {
        self.rx.try_recv().ok()
    }

    /// The next message from the host, waiting up to `timeout` for one.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ServerMsg> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Every message from the host that is waiting, in the order sent.
    pub fn drain(&self) -> Vec<ServerMsg> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{IVec3, Vec3};
    use rmc_world::chunk::ChunkStorage;
    use rmc_world::coords::{CellPos, ChunkPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditResult, EditStatus};
    use rmc_world::materials::MaterialId;
    use rmc_world::shape::Shape;

    /// A client message that carries `n` so its order can be checked.
    fn tagged(n: u32) -> ClientMsg {
        ClientMsg::Hello {
            protocol: n,
            material_hash: u64::from(n) * 31,
        }
    }

    /// A server message that carries `n` so its order can be checked.
    fn saved(n: u32) -> ServerMsg {
        ServerMsg::Saved {
            ok: n.is_multiple_of(2),
            path: format!("save/{n}"),
        }
    }

    #[test]
    fn channel_delivers_in_order() {
        let mut t = ChannelTransport::new();
        let a = t.connect();
        let b = t.connect();
        let c = t.connect();
        assert_eq!(a.player(), PlayerId(1));
        assert_eq!(b.player(), PlayerId(2));
        assert_eq!(c.player(), PlayerId(3));
        assert_eq!(t.players(), vec![PlayerId(1), PlayerId(2), PlayerId(3)]);

        // Nothing sent yet: recv is empty and does not block.
        assert!(t.recv().is_empty());
        assert!(a.try_recv().is_none());
        assert!(a.drain().is_empty());

        // Interleaved sends from three clients come back in global send order,
        // each tagged with its sender's id (not one the client chose).
        let order = [&a, &b, &a, &c, &c, &b, &a, &c, &b, &b];
        let mut expected = Vec::new();
        for (n, ep) in order.iter().enumerate() {
            let msg = tagged(n as u32);
            ep.send(msg.clone()).unwrap();
            expected.push((ep.player(), msg));
        }
        assert_eq!(t.recv(), expected);
        // Drained: a second recv is empty.
        assert!(t.recv().is_empty());

        // Every client message variant passes through unchanged. The Edit's
        // `issuer` field claims another player; the transport tag does not.
        let variants = vec![
            tagged(u32::MAX),
            ClientMsg::Edit(EditCommand {
                id: CommandId {
                    issuer: PlayerId(99),
                    seq: 7,
                },
                issuer: PlayerId(99),
                op: EditOp::Damage(3),
                shape: Shape::Sphere {
                    center: CellPos(IVec3::new(-5, 6, -7)),
                    radius: 4,
                },
            }),
            ClientMsg::PlayerState {
                pos: Vec3::new(1.5, -2.0, 3.25),
                yaw: 0.5,
                pitch: -0.25,
            },
            ClientMsg::RequestChunk(ChunkPos(IVec3::new(-1, 0, 2))),
            ClientMsg::SaveRequest,
        ];
        for m in &variants {
            b.send(m.clone()).unwrap();
        }
        let got = t.recv();
        assert_eq!(got.len(), variants.len());
        for ((who, m), want) in got.iter().zip(&variants) {
            assert_eq!(*who, PlayerId(2));
            assert_eq!(m, want);
        }

        // Messages sent after a recv show up on the next recv, not lost and
        // not merged into the previous batch.
        a.send(tagged(100)).unwrap();
        let first = t.recv();
        c.send(tagged(101)).unwrap();
        a.send(tagged(102)).unwrap();
        let second = t.recv();
        assert_eq!(first, vec![(PlayerId(1), tagged(100))]);
        assert_eq!(
            second,
            vec![(PlayerId(3), tagged(101)), (PlayerId(1), tagged(102))]
        );

        // A large burst keeps its order.
        for n in 0..10_000u32 {
            [&a, &b, &c][n as usize % 3].send(tagged(n)).unwrap();
        }
        let burst = t.recv();
        assert_eq!(burst.len(), 10_000);
        for (n, (who, m)) in burst.iter().enumerate() {
            assert_eq!(*who, PlayerId(n as u32 % 3 + 1));
            assert_eq!(*m, tagged(n as u32));
        }

        // Host to one client: in the order sent, via try_recv and drain.
        for n in 0..1_000 {
            t.send(Recipient::One(b.player()), saved(n));
        }
        assert_eq!(b.try_recv(), Some(saved(0)));
        assert_eq!(b.try_recv(), Some(saved(1)));
        assert_eq!(b.drain(), (2..1_000).map(saved).collect::<Vec<_>>());
        assert!(b.try_recv().is_none());
        assert!(b.drain().is_empty());
        assert!(a.drain().is_empty());
        assert!(c.drain().is_empty());

        // Broadcasts and directed messages interleave in send order per client.
        let snapshot = ServerMsg::ChunkSnapshot {
            pos: ChunkPos(IVec3::new(3, -1, 0)),
            revision: 42,
            storage: ChunkStorage::Uniform(MaterialId(5)),
        };
        let applied = ServerMsg::Applied(EditResult {
            command: CommandId {
                issuer: PlayerId(1),
                seq: 9,
            },
            status: EditStatus::Applied,
            deltas: Vec::new(),
            yields: vec![(MaterialId(2), 3)],
        });
        t.send(Recipient::All, saved(0));
        t.send(Recipient::One(a.player()), snapshot.clone());
        t.send(Recipient::All, applied.clone());
        t.send(Recipient::One(c.player()), saved(1));
        t.send(Recipient::All, saved(2));
        assert_eq!(
            a.drain(),
            vec![saved(0), snapshot, applied.clone(), saved(2)]
        );
        assert_eq!(b.drain(), vec![saved(0), applied.clone(), saved(2)]);
        assert_eq!(c.drain(), vec![saved(0), applied, saved(1), saved(2)]);

        // Endpoints are Send: a client on another thread is delivered in order.
        let d = t.connect();
        let sender = std::thread::spawn(move || {
            for n in 0..500 {
                d.send(tagged(n)).unwrap();
            }
            d
        });
        let d = sender.join().unwrap();
        assert_eq!(
            t.recv(),
            (0..500)
                .map(|n| (d.player(), tagged(n)))
                .collect::<Vec<_>>()
        );

        // Dropping the host side disconnects the client's send.
        drop(t);
        assert_eq!(a.send(tagged(0)), Err(Disconnected));
        assert!(a.try_recv().is_none());
    }

    #[test]
    fn recipient_filtering() {
        let p = PlayerId;
        // Recipient::includes truth table.
        for who in [p(0), p(1), p(2), p(u32::MAX)] {
            assert!(Recipient::All.includes(who));
            assert!(Recipient::One(who).includes(who));
            assert!(!Recipient::AllExcept(who).includes(who));
        }
        assert!(!Recipient::One(p(1)).includes(p(2)));
        assert!(!Recipient::One(p(2)).includes(p(1)));
        assert!(Recipient::AllExcept(p(1)).includes(p(2)));
        assert!(Recipient::AllExcept(p(2)).includes(p(1)));

        let mut t = ChannelTransport::new();
        let eps: Vec<ClientEndpoint> = (0..4).map(|_| t.connect()).collect();
        let ids: Vec<PlayerId> = eps.iter().map(ClientEndpoint::player).collect();
        assert_eq!(ids, vec![p(1), p(2), p(3), p(4)]);

        // Each recipient kind, addressed at each player, reaches exactly the
        // players it includes, once each.
        let mut n = 0;
        for &target in &ids {
            for to in [
                Recipient::All,
                Recipient::One(target),
                Recipient::AllExcept(target),
            ] {
                t.send(to, saved(n));
                for ep in &eps {
                    let got = ep.drain();
                    if to.includes(ep.player()) {
                        assert_eq!(got, vec![saved(n)], "{to:?} to {:?}", ep.player());
                    } else {
                        assert!(got.is_empty(), "{to:?} leaked to {:?}", ep.player());
                    }
                }
                n += 1;
            }
        }
        // Spot checks on the expected sets, independent of `includes`.
        t.send(Recipient::One(p(3)), saved(1000));
        t.send(Recipient::AllExcept(p(3)), saved(1001));
        let got: Vec<Vec<ServerMsg>> = eps.iter().map(ClientEndpoint::drain).collect();
        assert_eq!(
            got,
            vec![
                vec![saved(1001)],
                vec![saved(1001)],
                vec![saved(1000)],
                vec![saved(1001)],
            ]
        );

        // A player that never connected receives nothing, and nobody else does.
        t.send(Recipient::One(p(77)), saved(2000));
        t.send(Recipient::One(p(0)), saved(2001));
        for ep in &eps {
            assert!(ep.drain().is_empty());
        }
        // AllExcept an unknown player is everyone.
        t.send(Recipient::AllExcept(p(77)), saved(2002));
        for ep in &eps {
            assert_eq!(ep.drain(), vec![saved(2002)]);
        }

        // A dropped client is skipped without disturbing the others, and is
        // removed from the player list once a send finds it gone.
        let mut eps = eps;
        drop(eps.remove(1));
        t.send(Recipient::All, saved(3000));
        t.send(Recipient::One(p(2)), saved(3001));
        t.send(Recipient::AllExcept(p(1)), saved(3002));
        assert_eq!(eps[0].drain(), vec![saved(3000)]);
        assert_eq!(eps[1].drain(), vec![saved(3000), saved(3002)]);
        assert_eq!(eps[2].drain(), vec![saved(3000), saved(3002)]);
        assert_eq!(t.players(), vec![p(1), p(3), p(4)]);

        // Ids are never reused after a disconnect, and a client connected
        // later does not see earlier broadcasts.
        let late = t.connect();
        assert_eq!(late.player(), p(5));
        assert!(late.try_recv().is_none());
        t.send(Recipient::All, saved(4000));
        assert_eq!(late.drain(), vec![saved(4000)]);
        for ep in &eps {
            assert_eq!(ep.drain(), vec![saved(4000)]);
        }

        // Filtering is outbound only: a dropped client's earlier messages
        // still arrive, tagged with its id.
        let extra = t.connect();
        extra.send(tagged(1)).unwrap();
        drop(extra);
        assert_eq!(t.recv(), vec![(p(6), tagged(1))]);
    }
}
