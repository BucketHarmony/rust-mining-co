//! How the client reads liquid fills (a liquid cell's `moisture` layer).
//!
//! The host's water sweep pours a cell's water into a non-full cell of the
//! same liquid below it. Rain, though, lands on top of a pond at the end of
//! each sub-step, after the sweeps (SAD-P2 §7.3 D: `surface_y + 1`, and a
//! pond is its column's surface), so every snapshot and save holds a thin
//! film one cell above a pond whose top cell is not full. Drawn as it is,
//! that film is a sheet floating a cell above the water, and the player
//! swims in it. The next sweep pours it down.
//!
//! So the client reads each vertical run of one liquid as if its water had
//! already poured down: the run's total, from the bottom up, a full cell at
//! a time ([`poured`]). The mesher draws that level, and the controller
//! swims by it. Nothing here changes the replica.

use rmc_world::coords::CellPos;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::world::World;

/// A full cell's fill.
const FULL: u32 = u8::MAX as u32;

/// The thinnest poured fill the mesher draws: a fresh rain film (a
/// sub-step's rain is at most `rain`, 10 in `seasons.ron`) is wet ground,
/// not a sheet of water. A look constant (CLAUDE.md rule 21).
pub const MIN_DRAWN_FILL: u8 = 16;

/// The fill of the `k`-th cell (0 at the bottom) of a run of one liquid
/// that holds `total` in all, once its water has poured down.
pub fn poured(total: u32, k: u32) -> u8 {
    let left = total.saturating_sub(k.saturating_mul(FULL)).min(FULL);
    u8::try_from(left).unwrap_or(u8::MAX)
}

/// The cells of material `m` in an unbroken line from `cell` (excluded),
/// going down for a `step` of -1 or up for +1, and the fill they hold.
pub fn run_beyond(world: &World, cell: CellPos, m: MaterialId, step: i32) -> (u32, u32) {
    let (mut cells, mut total) = (0, 0);
    let mut c = CellPos::new(cell.0.x, cell.0.y + step, cell.0.z);
    while world.get_cell(c) == Some(m) {
        cells += 1;
        total += u32::try_from(world.layer(c, LayerId::Moisture)).unwrap_or(0);
        c = CellPos::new(c.0.x, c.0.y + step, c.0.z);
    }
    (cells, total)
}

/// For the cell `cell`, its fill once the vertical run of its material
/// through it has poured down, and the run's total fill (0 for a run that
/// holds no fill at all, such as water placed without the ecology).
/// `None` outside the resident world.
pub fn settled_fill(world: &World, cell: CellPos) -> Option<(u8, u32)> {
    let m = world.get_cell(cell)?;
    let (below, under) = run_beyond(world, cell, m, -1);
    let (_, over) = run_beyond(world, cell, m, 1);
    let own = u32::try_from(world.layer(cell, LayerId::Moisture)).unwrap_or(0);
    let total = under + own + over;
    Some((poured(total, below), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::CellAabb;
    use rmc_world::materials::MaterialTable;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn world() -> (World, MaterialId) {
        let table = Arc::new(MaterialTable::load(MATERIALS_PATH).expect("materials load"));
        let water = table.id("water").unwrap();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 63, 31));
        let mut world = World::new(table, bounds);
        for pos in bounds.chunks() {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        (world, water)
    }

    fn put(world: &mut World, cell: CellPos, m: MaterialId, fill: u8) {
        let (pos, local) = cell.split();
        let mut chunk = world.chunk(pos).unwrap().clone();
        chunk.storage.set(local, m);
        chunk.layers.set_u8(LayerId::Moisture, local, fill);
        chunk.revision += 1;
        world.insert_chunk(pos, chunk).unwrap();
    }

    #[test]
    fn poured_fills_full_cells_from_the_bottom() {
        assert_eq!(poured(0, 0), 0);
        assert_eq!(poured(30, 0), 30);
        assert_eq!(poured(30, 1), 0);
        assert_eq!(poured(300, 0), 255);
        assert_eq!(poured(300, 1), 45);
        assert_eq!(poured(300, 2), 0);
        assert_eq!(poured(u32::MAX, 3), 255);
    }

    #[test]
    fn a_film_over_a_shallow_cell_pours_into_it() {
        let (mut w, water) = world();
        let (low, film) = (CellPos::new(4, 10, 4), CellPos::new(4, 11, 4));
        put(&mut w, low, water, 20);
        put(&mut w, film, water, 4);
        assert_eq!(settled_fill(&w, low), Some((24, 24)));
        assert_eq!(settled_fill(&w, film), Some((0, 24)), "the film pours away");

        // A run across a chunk face (y 31 | 32) reads the same from both.
        let (a, b, c) = (
            CellPos::new(4, 31, 4),
            CellPos::new(4, 32, 4),
            CellPos::new(4, 33, 4),
        );
        put(&mut w, a, water, 255);
        put(&mut w, b, water, 100);
        put(&mut w, c, water, 200);
        assert_ne!(a.split().0, b.split().0);
        assert_eq!(settled_fill(&w, a), Some((255, 555)));
        assert_eq!(settled_fill(&w, b), Some((255, 555)));
        assert_eq!(settled_fill(&w, c), Some((45, 555)));
        assert_eq!(run_beyond(&w, b, water, -1), (1, 255));
        assert_eq!(run_beyond(&w, b, water, 1), (1, 200));

        // Placed without the ecology, a run holds no fill.
        let bare = CellPos::new(9, 5, 9);
        put(&mut w, bare, water, 0);
        assert_eq!(settled_fill(&w, bare), Some((0, 0)));
        assert_eq!(settled_fill(&w, CellPos::new(9, 99, 9)), None);
    }
}
