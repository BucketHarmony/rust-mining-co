//! Flat-array reference for the sweep passes (SAD-P2 §3).
//!
//! A [`FlatWorld`] copies a world's materials and chosen layers into plain
//! arrays over its bounds. [`flat_sweep`] visits every cell in the global
//! sweep order `(y, cz, cx, lz, lx)` with no active sets, so an oracle test
//! can run a pass's move rule on it and compare the result with
//! [`run_sweep`](crate::runner::run_sweep) over the chunked world.

use std::collections::BTreeMap;

use rmc_world::coords::{CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE, CellAabb, CellPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::world::World;

/// Every cell of `bounds` in the global sweep order: y ascending; within a
/// y-plane, chunks in (cz, cx) order; within a chunk, local (z, x).
pub fn global_order(bounds: CellAabb) -> impl Iterator<Item = CellPos> {
    let (lo, hi) = (bounds.min.0, bounds.max.0);
    let chunk = |v: i32| v >> CHUNK_SHIFT;
    (lo.y..=hi.y).flat_map(move |y| {
        (chunk(lo.z)..=chunk(hi.z)).flat_map(move |cz| {
            (chunk(lo.x)..=chunk(hi.x)).flat_map(move |cx| {
                (0..CHUNK_SIZE).flat_map(move |lz| {
                    (0..CHUNK_SIZE).filter_map(move |lx| {
                        let cell =
                            CellPos::new((cx << CHUNK_SHIFT) + lx, y, (cz << CHUNK_SHIFT) + lz);
                        bounds.contains(cell).then_some(cell)
                    })
                })
            })
        })
    })
}

/// The sort key of a cell in the global sweep order.
pub fn order_key(cell: CellPos) -> (i32, i32, i32, i32, i32) {
    let c = cell.0;
    (
        c.y,
        c.z >> CHUNK_SHIFT,
        c.x >> CHUNK_SHIFT,
        c.z & CHUNK_MASK,
        c.x & CHUNK_MASK,
    )
}

/// Materials and layers of a box of cells in flat arrays, indexed
/// `x + z·sx + y·sx·sz` from the box minimum. Layers not copied in read 0
/// and are created on the first write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatWorld {
    bounds: CellAabb,
    size: [usize; 3],
    materials: Vec<MaterialId>,
    layers: BTreeMap<LayerId, Vec<i32>>,
}

impl FlatWorld {
    /// Copies `world`'s bounds, materials and the layers `ids`. Cells of
    /// non-resident chunks read the default material.
    pub fn from_world(world: &World, ids: &[LayerId]) -> Self {
        let bounds = world.bounds();
        let d = bounds.max.0 - bounds.min.0;
        let size = [d.x as usize + 1, d.y as usize + 1, d.z as usize + 1];
        let mut flat = Self {
            bounds,
            size,
            materials: vec![MaterialId::default(); size[0] * size[1] * size[2]],
            layers: BTreeMap::new(),
        };
        for cell in global_order(bounds) {
            let i = flat.index(cell);
            flat.materials[i] = world.get_cell(cell).unwrap_or_default();
        }
        for &id in ids {
            let mut v = vec![0; flat.materials.len()];
            for cell in global_order(bounds) {
                v[flat.index(cell)] = world.layer(cell, id);
            }
            flat.layers.insert(id, v);
        }
        flat
    }

    pub fn bounds(&self) -> CellAabb {
        self.bounds
    }

    pub fn contains(&self, cell: CellPos) -> bool {
        self.bounds.contains(cell)
    }

    /// Material at `cell`; `None` outside the bounds.
    pub fn material(&self, cell: CellPos) -> Option<MaterialId> {
        self.contains(cell)
            .then(|| self.materials[self.index(cell)])
    }

    /// Panics outside the bounds.
    pub fn set_material(&mut self, cell: CellPos, id: MaterialId) {
        let i = self.index_checked(cell);
        self.materials[i] = id;
    }

    /// Value of layer `id` at `cell`; 0 outside the bounds or when the
    /// layer was never copied or written.
    pub fn layer(&self, cell: CellPos, id: LayerId) -> i32 {
        if !self.contains(cell) {
            return 0;
        }
        self.layers.get(&id).map_or(0, |v| v[self.index(cell)])
    }

    /// Panics outside the bounds.
    pub fn set_layer(&mut self, cell: CellPos, id: LayerId, v: i32) {
        let i = self.index_checked(cell);
        let n = self.materials.len();
        self.layers.entry(id).or_insert_with(|| vec![0; n])[i] = v;
    }

    /// The first cell, in global order, where `world` differs from this
    /// copy in material or in one of the layers this copy holds, as
    /// `(cell, what)`.
    pub fn first_difference(&self, world: &World) -> Option<(CellPos, String)> {
        global_order(self.bounds).find_map(|cell| {
            let mine = self.material(cell).unwrap_or_default();
            let theirs = world.get_cell(cell).unwrap_or_default();
            if mine != theirs {
                return Some((cell, format!("material {mine:?} vs {theirs:?}")));
            }
            self.layers.keys().find_map(|&id| {
                let (a, b) = (self.layer(cell, id), world.layer(cell, id));
                (a != b).then(|| (cell, format!("{id:?} {a} vs {b}")))
            })
        })
    }

    fn index(&self, cell: CellPos) -> usize {
        let d = cell.0 - self.bounds.min.0;
        d.x as usize + d.z as usize * self.size[0] + d.y as usize * self.size[0] * self.size[2]
    }

    fn index_checked(&self, cell: CellPos) -> usize {
        assert!(self.contains(cell), "{cell:?} outside the flat world");
        self.index(cell)
    }
}

/// Visits every cell of `flat` once, in the global sweep order; `visit`
/// applies its moves to `flat` immediately.
pub fn flat_sweep(flat: &mut FlatWorld, mut visit: impl FnMut(&mut FlatWorld, CellPos)) {
    for cell in global_order(flat.bounds) {
        visit(flat, cell);
    }
}

/// The materials and constants the water reference reads.
#[derive(Clone, Copy, Debug)]
pub struct WaterRef {
    /// Role `air` (the one `Phase::Air` material).
    pub air: MaterialId,
    /// Role `water`.
    pub water: MaterialId,
    /// `eco.flow_cap`.
    pub flow_cap: i32,
}

/// The sideways divisor of SAD-P2 §7.3 B3: `(fill − fill_n) / 2`.
const REF_SIDE_SPLIT: i32 = 2;
/// The most water a cell holds.
const REF_FULL: i32 = u8::MAX as i32;

/// One water flow sweep (SAD-P2 §7.3 B steps 1, 2, 3 and 5) over every
/// cell of `flat` in global order, written directly on the flat arrays
/// for the oracle tests of [`WaterPass`](crate::water::WaterPass).
/// Porous solids, magma and contact boiling are not modelled.
pub fn flat_sweep_water(flat: &mut FlatWorld, ids: WaterRef) {
    let moist = LayerId::Moisture;
    let holds_water = |flat: &FlatWorld, c: CellPos| {
        let m = flat.material(c);
        m == Some(ids.air) || m == Some(ids.water)
    };
    let give = |flat: &mut FlatWorld, from: CellPos, to: CellPos, amount: i32| {
        if flat.material(to) == Some(ids.air) {
            flat.set_material(to, ids.water);
        }
        flat.set_layer(from, moist, flat.layer(from, moist) - amount);
        flat.set_layer(to, moist, flat.layer(to, moist) + amount);
    };
    flat_sweep(flat, |flat, cell| {
        if flat.material(cell) == Some(ids.air) && flat.layer(cell, moist) > 0 {
            flat.set_material(cell, ids.water);
        }
        if flat.material(cell) != Some(ids.water) {
            return;
        }
        let c = cell.0;
        let below = CellPos::new(c.x, c.y - 1, c.z);
        if holds_water(flat, below) {
            let room = REF_FULL - flat.layer(below, moist);
            let amount = flat.layer(cell, moist).min(room);
            if amount > 0 {
                give(flat, cell, below, amount);
            }
        }
        for n in [
            CellPos::new(c.x, c.y, c.z - 1),
            CellPos::new(c.x - 1, c.y, c.z),
            CellPos::new(c.x + 1, c.y, c.z),
            CellPos::new(c.x, c.y, c.z + 1),
        ] {
            if !holds_water(flat, n) {
                continue;
            }
            let (fill, fill_n) = (flat.layer(cell, moist), flat.layer(n, moist));
            if fill_n >= fill {
                continue;
            }
            let amount = ids
                .flow_cap
                .min((fill - fill_n) / REF_SIDE_SPLIT)
                .min(REF_FULL - fill_n);
            if amount > 0 {
                give(flat, cell, n, amount);
            }
        }
        if flat.layer(cell, moist) == 0 {
            flat.set_material(cell, ids.air);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_order_is_y_cz_cx_lz_lx_and_covers_bounds() {
        let bounds = CellAabb::new(CellPos::new(-33, -2, -1), CellPos::new(40, 1, 33));
        let cells: Vec<CellPos> = global_order(bounds).collect();
        let d = bounds.max.0 - bounds.min.0 + 1;
        assert_eq!(cells.len(), (d.x * d.y * d.z) as usize);
        assert!(cells.windows(2).all(|w| order_key(w[0]) < order_key(w[1])));
        assert!(cells.iter().all(|&c| bounds.contains(c)));
        // Within a y-plane the chunk at cx = -2 comes before cx = -1, which
        // comes before cx = 0, for the same cz.
        assert_eq!(cells[0], CellPos::new(-33, -2, -1));
        assert_eq!(cells[1], CellPos::new(-64 + 32, -2, -1));
    }
}
