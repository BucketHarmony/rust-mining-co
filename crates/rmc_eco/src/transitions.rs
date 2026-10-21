//! Phase transitions (SAD-P2 §7.2), a Jacobi pass on temperature rules
//! only.
//!
//! Every cell takes the first row of `transitions.ron`, in table order,
//! whose `from` matches its material (by key or by tag) and whose `when`
//! holds for its temperature: `TempBelow(t)` is `T < t`, `TempAbove(t)` is
//! `T > t`. The cell becomes the row's `to` through [`World::eco_set`],
//! which runs the §7.0 displacement rules: water ↔ ice keeps the moisture,
//! water → air boils the fill to steam (`BoilAll`), magma is counted in
//! cells. A cell changes at most once per pass: the pass computes from the
//! previous state, so a new material waits for the next sub-step to match
//! its own rows. The rows read only the cell itself, so a chunk's output
//! does not depend on its border.
//!
//! The pass books one `transitions` ledger entry per (from, to) pair with
//! the number of cells it changed.
//!
//! [`World::eco_set`]: rmc_world::world::World::eco_set

use std::collections::BTreeMap;

use rmc_world::chunk::ChunkStorage;
use rmc_world::coords::LocalPos;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;

use crate::runner::{ChunkView, JacobiOut, JacobiPass};
use crate::tables::{EcoTables, MaterialMatch, TransitionWhen};

/// One resolved row: the condition and the material it leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rule {
    when: TransitionWhen,
    to: MaterialId,
}

impl Rule {
    fn holds(&self, t: i32) -> bool {
        match self.when {
            TransitionWhen::TempBelow(x) => t < i32::from(x),
            TransitionWhen::TempAbove(x) => t > i32::from(x),
        }
    }
}

/// The transitions pass, built once from the tables.
#[derive(Clone, Debug, Default)]
pub struct TransitionsPass {
    /// Per material id: the rows whose `from` matches it, in table order.
    /// Rows whose `from` key or `to` key the material table lacks yet, and
    /// rows that would leave a material unchanged, are dropped.
    rules: Vec<Vec<Rule>>,
}

impl TransitionsPass {
    pub fn new(tables: &EcoTables) -> Self {
        let materials = tables.materials();
        let mut rules = vec![Vec::new(); materials.len()];
        for row in tables.transitions() {
            let Some(to) = materials.id(&row.to) else {
                continue;
            };
            for def in materials.iter() {
                let matches = match &row.from {
                    MaterialMatch::Key(key) => def.key == *key,
                    MaterialMatch::Tag(tag) => def.has_tag(tag),
                };
                if matches && def.id != to {
                    rules[usize::from(def.id.0)].push(Rule { when: row.when, to });
                }
            }
        }
        Self { rules }
    }

    /// The material `from` becomes at temperature `t`, if a row matches.
    pub fn target(&self, from: MaterialId, t: i32) -> Option<MaterialId> {
        self.rules
            .get(usize::from(from.0))?
            .iter()
            .find(|r| r.holds(t))
            .map(|r| r.to)
    }

    /// Whether any row can change `m`.
    fn has_rules(&self, m: MaterialId) -> bool {
        self.rules
            .get(usize::from(m.0))
            .is_some_and(|r| !r.is_empty())
    }
}

impl JacobiPass for TransitionsPass {
    fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
        let mut out = JacobiOut::default();
        let storage = &view.chunk().storage;
        // The palette lists every material the chunk may hold.
        let candidates = match storage {
            ChunkStorage::Uniform(m) => self.has_rules(*m),
            ChunkStorage::Paletted { palette, .. } => palette.iter().any(|&m| self.has_rules(m)),
        };
        if !candidates {
            return out;
        }
        let mut counts: BTreeMap<(MaterialId, MaterialId), u32> = BTreeMap::new();
        for (i, cell) in view.cells() {
            let from = storage.get(LocalPos::from_index(i));
            if !self.has_rules(from) {
                continue;
            }
            let t = view.layer(cell, LayerId::Temperature);
            if let Some(to) = self.target(from, t) {
                out.materials.push((i, to));
                *counts.entry((from, to)).or_default() += 1;
            }
        }
        out.ledger.transitions = counts.into_iter().map(|((a, b), n)| (a, b, n)).collect();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, CellPos, ChunkPos};
    use rmc_world::layers::SWEEP_WATER;
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::world::World;

    use crate::invariants::InvariantChecker;
    use crate::runner::{Exec, activate_holders, run_jacobi, run_sweep};
    use crate::tables::EcoSources;
    use crate::water::WaterPass;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    /// The shipped tables with `transitions.ron` replaced by `rows`.
    fn tables_with(rows: &str) -> EcoTables {
        let shipped = tables();
        let mut src = EcoSources::read(ASSETS).unwrap();
        src.transitions = rows.into();
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src).expect("tables load")
    }

    fn id(t: &EcoTables, key: &str) -> MaterialId {
        t.materials().id(key).unwrap()
    }

    /// One chunk of air with `cells` holding (material, temperature,
    /// moisture). Every other cell is at 0.5 °C, which no shipped row
    /// matches for air.
    fn world(t: &EcoTables, cells: &[(CellPos, MaterialId, i16, u8)]) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut c = Chunk::uniform(MaterialId::AIR);
        for i in 0..CHUNK_VOLUME as u16 {
            c.layers
                .set_i16(LayerId::Temperature, LocalPos::from_index(i), 5);
        }
        for &(at, m, temp, moisture) in cells {
            let (_, l) = at.split();
            c.storage.set(l, m);
            c.layers.set_i16(LayerId::Temperature, l, temp);
            c.layers.set_u8(LayerId::Moisture, l, moisture);
        }
        w.insert_chunk(ChunkPos::new(0, 0, 0), c).unwrap();
        w
    }

    fn run(t: &EcoTables, w: &mut World) -> SeasonLedger {
        let mut ledger = SeasonLedger::default();
        run_jacobi(w, &TransitionsPass::new(t), Exec::Parallel, &mut ledger);
        ledger
    }

    fn at(x: i32) -> CellPos {
        CellPos::new(x, 4, 4)
    }

    #[test]
    fn water_freezes_below_zero() {
        let t = tables();
        let (water, ice) = (id(&t, "water"), id(&t, "ice"));
        let mut w = world(
            &t,
            &[
                (at(1), water, -1, 200),
                (at(3), water, -150, 90),
                (at(5), water, 0, 200),
                (at(7), water, 20, 200),
            ],
        );
        let rev = w.chunk(ChunkPos::new(0, 0, 0)).unwrap().revision;
        let ledger = run(&t, &mut w);
        assert_eq!(w.get_cell(at(1)), Some(ice), "-0.1 °C freezes");
        assert_eq!(w.get_cell(at(3)), Some(ice), "-15 °C freezes");
        assert_eq!(w.get_cell(at(5)), Some(water), "0 °C is not below 0");
        assert_eq!(w.get_cell(at(7)), Some(water), "2 °C stays water");
        assert_eq!(ledger.transitions, vec![(water, ice, 2)]);
        assert!(w.chunk(ChunkPos::new(0, 0, 0)).unwrap().revision > rev);
        // Ice is a solid: it stands on the column's surface.
        assert_eq!(w.surface_y(1, 4), 4);
        // Temperatures are the temperature pass's business, not this one's.
        assert_eq!(w.layer(at(1), LayerId::Temperature), -1);
    }

    #[test]
    fn ice_melts_above_hysteresis() {
        let t = tables();
        let (water, ice) = (id(&t, "water"), id(&t, "ice"));
        let mut w = world(
            &t,
            &[
                (at(1), ice, 0, 200),
                (at(3), ice, 30, 200),
                (at(5), ice, 50, 200),
                (at(7), ice, 51, 200),
                (at(9), ice, 400, 200),
                (at(11), water, 30, 200),
            ],
        );
        let ledger = run(&t, &mut w);
        for x in [1, 3, 5] {
            assert_eq!(w.get_cell(at(x)), Some(ice), "x {x}: ≤ 5.0 °C stays ice");
        }
        assert_eq!(w.get_cell(at(7)), Some(water), "5.1 °C melts");
        assert_eq!(w.get_cell(at(9)), Some(water), "40 °C melts");
        assert_eq!(
            w.get_cell(at(11)),
            Some(water),
            "3 °C water does not freeze"
        );
        assert_eq!(ledger.transitions, vec![(ice, water, 2)]);

        // Between 0 and 5 °C both phases hold: repeated passes change
        // nothing.
        let hash = w.hash();
        let ledger = run(&t, &mut w);
        assert_eq!(w.hash(), hash);
        assert!(ledger.transitions.is_empty());
    }

    #[test]
    fn moisture_preserved_across_freeze() {
        let t = tables();
        let (water, ice) = (id(&t, "water"), id(&t, "ice"));
        // A frozen pond cell on a stone floor, with water on top of it.
        let stone = id(&t, "stone");
        let pond = at(4);
        let above = CellPos::new(4, 5, 4);
        let mut w = world(
            &t,
            &[
                (CellPos::new(4, 3, 4), stone, -50, 0),
                (pond, water, -50, 173),
                (above, water, 20, 40),
            ],
        );
        activate_holders(&mut w, &WaterPass::new(&t));
        let checker = InvariantChecker::new(&w);
        let mut ledger = run(&t, &mut w);
        assert_eq!(w.get_cell(pond), Some(ice));
        assert_eq!(w.layer(pond, LayerId::Moisture), 173, "ice keeps the fill");
        assert_eq!(ledger.saturation_loss, [0; rmc_world::layers::N_SUBSTANCES]);
        checker
            .check(&w, &ledger)
            .expect("water conserved by freezing");

        // The water sweep moves nothing out of ice and nothing into it.
        let mut water_pass = WaterPass::new(&t);
        for _ in 0..water_pass.flow_iters() {
            run_sweep(&mut w, &mut water_pass, &mut ledger);
        }
        // The water above spread out over the ice, none of it into it.
        assert_eq!(w.layer(pond, LayerId::Moisture), 173);
        assert!(
            w.layer(above, LayerId::Moisture) < 40,
            "the water above flowed"
        );
        let (pos, local) = pond.split();
        assert!(
            !w.chunk(pos).unwrap().active[SWEEP_WATER].contains(local),
            "ice leaves the water sweep"
        );
        checker.check(&w, &ledger).expect("water conserved");

        // Thawing gives the same fill back as water, which flows again.
        w.eco_put_layer(pond, LayerId::Temperature, 60);
        let thaw = run(&t, &mut w);
        crate::ledger::merge(&mut ledger, &thaw);
        assert_eq!(w.get_cell(pond), Some(water));
        assert_eq!(w.layer(pond, LayerId::Moisture), 173);
        assert!(
            w.chunk(pos).unwrap().active[SWEEP_WATER].contains(local),
            "melted water joins the water sweep"
        );
        checker
            .check(&w, &ledger)
            .expect("water conserved by thawing");
    }

    #[test]
    fn table_order_precedence() {
        let water_first = r#"[
            (from: Key("water"), to: "ice",    when: TempBelow(0)),
            (from: Key("water"), to: "basalt", when: TempBelow(-100)),
            (from: Key("ice"),   to: "stone",  when: TempBelow(0)),
        ]"#;
        let basalt_first = r#"[
            (from: Key("water"), to: "basalt", when: TempBelow(-100)),
            (from: Key("water"), to: "ice",    when: TempBelow(0)),
        ]"#;
        let tag_first = r#"[
            (from: Tag("stone"), to: "dirt",  when: TempAbove(100)),
            (from: Key("granite"), to: "clay", when: TempAbove(100)),
        ]"#;
        let key_first = r#"[
            (from: Key("granite"), to: "clay", when: TempAbove(100)),
            (from: Tag("stone"), to: "dirt",  when: TempAbove(100)),
        ]"#;

        let cell = at(2);
        let outcome = |rows: &str, from: &str, temp: i16| {
            let t = tables_with(rows);
            let mut w = world(&t, &[(cell, id(&t, from), temp, 0)]);
            run(&t, &mut w);
            let m = w.get_cell(cell).unwrap();
            t.materials().get(m).unwrap().key.clone()
        };
        // Both water rows match at -20 °C; the first one wins. Only one
        // change per pass: the new ice does not go on to stone.
        assert_eq!(outcome(water_first, "water", -200), "ice");
        assert_eq!(outcome(basalt_first, "water", -200), "basalt");
        // Only the second row matches at -5 °C.
        assert_eq!(outcome(basalt_first, "water", -50), "ice");
        // A tag row and a key row match the same cell: table order decides.
        assert_eq!(outcome(tag_first, "granite", 200), "dirt");
        assert_eq!(outcome(key_first, "granite", 200), "clay");
        assert_eq!(outcome(key_first, "stone", 200), "dirt");
        assert_eq!(outcome(key_first, "granite", 100), "granite");

        // The ice from the first pass goes on to stone in the next.
        let t = tables_with(water_first);
        let mut w = world(&t, &[(cell, id(&t, "water"), -200, 0)]);
        run(&t, &mut w);
        run(&t, &mut w);
        assert_eq!(w.get_cell(cell), Some(id(&t, "stone")));
    }

    #[test]
    fn shipped_rows_leave_temperate_worlds_alone() {
        // No shipped row matches air, soil or stone at everyday
        // temperatures, so a world without water changes nothing.
        let t = tables();
        let cells: Vec<_> = ["topsoil", "dirt", "stone", "granite", "basalt", "bedrock"]
            .iter()
            .enumerate()
            .map(|(i, k)| (at(2 * i as i32), id(&t, k), 300, 0))
            .collect();
        let mut w = world(&t, &cells);
        let hash = w.hash();
        let ledger = run(&t, &mut w);
        assert_eq!(w.hash(), hash);
        assert_eq!(ledger, SeasonLedger::default());
    }
}
