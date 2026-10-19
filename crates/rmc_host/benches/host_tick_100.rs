//! SAD §13/§14 bench `host_tick_100`: one `Host::tick` that handles 100
//! single-cell edits from a joined client. Target < 2 ms in release.
//!
//! The world is the default generated world. Iterations alternate between
//! setting 100 cells to a solid material and removing them again, so every
//! edit changes its cell. Sending the edits and draining the replies are
//! outside the timed region.

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use rmc_host::{ChannelTransport, Host, HostConfig};
use rmc_protocol::{ClientMsg, PROTOCOL_VERSION};
use rmc_world::coords::CellPos;
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::MaterialTable;
use rmc_world::shape::Shape;
use rmc_world::world::World;
use rmc_worldgen::{GenParams, Generator};

const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
const PARAMS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/worldgen.ron");
const SEED: u64 = 45;
/// Edits per tick.
const EDITS: usize = 100;

fn host_tick_100(c: &mut Criterion) {
    let table = Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
    let params = GenParams::load(PARAMS_PATH).expect("assets/worldgen.ron loads");
    let generator = Generator::new(SEED, &params, &table).expect("shipped params fit the table");
    let bounds = generator.bounds();
    let mut world = World::new(table.clone(), bounds);
    for (pos, chunk) in generator.generate_world() {
        world
            .insert_chunk(pos, chunk)
            .expect("generated chunk fits the world");
    }
    let place = table
        .with_tag("placeable")
        .next()
        .expect("the table has a placeable material")
        .id;

    let mut transport = ChannelTransport::new();
    let client = transport.connect();
    let player = client.player();
    let mut host = Host::new(world, HostConfig::default(), Box::new(transport));
    client
        .send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            material_hash: table.hash(),
        })
        .expect("host is connected");
    host.tick();
    client.drain();

    // 100 cells spread over the middle of the world, one per command, in
    // several chunks.
    let center = (bounds.min.0 + bounds.max.0) / 2;
    let cells: Vec<CellPos> = (0..EDITS as i32)
        .map(|i| {
            CellPos(
                center + glam::IVec3::new((i % 10) * 7 - 32, (i / 10) * 3 - 15, (i % 7) * 9 - 30),
            )
        })
        .collect();

    let mut seq = 0u32;
    let mut set = true;
    c.bench_function("host_tick_100", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let op = if set {
                    EditOp::Set(place)
                } else {
                    EditOp::Remove
                };
                set = !set;
                for &cell in &cells {
                    seq += 1;
                    client
                        .send(ClientMsg::Edit(EditCommand {
                            id: CommandId {
                                issuer: player,
                                seq,
                            },
                            issuer: player,
                            op,
                            shape: Shape::Cell(cell),
                        }))
                        .expect("host is connected");
                }
                let start = Instant::now();
                host.tick();
                total += start.elapsed();
                client.drain();
            }
            total
        })
    });
}

criterion_group!(benches, host_tick_100);
criterion_main!(benches);
