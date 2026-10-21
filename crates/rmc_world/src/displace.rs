//! The displacement rules (SAD-P2 §7.0): what happens to a cell's layers,
//! its neighbors' layers and its plant when its material changes.
//!
//! Implemented once and run by both [`World::apply`] (edits: effects become
//! `ChunkDelta::Layers`, ledger terms go on the `EditResult`) and
//! [`World::eco_set`] (season passes: no deltas, ledger terms go into the
//! season's ledger). Roles come from [`EcoRules`], never from keys.

use std::collections::{BTreeMap, BTreeSet};

use crate::coords::{CHUNK_VOLUME, CellPos, ChunkPos};
use crate::edit::EditOp;
use crate::layers::{LayerId, N_SUBSTANCES, PlantState, SWEEP_GAS, SWEEP_WATER};
use crate::ledger::SeasonLedger;
use crate::materials::{MaterialId, Phase};
use crate::world::World;

/// The ecology roles and constants the displacement rules read. Built by
/// `rmc_eco` from `eco.ron`, `plants.ron` and `substances.ron`; the default
/// (no roles) leaves water, ice, magma, steam and litter rules inert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EcoRules {
    /// Role `water`.
    pub water: Option<MaterialId>,
    /// Role `ice`.
    pub ice: Option<MaterialId>,
    /// Role `magma`.
    pub magma: Option<MaterialId>,
    /// Gas index of role `steam`.
    pub steam: Option<u8>,
    /// Substance index of role `organic`.
    pub organic: Option<u8>,
    /// `eco.magma_init_temp`, 0.1 °C: what brush `Set(magma)` writes.
    pub magma_init_temp: i16,
    /// `litter` per plant species index.
    pub plant_litter: Vec<u8>,
    /// `eco.harvest_shift`.
    pub harvest_shift: u8,
    /// `yield_material` per substance index.
    pub substance_yields: [Option<MaterialId>; N_SUBSTANCES],
}

/// What caused a material change: an edit (with its op) or a season pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    Edit(EditOp),
    Pass,
}

impl Cause {
    /// The pickaxe harvests a plant it breaks (§7.8).
    fn harvests(self) -> bool {
        matches!(self, Cause::Edit(EditOp::Damage(_)))
    }

    /// Brush `Set` (and `Paint`, and a placed cell) of water or ice writes a
    /// full cell.
    fn brush_fills(self) -> bool {
        matches!(
            self,
            Cause::Edit(EditOp::Set(_) | EditOp::Paint(_) | EditOp::Place(_))
        )
    }
}

/// How the rules see a material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Air,
    Water,
    Ice,
    Magma,
    Other,
}

impl Class {
    /// Air, water and ice hold moisture freely; anything else displaces it.
    fn holds_moisture(self) -> bool {
        matches!(self, Class::Air | Class::Water | Class::Ice)
    }
}

/// Everything one edit or one `eco_set` changed, per chunk.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    /// Material changes, by chunk and local index (last write wins).
    pub cells: BTreeMap<ChunkPos, BTreeMap<u16, MaterialId>>,
    /// Layer changes as final values.
    pub layers: BTreeMap<ChunkPos, BTreeMap<(u16, LayerId), i32>>,
    /// Plant changes as final states (`None`: removed).
    pub plants: BTreeMap<ChunkPos, BTreeMap<u16, Option<PlantState>>>,
    /// Cells whose material or layers changed, for the active sets.
    pub touched: BTreeSet<CellPos>,
    /// Harvest yields.
    pub yields: BTreeMap<MaterialId, u32>,
}

/// The six face neighbors in sorted (y, z, x) order: below first.
fn neighbors(cell: CellPos) -> [CellPos; 6] {
    let c = cell.0;
    [
        CellPos::new(c.x, c.y - 1, c.z),
        CellPos::new(c.x, c.y, c.z - 1),
        CellPos::new(c.x - 1, c.y, c.z),
        CellPos::new(c.x + 1, c.y, c.z),
        CellPos::new(c.x, c.y, c.z + 1),
        CellPos::new(c.x, c.y + 1, c.z),
    ]
}

fn above(cell: CellPos) -> CellPos {
    CellPos::new(cell.0.x, cell.0.y + 1, cell.0.z)
}

fn below(cell: CellPos) -> CellPos {
    CellPos::new(cell.0.x, cell.0.y - 1, cell.0.z)
}

/// Largest value a layer holds.
fn layer_max(id: LayerId) -> i32 {
    if id.is_i16() {
        i32::from(i16::MAX)
    } else {
        i32::from(u8::MAX)
    }
}

/// Whether `v` fits the layer's value type.
pub(crate) fn layer_value_fits(id: LayerId, v: i32) -> bool {
    if id.is_i16() {
        i16::try_from(v).is_ok()
    } else {
        u8::try_from(v).is_ok()
    }
}

impl World {
    /// The displacement rules in force.
    pub fn eco_rules(&self) -> &EcoRules {
        &self.rules
    }

    /// Sets the roles the displacement rules read (host only; replicas take
    /// layer effects from deltas).
    pub fn set_eco_rules(&mut self, rules: EcoRules) {
        self.rules = std::sync::Arc::new(rules);
    }

    /// Value of layer `id` at `cell`, widened to `i32`; 0 when the layer is
    /// absent or the cell is outside the bounds or not resident.
    pub fn layer(&self, cell: CellPos, id: LayerId) -> i32 {
        if !self.bounds.contains(cell) {
            return 0;
        }
        let (pos, local) = cell.split();
        self.chunks.get(&pos).map_or(0, |c| {
            if id.is_i16() {
                i32::from(c.layers.get_i16(id, local))
            } else {
                i32::from(c.layers.get_u8(id, local))
            }
        })
    }

    /// Plant state at `cell`, if any.
    pub fn plant(&self, cell: CellPos) -> Option<&PlantState> {
        let (pos, local) = cell.split();
        self.chunks.get(&pos)?.plants.get(&local.index())
    }

    /// Season-pass cell write (SAD-P2 §7.0.1). Host-only, called only by
    /// `rmc_eco` inside `season_tick`.
    ///
    /// Runs the full §7.0 displacement set for the (from, to) pair,
    /// including the boiling exception (water to air turns the fill into
    /// steam) and the push of moisture, substances and gases out of a cell
    /// that can no longer hold them, with remainders charged to the
    /// `*saturation_loss` terms of `ledger`. Updates `surface_y` and the
    /// active sets of every cell it changed or pushed into, bumps the
    /// `revision` (and, when layers or plants changed, the `eco_revision`)
    /// of each chunk it touched, sets `modified` and clears the cell's
    /// damage. No validation, no delta. `from == to`, a cell outside the
    /// bounds and a non-resident chunk change nothing.
    pub fn eco_set(&mut self, cell: CellPos, to: MaterialId, ledger: &mut SeasonLedger) {
        if !self.bounds.contains(cell) {
            return;
        }
        let mut fx = Effects::default();
        self.change_material(cell, to, Cause::Pass, ledger, &mut fx);
        self.finish_effects(&fx);
    }

    /// Season-pass layer write (rule 14). Host-only, called only by
    /// `rmc_eco` inside `season_tick`.
    ///
    /// Sets layer `id` at `cell` to `v` and returns whether it changed. On a
    /// change it bumps the chunk's `eco_revision` and, like
    /// [`World::eco_set`], adds the cell to the water sweep when it holds
    /// moisture or is water or magma, and to gas sweep `g` when it holds
    /// gas `g`. A cell outside
    /// the bounds or in a non-resident chunk changes nothing. Panics if `id`
    /// is out of range or `v` does not fit the layer's type.
    pub fn eco_put_layer(&mut self, cell: CellPos, id: LayerId, v: i32) -> bool {
        assert!(id.is_valid(), "layer {id:?} index out of range");
        assert!(layer_value_fits(id, v), "value {v} does not fit {id:?}");
        if !self.bounds.contains(cell) {
            return false;
        }
        // The single-cell case of `put_layer` + `finish_effects`, without
        // their effect maps: the sweeps call this for every move.
        let wet_material = self
            .get_cell(cell)
            .is_some_and(|id| matches!(self.class(id), Class::Water | Class::Magma));
        let (pos, local) = cell.split();
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return false;
        };
        if id.is_i16() {
            let v = i16::try_from(v).expect("i16 layer value in range");
            if chunk.layers.get_i16(id, local) == v {
                return false;
            }
            chunk.layers.set_i16(id, local, v);
        } else {
            let v = u8::try_from(v).expect("u8 layer value in range");
            if chunk.layers.get_u8(id, local) == v {
                return false;
            }
            chunk.layers.set_u8(id, local, v);
        }
        chunk.eco_revision += 1;
        if wet_material || chunk.layers.get_u8(LayerId::Moisture, local) > 0 {
            chunk.active[SWEEP_WATER].insert(local);
        }
        for g in 0..crate::layers::N_GASES as u8 {
            if chunk.layers.get_u8(LayerId::Gas(g), local) > 0 {
                chunk.active[SWEEP_GAS + usize::from(g)].insert(local);
            }
        }
        true
    }

    /// Season-pass plant write (rule 14). Host-only, called only by
    /// `rmc_eco` inside `season_tick`.
    ///
    /// Replaces the plant state at `cell` (`None` removes it) and returns
    /// whether it changed; a change bumps the chunk's `eco_revision`. A
    /// cell outside the bounds or in a non-resident chunk changes nothing.
    pub fn eco_put_plant(&mut self, cell: CellPos, plant: Option<PlantState>) -> bool {
        if !self.bounds.contains(cell) {
            return false;
        }
        let (pos, local) = cell.split();
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return false;
        };
        let old = match plant {
            Some(p) => chunk.plants.insert(local.index(), p),
            None => chunk.plants.remove(&local.index()),
        };
        if old == plant {
            return false;
        }
        chunk.eco_revision += 1;
        true
    }

    /// Adds `cell` to the active set of sweep `sweep` (SAD-P2 §3); returns
    /// whether it was absent. Host-only, for `rmc_eco`'s sweep runner. A
    /// cell outside the bounds or in a non-resident chunk is never active.
    /// Panics if `sweep` is not below `N_SWEEPS`.
    pub fn eco_activate(&mut self, cell: CellPos, sweep: usize) -> bool {
        if !self.bounds.contains(cell) {
            return false;
        }
        let (pos, local) = cell.split();
        self.chunks
            .get_mut(&pos)
            .is_some_and(|c| c.active[sweep].insert(local))
    }

    /// Sets the freeze-thaw bit of `cell` (SAD-P2 §7.5); returns whether it
    /// was clear. Host-only, for `rmc_eco`'s temperature pass. A cell
    /// outside the bounds or in a non-resident chunk has no bit.
    pub fn eco_mark_freeze_thaw(&mut self, cell: CellPos) -> bool {
        if !self.bounds.contains(cell) {
            return false;
        }
        let (pos, local) = cell.split();
        self.chunks
            .get_mut(&pos)
            .is_some_and(|c| c.freeze_thaw.insert(local))
    }

    /// Clears the freeze-thaw bits of every chunk (SAD-P2 §7.5: at the
    /// start of each season). Host-only, for `rmc_eco`'s season tick.
    pub fn eco_clear_freeze_thaw(&mut self) {
        for chunk in self.chunks.values_mut() {
            chunk.freeze_thaw.clear();
        }
    }

    /// Removes `cell` from the active set of sweep `sweep`; returns whether
    /// it was present. Panics if `sweep` is not below `N_SWEEPS`.
    pub fn eco_deactivate(&mut self, cell: CellPos, sweep: usize) -> bool {
        let (pos, local) = cell.split();
        self.chunks
            .get_mut(&pos)
            .is_some_and(|c| c.active[sweep].remove(local))
    }

    fn class(&self, id: MaterialId) -> Class {
        let r = &self.rules;
        if Some(id) == r.water {
            Class::Water
        } else if Some(id) == r.ice {
            Class::Ice
        } else if Some(id) == r.magma {
            Class::Magma
        } else if self
            .materials
            .get(id)
            .is_some_and(|d| d.phase == Phase::Air)
        {
            Class::Air
        } else {
            Class::Other
        }
    }

    fn is_air(&self, cell: CellPos) -> bool {
        self.get_cell(cell)
            .is_some_and(|id| self.class(id) == Class::Air)
    }

    /// Writes `v` into layer `id` at `cell` if the chunk is resident and the
    /// value differs, recording the change.
    pub(crate) fn put_layer(&mut self, cell: CellPos, id: LayerId, v: i32, fx: &mut Effects) {
        if !self.bounds.contains(cell) {
            return;
        }
        let (pos, local) = cell.split();
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return;
        };
        let old = if id.is_i16() {
            i32::from(chunk.layers.get_i16(id, local))
        } else {
            i32::from(chunk.layers.get_u8(id, local))
        };
        if old == v {
            return;
        }
        if id.is_i16() {
            let v = i16::try_from(v).expect("i16 layer value in range");
            chunk.layers.set_i16(id, local, v);
        } else {
            let v = u8::try_from(v).expect("u8 layer value in range");
            chunk.layers.set_u8(id, local, v);
        }
        fx.layers
            .entry(pos)
            .or_default()
            .insert((local.index(), id), v);
        fx.touched.insert(cell);
    }

    /// Adds up to `amount` to layer `id` at `cell`, capped by the layer's
    /// maximum; returns what was added. A non-resident cell takes nothing.
    fn deposit(&mut self, cell: CellPos, id: LayerId, amount: i32, fx: &mut Effects) -> i32 {
        if amount <= 0 || !self.bounds.contains(cell) || self.chunk(cell.chunk()).is_none() {
            return 0;
        }
        let have = self.layer(cell, id);
        let added = amount.min(layer_max(id) - have);
        self.put_layer(cell, id, have + added, fx);
        added
    }

    /// Changes the material at `cell` to `to` and runs the displacement
    /// rules. Returns whether the material changed.
    pub(crate) fn change_material(
        &mut self,
        cell: CellPos,
        to: MaterialId,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) -> bool {
        let (pos, local) = cell.split();
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return false;
        };
        let from = chunk.storage.get(local);
        if from == to {
            return false;
        }
        chunk.storage.set(local, to);
        self.damage.clear(cell);
        fx.cells.entry(pos).or_default().insert(local.index(), to);
        fx.touched.insert(cell);
        self.columns
            .cell_changed(cell, from, to, &self.chunks, &self.materials, self.bounds);
        self.displace(cell, from, to, cause, ledger, fx);
        true
    }

    /// The §7.0 rules for one changed cell.
    fn displace(
        &mut self,
        cell: CellPos,
        from: MaterialId,
        to: MaterialId,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let (fc, tc) = (self.class(from), self.class(to));
        let pass = cause == Cause::Pass;

        // Plant removal: dig harvests, anything else returns `held` to the
        // anchor below.
        if let Some(plant) = self.take_plant(cell, fx) {
            if cause.harvests() {
                self.harvest(&plant, ledger, fx);
            } else {
                self.return_held(cell, &plant, cause, ledger, fx);
            }
            self.remove_tall_top(cell, from, cause, ledger, fx);
        }

        // Magma is counted in cells.
        if fc == Class::Magma && tc != Class::Magma {
            if pass {
                ledger.magma_set += 1;
            } else {
                ledger.magma_edit -= 1;
            }
        }
        if tc == Class::Magma && fc != Class::Magma {
            if pass {
                ledger.magma_melt_in += 1;
            } else {
                ledger.magma_edit += 1;
                let t = i32::from(self.rules.magma_init_temp);
                self.put_layer(cell, LayerId::Temperature, t, fx);
            }
        }

        // Boiling: a pass turning water into air converts the fill to steam.
        if pass
            && fc == Class::Water
            && tc == Class::Air
            && let Some(steam) = self.rules.steam
        {
            let fill = self.layer(cell, LayerId::Moisture);
            self.put_layer(cell, LayerId::Moisture, 0, fx);
            let added = self.deposit(cell, LayerId::Gas(steam), fill, fx);
            ledger.boil_to_steam += i64::from(added);
            // Fill that found no room as steam never became gas: it is
            // water lost, not steam lost.
            ledger.water_saturation_loss += i64::from(fill - added);
        }

        // Air becoming anything else pushes its gases out.
        if fc == Class::Air && tc != Class::Air {
            self.push_gases(cell, cause, ledger, fx);
        }

        // A cell that can no longer hold moisture pushes it out.
        if fc.holds_moisture() && !tc.holds_moisture() {
            self.push_moisture(cell, cause, ledger, fx);
        }

        // Brush water or ice is a full cell.
        if cause.brush_fills() && matches!(tc, Class::Water | Class::Ice) {
            let have = self.layer(cell, LayerId::Moisture);
            let full = i32::from(u8::MAX);
            self.put_layer(cell, LayerId::Moisture, full, fx);
            ledger.water_edit_in += i64::from(full - have);
        }

        // Becoming air: weathering and growth reset; a plant on top dies.
        if tc == Class::Air && fc != Class::Air {
            self.put_layer(cell, LayerId::Regolith, 0, fx);
            self.put_layer(cell, LayerId::Growth, 0, fx);
            self.kill_plant_above(cell, cause, ledger, fx);
        }
    }

    /// Removes and returns the plant state at `cell`, recording it.
    fn take_plant(&mut self, cell: CellPos, fx: &mut Effects) -> Option<PlantState> {
        let (pos, local) = cell.split();
        let plant = self.chunks.get_mut(&pos)?.plants.remove(&local.index())?;
        fx.plants
            .entry(pos)
            .or_default()
            .insert(local.index(), None);
        fx.touched.insert(cell);
        Some(plant)
    }

    /// Charges an amount displaced with nowhere to go: `edit_out` for edits,
    /// `saturation_loss` for passes.
    fn lose_substance(x: usize, amount: i32, cause: Cause, ledger: &mut SeasonLedger) {
        let amount = i64::from(amount);
        if cause == Cause::Pass {
            ledger.saturation_loss[x] += amount;
        } else {
            ledger.edit_out[x] += amount;
        }
    }

    fn lose_water(amount: i32, cause: Cause, ledger: &mut SeasonLedger) {
        let amount = i64::from(amount);
        if cause == Cause::Pass {
            ledger.water_saturation_loss += amount;
        } else {
            ledger.water_edit_out += amount;
        }
    }

    fn lose_gas(&self, g: u8, amount: i32, cause: Cause, ledger: &mut SeasonLedger) {
        if self.rules.steam == Some(g) {
            Self::lose_water(amount, cause, ledger);
        }
        let amount = i64::from(amount);
        if cause == Cause::Pass {
            ledger.gas_saturation_loss[usize::from(g)] += amount;
        } else {
            ledger.gas_edit_out[usize::from(g)] += amount;
        }
    }

    /// Harvest (§7.8): every held substance leaves through `harvest_out`;
    /// one with a yield material yields `max(1, held >> harvest_shift)`.
    fn harvest(&mut self, plant: &PlantState, ledger: &mut SeasonLedger, fx: &mut Effects) {
        for (x, &held) in plant.held.iter().enumerate() {
            if held == 0 {
                continue;
            }
            ledger.harvest_out[x] += i64::from(held);
            if let Some(m) = self.rules.substance_yields[x] {
                let n = (u32::from(held) >> self.rules.harvest_shift).max(1);
                *fx.yields.entry(m).or_insert(0) += n;
            }
        }
    }

    /// Plant removal without harvest: `held` goes into the anchor below if
    /// its material is an `anchor`, by room; the rest is lost.
    fn return_held(
        &mut self,
        cell: CellPos,
        plant: &PlantState,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let anchor = below(cell);
        let is_anchor = self
            .get_cell(anchor)
            .and_then(|id| self.materials.get(id))
            .is_some_and(|d| d.anchor);
        for (x, &held) in plant.held.iter().enumerate() {
            let held = i32::from(held);
            let added = if is_anchor {
                self.deposit(anchor, LayerId::Substance(x as u8), held, fx)
            } else {
                0
            };
            if held > added {
                Self::lose_substance(x, held - added, cause, ledger);
            }
        }
    }

    /// A tall plant's upper cell (same material, no plant state) goes with
    /// the plant.
    fn remove_tall_top(
        &mut self,
        cell: CellPos,
        plant_material: MaterialId,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let top = above(cell);
        if self.get_cell(top) == Some(plant_material) && self.plant(top).is_none() {
            self.change_material(top, MaterialId::AIR, cause, ledger, fx);
        }
    }

    /// The §7.8 death rule for a plant whose anchor `cell` just became air:
    /// litter and `held` go into `cell` by room (litter is a `litter_in`
    /// source, its remainder `saturation_loss`; the rest of `held` is lost),
    /// the plant cell becomes air and `plants_died` counts it.
    fn kill_plant_above(
        &mut self,
        cell: CellPos,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let top = above(cell);
        let Some(plant_material) = self.get_cell(top) else {
            return;
        };
        let Some(plant) = self.take_plant(top, fx) else {
            return;
        };
        ledger.plants_died += 1;
        let litter = self
            .rules
            .plant_litter
            .get(usize::from(plant.species))
            .copied()
            .map_or(0, i32::from);
        if let Some(c) = self.rules.organic
            && litter > 0
        {
            let added = self.deposit(cell, LayerId::Substance(c), litter, fx);
            ledger.litter_in[usize::from(c)] += i64::from(litter);
            ledger.saturation_loss[usize::from(c)] += i64::from(litter - added);
        }
        for (x, &held) in plant.held.iter().enumerate() {
            let held = i32::from(held);
            let added = self.deposit(cell, LayerId::Substance(x as u8), held, fx);
            if held > added {
                Self::lose_substance(x, held - added, cause, ledger);
            }
        }
        self.remove_tall_top(top, plant_material, cause, ledger, fx);
        self.change_material(top, MaterialId::AIR, cause, ledger, fx);
    }

    /// Pushes every gas on `cell` into its air neighbors in sorted order,
    /// each up to its room; the remainder is lost.
    fn push_gases(
        &mut self,
        cell: CellPos,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let receivers: Vec<CellPos> = neighbors(cell)
            .into_iter()
            .filter(|&n| self.is_air(n))
            .collect();
        for g in 0..crate::layers::N_GASES as u8 {
            let id = LayerId::Gas(g);
            let mut left = self.layer(cell, id);
            if left == 0 {
                continue;
            }
            self.put_layer(cell, id, 0, fx);
            for &n in &receivers {
                if left == 0 {
                    break;
                }
                left -= self.deposit(n, id, left, fx);
            }
            if left > 0 {
                self.lose_gas(g, left, cause, ledger);
            }
        }
    }

    /// Pushes the cell's moisture to the first air or water neighbor in
    /// sorted order with room (as much as fits). Substances follow the water
    /// proportionally, by the receiver's room; what does not follow stays in
    /// the cell. Water that does not fit is lost.
    fn push_moisture(
        &mut self,
        cell: CellPos,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let m = self.layer(cell, LayerId::Moisture);
        if m == 0 {
            return;
        }
        let receiver = neighbors(cell).into_iter().find(|&n| {
            self.get_cell(n)
                .is_some_and(|id| matches!(self.class(id), Class::Air | Class::Water))
                && self.layer(n, LayerId::Moisture) < i32::from(u8::MAX)
        });
        self.put_layer(cell, LayerId::Moisture, 0, fx);
        let moved = match receiver {
            Some(n) => self.deposit(n, LayerId::Moisture, m, fx),
            None => 0,
        };
        if m > moved {
            Self::lose_water(m - moved, cause, ledger);
        }
        let Some(n) = receiver else {
            return;
        };
        for x in 0..N_SUBSTANCES as u8 {
            let id = LayerId::Substance(x);
            let s = self.layer(cell, id);
            if s == 0 {
                continue;
            }
            // s ≤ FULL and moved ≤ m ≤ FULL, so the product fits u32.
            let share = (s as u32 * moved as u32 / m as u32) as i32;
            let followed = self.deposit(n, id, share, fx);
            self.put_layer(cell, id, s - followed, fx);
        }
    }

    /// Bumps revisions for what `fx` changed and marks active sets: a cell
    /// holding moisture, water or magma joins the water sweep, a cell
    /// holding gas `g` joins gas sweep `g`.
    pub(crate) fn finish_effects(&mut self, fx: &Effects) {
        for pos in fx.cells.keys() {
            let chunk = self.chunks.get_mut(pos).expect("changed chunk is resident");
            chunk.revision += 1;
            chunk.modified = true;
        }
        let eco: BTreeSet<ChunkPos> = fx.layers.keys().chain(fx.plants.keys()).copied().collect();
        for pos in &eco {
            let chunk = self.chunks.get_mut(pos).expect("changed chunk is resident");
            chunk.eco_revision += 1;
        }
        for &cell in &fx.touched {
            let wet = self.layer(cell, LayerId::Moisture) > 0
                || self
                    .get_cell(cell)
                    .is_some_and(|id| matches!(self.class(id), Class::Water | Class::Magma));
            let gases: [bool; crate::layers::N_GASES] =
                std::array::from_fn(|g| self.layer(cell, LayerId::Gas(g as u8)) > 0);
            let (pos, local) = cell.split();
            let Some(chunk) = self.chunks.get_mut(&pos) else {
                continue;
            };
            if wet {
                chunk.active[SWEEP_WATER].insert(local);
            }
            for (g, has) in gases.into_iter().enumerate() {
                if has {
                    chunk.active[SWEEP_GAS + g].insert(local);
                }
            }
        }
    }
}

/// Checks a `ChunkDelta::Layers` entry before anything is written.
pub(crate) fn layer_change_ok(index: u16, id: LayerId, v: i32) -> bool {
    usize::from(index) < CHUNK_VOLUME && id.is_valid() && layer_value_fits(id, v)
}
