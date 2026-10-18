//! The client's connection to the host (SAD §3 listen server, §8.4).
//!
//! The client never sees the host's types. It talks through a [`HostLink`]
//! (the app and the test kit wrap a `ChannelTransport` endpoint in one) held
//! in the [`HostConnection`] resource. [`NetPlugin`] moves messages between
//! the link and Bevy:
//!
//! - `Startup`: sends `Hello` with the protocol version and the hash of the
//!   [`ClientMaterials`] table.
//! - `PreUpdate`: every [`ServerMsg`] waiting on the link becomes a
//!   [`FromHost`] message, in the order the host sent them, so the frame's
//!   systems see everything the host has done so far.
//! - `Last`: every [`ToHost`] message written during the frame is sent, in
//!   write order.

use std::sync::Arc;

use bevy::prelude::*;
use rmc_protocol::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use rmc_world::materials::MaterialTable;

/// The client's end of a connection to the host.
pub trait HostLink: Send + Sync + 'static {
    /// Sends `msg` to the host. Returns false if the host is gone.
    fn send(&self, msg: ClientMsg) -> bool;
    /// Every message from the host that is waiting, in the order sent. Never blocks.
    fn drain(&self) -> Vec<ServerMsg>;
}

/// The link to the host. Without it, [`NetPlugin`]'s systems do nothing.
#[derive(Resource)]
pub struct HostConnection {
    link: Box<dyn HostLink>,
    /// Set once a send fails; later sends are dropped without retrying.
    disconnected: bool,
}

impl HostConnection {
    pub fn new(link: impl HostLink) -> Self {
        Self {
            link: Box::new(link),
            disconnected: false,
        }
    }

    /// Whether a send to the host has failed.
    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    fn send(&mut self, msg: ClientMsg) {
        if self.disconnected {
            return;
        }
        if !self.link.send(msg) {
            warn!("host link disconnected");
            self.disconnected = true;
        }
    }
}

/// The material table the client was built with. Its hash goes in `Hello`.
#[derive(Resource, Clone)]
pub struct ClientMaterials(pub Arc<MaterialTable>);

/// A message the host sent, delivered in `PreUpdate`.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct FromHost(pub ServerMsg);

/// A message for the host, sent in `Last` of the frame it was written in.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct ToHost(pub ClientMsg);

/// Moves messages between the [`HostConnection`] and [`FromHost`]/[`ToHost`].
pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<FromHost>()
            .add_message::<ToHost>()
            .add_systems(Startup, send_hello)
            .add_systems(PreUpdate, receive_from_host)
            .add_systems(Last, send_to_host);
    }
}

fn send_hello(materials: Option<Res<ClientMaterials>>, mut out: MessageWriter<ToHost>) {
    let Some(materials) = materials else {
        warn!("no ClientMaterials resource; Hello not sent");
        return;
    };
    out.write(ToHost(ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        material_hash: materials.0.hash(),
    }));
}

fn receive_from_host(connection: Option<Res<HostConnection>>, mut inbox: MessageWriter<FromHost>) {
    let Some(connection) = connection else {
        return;
    };
    inbox.write_batch(connection.link.drain().into_iter().map(FromHost));
}

fn send_to_host(connection: Option<ResMut<HostConnection>>, mut outbox: MessageReader<ToHost>) {
    let Some(mut connection) = connection else {
        outbox.clear();
        return;
    };
    for ToHost(msg) in outbox.read() {
        connection.send(msg.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use rmc_world::coords::CellAabb;
    use rmc_world::edit::PlayerId;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// A link whose host side is the test: `sent` records what the client
    /// sent, `inbound` holds what the "host" queued.
    #[derive(Clone, Default)]
    struct FakeLink {
        sent: Arc<Mutex<Vec<ClientMsg>>>,
        inbound: Arc<Mutex<Vec<ServerMsg>>>,
        closed: Arc<Mutex<bool>>,
    }

    impl HostLink for FakeLink {
        fn send(&self, msg: ClientMsg) -> bool {
            if *self.closed.lock().unwrap() {
                return false;
            }
            self.sent.lock().unwrap().push(msg);
            true
        }

        fn drain(&self) -> Vec<ServerMsg> {
            std::mem::take(&mut *self.inbound.lock().unwrap())
        }
    }

    #[derive(Resource, Default)]
    struct Received(Vec<ServerMsg>);

    fn record(mut inbox: MessageReader<FromHost>, mut received: ResMut<Received>) {
        received.0.extend(inbox.read().map(|m| m.0.clone()));
    }

    fn app_with(link: FakeLink) -> (App, Arc<MaterialTable>) {
        let materials =
            Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, NetPlugin))
            .insert_resource(ClientMaterials(materials.clone()))
            .insert_resource(HostConnection::new(link))
            .init_resource::<Received>()
            .add_systems(Update, record);
        (app, materials)
    }

    fn welcome(player: u32) -> ServerMsg {
        ServerMsg::Welcome {
            player: PlayerId(player),
            seed: 7,
            bounds: CellAabb::PHASE1_DEFAULT,
            gen_params_hash: 9,
        }
    }

    #[test]
    fn hello_sent_once_on_startup() {
        let link = FakeLink::default();
        let (mut app, materials) = app_with(link.clone());
        app.update();
        app.update();
        assert_eq!(
            *link.sent.lock().unwrap(),
            vec![ClientMsg::Hello {
                protocol: PROTOCOL_VERSION,
                material_hash: materials.hash(),
            }]
        );
    }

    #[test]
    fn host_messages_arrive_in_order_in_the_same_frame() {
        let link = FakeLink::default();
        let (mut app, _) = app_with(link.clone());
        app.update();
        let msgs = vec![
            welcome(1),
            ServerMsg::Inventory { items: vec![] },
            welcome(2),
        ];
        link.inbound.lock().unwrap().extend(msgs.clone());
        app.update();
        assert_eq!(app.world().resource::<Received>().0, msgs);
        app.update();
        assert_eq!(app.world().resource::<Received>().0, msgs, "delivered once");
    }

    #[test]
    fn to_host_sent_in_write_order_at_end_of_frame() {
        let link = FakeLink::default();
        let (mut app, _) = app_with(link.clone());
        app.update();
        link.sent.lock().unwrap().clear();
        app.world_mut()
            .write_message(ToHost(ClientMsg::SaveRequest));
        app.world_mut()
            .write_message(ToHost(ClientMsg::RequestChunk(Default::default())));
        app.update();
        assert_eq!(
            *link.sent.lock().unwrap(),
            vec![
                ClientMsg::SaveRequest,
                ClientMsg::RequestChunk(Default::default())
            ]
        );
        app.update();
        assert_eq!(link.sent.lock().unwrap().len(), 2, "sent once");
    }

    #[test]
    fn failed_send_marks_disconnected() {
        let link = FakeLink::default();
        *link.closed.lock().unwrap() = true;
        let (mut app, _) = app_with(link.clone());
        app.update();
        assert!(app.world().resource::<HostConnection>().is_disconnected());
        assert!(link.sent.lock().unwrap().is_empty());
    }
}
