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
    use rmc_world::materials::{MaterialDef, MaterialTable};
    use rmc_world::world::World;

    use crate::invariants::InvariantChecker;
    use crate::runner::{Exec, activate_holders, run_jacobi, run_sweep};
    use crate::tables::EcoSources;
    use crate::temperature::{Profile, TemperaturePass};
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

    /// SAD-P2 §5's magma, magma_source and obsidian rows, for material
    /// tables that lack them (F133 ships them). Magma's `heat_capacity` is
    /// 64, not §5's 32: at 32 the tip cells of every worldgen chamber (5
    /// stone faces, 1 magma face) cool below 900.0 °C by sub-step 19 and
    /// the chamber does not stay molten (see PROGRESS.md, F131).
    const MAGMA_ROWS: [&str; 3] = [
        r#"(id: 0, key: "magma", phase: Liquid, tags: [], hardness: 0, diggable: false,
            yield_count: 0, color: (255, 90, 0, 255), emissive: 2.0,
            viscosity: 3, conductivity: 64, heat_capacity: 64)"#,
        r#"(id: 0, key: "magma_source", phase: Solid, tags: [], hardness: 0,
            diggable: false, yield_count: 0, color: (255, 60, 0, 255), emissive: 2.0,
            heat_source: Some(13000))"#,
        r#"(id: 0, key: "obsidian", phase: Solid, tags: [], hardness: 70,
            diggable: true, yield_count: 1, color: (30, 20, 40, 255), emissive: 0.0)"#,
    ];

    /// The shipped tables, with the magma rows appended where the material
    /// table lacks them.
    fn magma_tables() -> EcoTables {
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for text in MAGMA_ROWS {
            let mut d: MaterialDef = ron::from_str(text).unwrap();
            if base.id(&d.key).is_some() {
                continue;
            }
            d.id = MaterialId(u16::try_from(defs.len()).unwrap());
            defs.push(d);
        }
        let m = Arc::new(MaterialTable::from_defs(defs).unwrap());
        EcoTables::load_with_materials(m, ASSETS).expect("Phase 2 tables load")
    }

    /// One chunk of `fill` at the worldgen profile (SAD-P2 §8, bottom at
    /// y 0), with `cells` holding (material, temperature) on top.
    fn solid_world(t: &EcoTables, fill: MaterialId, cells: &[(CellPos, MaterialId, i16)]) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let profile = Profile::new(t.params());
        let mut c = Chunk::uniform(fill);
        for i in 0..CHUNK_VOLUME as u16 {
            let l = LocalPos::from_index(i);
            let y = CellPos::from_chunk_local(ChunkPos::new(0, 0, 0), l).0.y;
            c.layers.set_i16(LayerId::Temperature, l, profile.at(0, y));
        }
        for &(at, m, temp) in cells {
            let (_, l) = at.split();
            c.storage.set(l, m);
            c.layers.set_i16(LayerId::Temperature, l, temp);
        }
        w.insert_chunk(ChunkPos::new(0, 0, 0), c).unwrap();
        w
    }

    /// Temperature, transitions and the water flow sweeps (where magma
    /// moves and boils) of one sub-step, in SAD-P2 §3 order, with the
    /// invariants checked after each pass.
    fn substep(
        t: &EcoTables,
        w: &mut World,
        checker: &InvariantChecker,
        ledger: &mut SeasonLedger,
    ) {
        run_jacobi(w, &TemperaturePass::new(t, 0), Exec::Parallel, ledger);
        checker
            .check(w, ledger)
            .expect("temperature keeps the invariants");
        run_jacobi(w, &TransitionsPass::new(t), Exec::Parallel, ledger);
        checker
            .check(w, ledger)
            .expect("transitions keep the invariants");
        let mut water = WaterPass::new(t);
        activate_holders(w, &water);
        for i in 0..water.flow_iters() {
            water.set_iter(i);
            run_sweep(w, &mut water, ledger);
        }
        checker
            .check(w, ledger)
            .expect("the water sweep keeps the invariants");
    }

    /// Cells of `m` in the one-chunk world.
    fn count(w: &World, m: MaterialId) -> usize {
        let c = w.chunk(ChunkPos::new(0, 0, 0)).unwrap();
        (0..CHUNK_VOLUME as u16)
            .filter(|&i| c.storage.get(LocalPos::from_index(i)) == m)
            .count()
    }

    /// A worldgen chamber (SAD-P2 §8, `worldgen.ron` `magma_chambers`, no
    /// boundary noise): the cells within `radius` of `center`, magma at
    /// `eco.magma_init_temp` around a core of the cells within 1 of it at
    /// the core's `heat_source`.
    fn chamber(t: &EcoTables, center: CellPos, radius: i32) -> Vec<(CellPos, MaterialId, i16)> {
        let (magma, core) = (id(t, "magma"), id(t, "magma_source"));
        let core_temp = t.materials().get(core).unwrap().heat_source.unwrap();
        let init = t.params().magma_init_temp;
        let mut cells = Vec::new();
        for dy in -radius..=radius {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    let d2 = dx * dx + dy * dy + dz * dz;
                    if d2 > radius * radius {
                        continue;
                    }
                    let at = CellPos::new(center.0.x + dx, center.0.y + dy, center.0.z + dz);
                    cells.push(if d2 <= 1 {
                        (at, core, core_temp)
                    } else {
                        (at, magma, init)
                    });
                }
            }
        }
        cells
    }

    /// `worldgen.ron` `magma_chambers` `radius_min..=radius_max`.
    const CHAMBER_RADII: [i32; 3] = [3, 4, 5];
    /// Sub-steps the stray magma cell gets to set in (SAD-P2 §3: 8 per
    /// season).
    const STRAY_SET_WITHIN: u32 = 64;
    /// Sub-steps the walled-in stone gets to melt in.
    const MELT_WITHIN: u32 = 256;

    #[test]
    fn magma_cools_to_basalt_away_from_source() {
        let t = magma_tables();
        let (magma, basalt, stone) = (id(&t, "magma"), id(&t, "basalt"), id(&t, "stone"));

        // The row: magma below 900.0 °C sets as basalt, keeping its
        // temperature; at 900.0 °C and above it stays molten.
        let mut w = world(
            &t,
            &[
                (at(1), magma, 8999, 0),
                (at(3), magma, 9000, 0),
                (at(5), magma, 12000, 0),
                (at(7), magma, 200, 0),
            ],
        );
        let checker = InvariantChecker::new(&w);
        let ledger = run(&t, &mut w);
        assert_eq!(w.get_cell(at(1)), Some(basalt), "899.9 °C sets");
        assert_eq!(w.get_cell(at(3)), Some(magma), "900.0 °C is not below 900");
        assert_eq!(w.get_cell(at(5)), Some(magma));
        assert_eq!(w.get_cell(at(7)), Some(basalt), "cold magma sets");
        assert_eq!(
            w.layer(at(1), LayerId::Temperature),
            8999,
            "temperature kept"
        );
        assert_eq!(ledger.transitions, vec![(magma, basalt, 2)]);
        assert_eq!(ledger.magma_set, 2, "two cells left the magma count");
        checker.check(&w, &ledger).expect("magma count conserved");

        // In stone: a chamber with its core, and one cell of magma that got
        // away from it. The stray cell cools through the stone and sets;
        // the chamber stays molten.
        let center = CellPos::new(10, 16, 16);
        let stray = CellPos::new(24, 16, 16);
        let mut cells = chamber(&t, center, 4);
        cells.push((stray, magma, t.params().magma_init_temp));
        let mut w = solid_world(&t, stone, &cells);
        let chamber_magma = count(&w, magma) - 1;
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut set_after = None;
        for i in 1..=STRAY_SET_WITHIN {
            substep(&t, &mut w, &checker, &mut ledger);
            if w.get_cell(stray) == Some(basalt) {
                set_after = Some(i);
                break;
            }
        }
        let n = set_after.expect("the stray magma sets within the bound");
        assert!(w.layer(stray, LayerId::Temperature) < 9000);
        assert_eq!(
            count(&w, magma),
            chamber_magma,
            "after {n} sub-steps the chamber is molten"
        );
        assert_eq!(ledger.magma_set, 1, "only the stray cell set");
        assert_eq!(ledger.transitions, vec![(magma, basalt, 1)]);
    }

    #[test]
    fn worldgen_magma_stays_molten_20_substeps() {
        let t = magma_tables();
        let (magma, stone) = (id(&t, "magma"), id(&t, "stone"));
        // Every radius worldgen draws, each chamber alone in its own world.
        for radius in CHAMBER_RADII {
            let cells = chamber(&t, CellPos::new(16, 16, 16), radius);
            let mut w = solid_world(&t, stone, &cells);
            let molten: Vec<CellPos> = cells.iter().filter(|c| c.1 == magma).map(|c| c.0).collect();
            let checker = InvariantChecker::new(&w);
            let mut ledger = SeasonLedger::default();
            for i in 1..=20 {
                substep(&t, &mut w, &checker, &mut ledger);
                for &c in &molten {
                    assert_eq!(
                        w.get_cell(c),
                        Some(magma),
                        "radius {radius}, sub-step {i}: {c:?} set at {}",
                        w.layer(c, LayerId::Temperature)
                    );
                }
            }
            assert_eq!(ledger.magma_set, 0, "radius {radius}");
            assert_eq!(ledger.magma_melt_in, 0, "radius {radius}");
        }
    }

    #[test]
    fn stone_tag_melts_near_source() {
        let t = magma_tables();
        let (magma, core, stone, bedrock) = (
            id(&t, "magma"),
            id(&t, "magma_source"),
            id(&t, "stone"),
            id(&t, "bedrock"),
        );

        // The row: every `stone`-tagged material above 1250.0 °C melts,
        // keeping its temperature; other materials do not.
        let stones: Vec<MaterialId> = t
            .materials()
            .iter()
            .filter(|d| d.has_tag("stone"))
            .map(|d| d.id)
            .collect();
        assert!(stones.len() >= 3, "stone, granite, basalt, limestone");
        let mut cells: Vec<_> = stones
            .iter()
            .enumerate()
            .map(|(i, &m)| (at(2 * i as i32), m, 12501, 0))
            .collect();
        let warm = CellPos::new(2, 8, 4);
        let untagged = CellPos::new(4, 8, 4);
        cells.push((warm, stone, 12500, 0));
        cells.push((untagged, bedrock, 14000, 0));
        let mut w = world(&t, &cells);
        let checker = InvariantChecker::new(&w);
        let ledger = run(&t, &mut w);
        for (i, &m) in stones.iter().enumerate() {
            let c = at(2 * i as i32);
            let key = &t.materials().get(m).unwrap().key;
            assert_eq!(w.get_cell(c), Some(magma), "{key} melts");
            assert_eq!(w.layer(c, LayerId::Temperature), 12501, "temperature kept");
        }
        assert_eq!(w.get_cell(warm), Some(stone), "1250.0 °C is not above 1250");
        assert_eq!(
            w.get_cell(untagged),
            Some(bedrock),
            "only the stone tag melts"
        );
        assert_eq!(ledger.magma_melt_in, stones.len() as i64);
        checker.check(&w, &ledger).expect("magma count conserved");

        // In stone: a cell walled in by cores on all six faces heats past
        // the row and melts; a cell with one core face, and stone two cells
        // off a core, never get there.
        let inner = CellPos::new(8, 16, 16);
        let (beside, off) = (CellPos::new(22, 16, 16), CellPos::new(23, 16, 16));
        let core_temp = t.materials().get(core).unwrap().heat_source.unwrap();
        let mut cells: Vec<_> = [
            [0, -1, 0],
            [0, 0, -1],
            [-1, 0, 0],
            [1, 0, 0],
            [0, 0, 1],
            [0, 1, 0],
        ]
        .iter()
        .map(|d| {
            let c = CellPos::new(inner.0.x + d[0], inner.0.y + d[1], inner.0.z + d[2]);
            (c, core, core_temp)
        })
        .collect();
        cells.push((CellPos::new(21, 16, 16), core, core_temp));
        let mut w = solid_world(&t, stone, &cells);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut melted_after = None;
        for i in 1..=MELT_WITHIN {
            substep(&t, &mut w, &checker, &mut ledger);
            assert_eq!(
                w.get_cell(beside),
                Some(stone),
                "sub-step {i}: one core face is not enough"
            );
            assert_eq!(w.get_cell(off), Some(stone), "sub-step {i}");
            if melted_after.is_none() && w.get_cell(inner) == Some(magma) {
                melted_after = Some(i);
            }
        }
        assert!(
            melted_after.is_some(),
            "the walled-in stone melts within {MELT_WITHIN} sub-steps"
        );
        assert_eq!(ledger.magma_melt_in, 1);
        assert_eq!(ledger.transitions, vec![(stone, magma, 1)]);
        assert_eq!(w.get_cell(inner), Some(magma), "the cores keep it molten");
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
