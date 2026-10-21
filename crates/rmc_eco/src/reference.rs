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

use crate::chemistry::MoveLog;

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
#[derive(Clone, Debug)]
pub struct WaterRef {
    /// Role `air` (the one `Phase::Air` material).
    pub air: MaterialId,
    /// Role `water`.
    pub water: MaterialId,
    /// `eco.flow_cap`.
    pub flow_cap: i32,
    /// `eco.infil_shift`.
    pub infil_shift: u8,
    /// `eco.infil_side_shift`.
    pub infil_side_shift: u8,
    /// `eco.seep_num`.
    pub seep_num: i32,
    /// `eco.seep_shift`.
    pub seep_shift: u8,
    /// The porous solids: `(material, permeability, field_capacity)`.
    pub porous: Vec<(MaterialId, i32, i32)>,
}

/// The sideways divisor of SAD-P2 §7.3 B3: `(fill − fill_n) / 2`.
const REF_SIDE_SPLIT: i32 = 2;
/// The seepage threshold's divisor, `field_capacity · seep_num / 256`.
const REF_SEEP_DEN: i32 = 256;
/// The most water a cell holds.
const REF_FULL: i32 = u8::MAX as i32;

/// One water flow sweep (SAD-P2 §7.3 B steps 1 to 5) over every cell of
/// `flat` in global order, written directly on the flat arrays for the
/// oracle tests of [`WaterPass`](crate::water::WaterPass). Magma and
/// contact boiling are not modelled.
pub fn flat_sweep_water(flat: &mut FlatWorld, ids: &WaterRef) {
    let moist = LayerId::Moisture;
    // (permeability, field_capacity) of a porous cell.
    let porous = |flat: &FlatWorld, c: CellPos| {
        let m = flat.material(c)?;
        ids.porous
            .iter()
            .find(|p| p.0 == m)
            .map(|&(_, perm, cap)| (perm, cap))
    };
    // How much more water a cell takes; `None` when water cannot enter it.
    let room = |flat: &FlatWorld, c: CellPos| {
        let m = flat.material(c)?;
        if m == ids.air || m == ids.water {
            Some(REF_FULL - flat.layer(c, moist))
        } else {
            porous(flat, c).map(|(_, cap)| (cap - flat.layer(c, moist)).max(0))
        }
    };
    let give = |flat: &mut FlatWorld, from: CellPos, to: CellPos, amount: i32| {
        if flat.material(to) == Some(ids.air) {
            flat.set_material(to, ids.water);
        }
        flat.set_layer(from, moist, flat.layer(from, moist) - amount);
        flat.set_layer(to, moist, flat.layer(to, moist) + amount);
    };
    flat_sweep(flat, |flat, cell| {
        let c = cell.0;
        let below = CellPos::new(c.x, c.y - 1, c.z);
        let sides = [
            CellPos::new(c.x, c.y, c.z - 1),
            CellPos::new(c.x - 1, c.y, c.z),
            CellPos::new(c.x + 1, c.y, c.z),
            CellPos::new(c.x, c.y, c.z + 1),
        ];
        // 4. Seepage out of a porous cell above its threshold.
        if let Some((perm, cap)) = porous(flat, cell) {
            let threshold = cap * ids.seep_num / REF_SEEP_DEN;
            let excess = flat.layer(cell, moist) - threshold;
            let most = excess.min(perm / (1 << ids.seep_shift));
            if most <= 0 {
                return;
            }
            let first = std::iter::once(below)
                .chain(sides)
                .find_map(|n| room(flat, n).filter(|&r| r > 0).map(|r| (n, r)));
            if let Some((n, r)) = first {
                give(flat, cell, n, most.min(r));
            }
            return;
        }
        if flat.material(cell) == Some(ids.air) && flat.layer(cell, moist) > 0 {
            flat.set_material(cell, ids.water);
        }
        if flat.material(cell) != Some(ids.water) {
            return;
        }
        if let Some(r) = room(flat, below) {
            let fill = flat.layer(cell, moist);
            let amount = match porous(flat, below) {
                Some((perm, _)) => fill.min(perm / (1 << ids.infil_shift)).min(r),
                None => fill.min(r),
            };
            if amount > 0 {
                give(flat, cell, below, amount);
            }
        }
        for n in sides {
            let Some(r) = room(flat, n) else {
                continue;
            };
            let (fill, fill_n) = (flat.layer(cell, moist), flat.layer(n, moist));
            let amount = match porous(flat, n) {
                Some((perm, _)) => fill.min(perm / (1 << ids.infil_side_shift)).min(r),
                None if fill_n < fill => ids.flow_cap.min((fill - fill_n) / REF_SIDE_SPLIT).min(r),
                None => 0,
            };
            if amount > 0 {
                give(flat, cell, n, amount);
            }
        }
        if flat.layer(cell, moist) == 0 {
            flat.set_material(cell, ids.air);
        }
    });
}

/// The material and constants the gas reference reads for one gas.
#[derive(Clone, Debug)]
pub struct GasRef {
    /// Role `air` (the one `Phase::Air` material): gas moves only between
    /// air cells.
    pub air: MaterialId,
    /// The gas's layer index.
    pub gas: u8,
    /// `density` of the gas's row: negative rises, positive sinks.
    pub density: i32,
    /// `diffusion` of the gas's row.
    pub diffusion: i32,
    /// `eco.drift_shift`.
    pub drift_shift: u8,
    /// `eco.diff_shift`.
    pub diff_shift: u8,
}

/// The most gas a cell holds.
const REF_GAS_FULL: i32 = u8::MAX as i32;

/// One sweep of one gas (SAD-P2 §7.4 steps 1 and 2) over every cell of
/// `flat` in global order, written directly on the flat arrays for the
/// oracle tests of [`GasPass`](crate::gases::GasPass).
pub fn flat_sweep_gas(flat: &mut FlatWorld, ids: &GasRef) {
    let layer = LayerId::Gas(ids.gas);
    let is_air = |flat: &FlatWorld, c: CellPos| flat.material(c) == Some(ids.air);
    let rate = ids.density.abs() * (1 << ids.drift_shift);
    let give = |flat: &mut FlatWorld, from: CellPos, to: CellPos, amount: i32| {
        flat.set_layer(from, layer, flat.layer(from, layer) - amount);
        flat.set_layer(to, layer, flat.layer(to, layer) + amount);
    };
    flat_sweep(flat, |flat, cell| {
        if !is_air(flat, cell) || flat.layer(cell, layer) == 0 {
            return;
        }
        let c = cell.0;
        let at = |dx: i32, dy: i32, dz: i32| CellPos::new(c.x + dx, c.y + dy, c.z + dz);
        // 1. Drift: up for a light gas, down for a heavy one.
        let drift_to = match ids.density {
            d if d < 0 => Some(at(0, 1, 0)),
            d if d > 0 => Some(at(0, -1, 0)),
            _ => None,
        };
        if let Some(n) = drift_to.filter(|&n| is_air(flat, n)) {
            let room = REF_GAS_FULL - flat.layer(n, layer);
            let amount = flat.layer(cell, layer).min(rate).min(room);
            if amount > 0 {
                give(flat, cell, n, amount);
            }
        }
        // 2. Diffusion into each lower air neighbor, below first.
        let six = [
            at(0, -1, 0),
            at(0, 0, -1),
            at(-1, 0, 0),
            at(1, 0, 0),
            at(0, 0, 1),
            at(0, 1, 0),
        ];
        for n in six {
            if !is_air(flat, n) {
                continue;
            }
            let (v, v_n) = (flat.layer(cell, layer), flat.layer(n, layer));
            if v_n >= v {
                continue;
            }
            let amount = ((v - v_n) * ids.diffusion / (1 << ids.diff_shift))
                .min(REF_GAS_FULL - v_n)
                .min(v);
            if amount > 0 {
                give(flat, cell, n, amount);
            }
        }
    });
}

/// The constants the transport reference reads (SAD-P2 §7.6).
#[derive(Clone, Debug)]
pub struct TransportRef {
    /// Per substance row: `(mobility, mobility_acid)`.
    pub mobility: Vec<(i32, i32)>,
    /// Substance roles `acid` and `lime`.
    pub acid: Option<u8>,
    pub lime: Option<u8>,
    /// `eco.ph_shift`.
    pub ph_shift: u8,
    /// `eco.acid_ph`.
    pub acid_ph: i32,
}

/// The neutral pH and the top of the pH scale (§7.6).
const REF_PH_NEUTRAL: i32 = 7;
const REF_PH_MAX: i32 = 14;
/// The transported share's divisor, `(x · mob · amount) / 256`.
const REF_MOBILITY_DEN: i64 = 256;
/// The most of a substance a cell holds.
const REF_SUB_FULL: i64 = u8::MAX as i64;

/// The chemistry transport (SAD-P2 §7.6) written directly on the flat
/// arrays, for the oracle tests of
/// [`Transport`](crate::chemistry::Transport): replays `log` in order and
/// returns the substance units moved.
pub fn flat_transport(flat: &mut FlatWorld, log: &MoveLog, ids: &TransportRef) -> u64 {
    let mut moved = 0;
    for m in log.moves() {
        if m.from == m.to {
            continue;
        }
        let read = |x: Option<u8>| x.map_or(0, |x| flat.layer(m.from, LayerId::Substance(x)));
        let ph = (REF_PH_NEUTRAL + (read(ids.lime) - read(ids.acid)) / (1 << ids.ph_shift))
            .clamp(0, REF_PH_MAX);
        for (x, &(mobility, mobility_acid)) in ids.mobility.iter().enumerate() {
            let id = LayerId::Substance(x as u8);
            let mob = if ph < ids.acid_ph {
                mobility_acid
            } else {
                mobility
            };
            let (x_a, x_b) = (
                i64::from(flat.layer(m.from, id)),
                i64::from(flat.layer(m.to, id)),
            );
            let share = x_a * i64::from(mob) * i64::from(m.amount)
                / REF_MOBILITY_DEN
                / i64::from(m.fill_from);
            let k = share.min(x_a).min(REF_SUB_FULL - x_b);
            if k > 0 {
                flat.set_layer(m.from, id, (x_a - k) as i32);
                flat.set_layer(m.to, id, (x_b + k) as i32);
                moved += k as u64;
            }
        }
    }
    moved
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
