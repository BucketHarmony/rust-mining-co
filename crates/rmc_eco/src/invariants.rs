//! The conservation invariants of SAD-P2 §7, checked after every pass of
//! every sub-step in tests.
//!
//! - Water: `Σ moisture + Σ steam = before + rain_in + water_edit_in −
//!   water_edit_out − steam_vent_out − water_saturation_loss`.
//! - Substance x: `Σ layer_x + Σ held_x = before + weather_in + litter_in +
//!   edit_in − edit_out − uptake_sink − harvest_out + react_delta −
//!   saturation_loss`.
//! - Gas g: `Σ c_g = before + emit_in + react_in + gas_edit_in − vent_out −
//!   react_out − gas_edit_out − gas_saturation_loss`; for steam `emit_in` is
//!   `evap_to_steam + boil_to_steam`, `vent_out` is `steam_vent_out` and
//!   `react_out` includes `condense_from_steam`.
//! - Magma: `count(magma) = before + magma_melt_in + magma_edit − magma_set`.
//!
//! Moisture counts on every cell, whatever its material: a cell that turns
//! to air keeps its moisture (§7.0). The steam gas and magma material come
//! from the world's [`EcoRules`](rmc_world::EcoRules) roles.

use std::fmt;

use rmc_world::chunk::ChunkStorage;
use rmc_world::coords::{CHUNK_VOLUME, LocalPos};
use rmc_world::layers::{LayerData, LayerId, N_GASES, N_SUBSTANCES};
use rmc_world::world::World;

use crate::ledger::SeasonLedger;

/// The conserved quantities of a world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Moisture on every cell plus the steam gas.
    pub water: i64,
    /// Each substance layer plus what plants hold.
    pub substances: [i64; N_SUBSTANCES],
    /// Each gas layer.
    pub gases: [i64; N_GASES],
    /// Cells of the magma role.
    pub magma: i64,
}

impl Totals {
    /// Sums `world`'s resident chunks.
    pub fn measure(world: &World) -> Self {
        let rules = world.eco_rules();
        let mut t = Self::default();
        for (_, chunk) in world.iter_chunks_sorted() {
            t.water += layer_sum(chunk.layers.data(LayerId::Moisture));
            for (x, s) in t.substances.iter_mut().enumerate() {
                *s += layer_sum(chunk.layers.data(LayerId::Substance(x as u8)));
            }
            for (g, s) in t.gases.iter_mut().enumerate() {
                *s += layer_sum(chunk.layers.data(LayerId::Gas(g as u8)));
            }
            for plant in chunk.plants.values() {
                for (s, &held) in t.substances.iter_mut().zip(&plant.held) {
                    *s += i64::from(held);
                }
            }
            if let Some(magma) = rules.magma {
                t.magma += match &chunk.storage {
                    ChunkStorage::Uniform(id) if *id == magma => CHUNK_VOLUME as i64,
                    ChunkStorage::Uniform(_) => 0,
                    storage => (0..CHUNK_VOLUME as u16)
                        .filter(|&i| storage.get(LocalPos::from_index(i)) == magma)
                        .count() as i64,
                };
            }
        }
        if let Some(steam) = rules.steam {
            t.water += t.gases[usize::from(steam)];
        }
        t
    }

    /// What the totals must be once `ledger`'s terms apply to `self`.
    /// `steam` is the steam gas index, if the world has one.
    pub fn expected(&self, ledger: &SeasonLedger, steam: Option<u8>) -> Self {
        let l = ledger;
        let mut t = *self;
        t.water += l.rain_in + l.water_edit_in
            - l.water_edit_out
            - l.steam_vent_out
            - l.water_saturation_loss;
        for (x, s) in t.substances.iter_mut().enumerate() {
            *s += l.weather_in[x] + l.litter_in[x] + l.edit_in[x]
                - l.edit_out[x]
                - l.uptake_sink[x]
                - l.harvest_out[x]
                + l.react_delta[x]
                - l.saturation_loss[x];
        }
        for (g, s) in t.gases.iter_mut().enumerate() {
            let (emit, vent, react_out) = if steam.is_some_and(|st| usize::from(st) == g) {
                (
                    l.evap_to_steam + l.boil_to_steam,
                    l.steam_vent_out,
                    l.react_out[g] + l.condense_from_steam,
                )
            } else {
                (l.emit_in[g], l.vent_out[g], l.react_out[g])
            };
            *s += emit + l.react_in[g] + l.gas_edit_in[g]
                - vent
                - react_out
                - l.gas_edit_out[g]
                - l.gas_saturation_loss[g];
        }
        t.magma += l.magma_melt_in + l.magma_edit - l.magma_set;
        t
    }
}

/// Sum of a `u8` layer over a chunk; 0 when absent.
fn layer_sum(data: Option<&LayerData>) -> i64 {
    match data {
        Some(LayerData::U8(v)) => v.iter().map(|&x| i64::from(x)).sum(),
        Some(LayerData::I16(v)) => v.iter().map(|&x| i64::from(x)).sum(),
        None => 0,
    }
}

/// One conserved quantity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantity {
    Water,
    Substance(u8),
    Gas(u8),
    Magma,
}

/// A quantity whose measured total differs from what the ledger explains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation {
    pub quantity: Quantity,
    pub expected: i64,
    pub found: i64,
}

/// Every violated invariant, in [`Quantity`] order (water, substances,
/// gases, magma).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violations(pub Vec<Violation>);

impl fmt::Display for Violations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, v) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(
                f,
                "{:?}: expected {}, found {} ({:+})",
                v.quantity,
                v.expected,
                v.found,
                v.found - v.expected
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for Violations {}

/// Holds the totals a season (or a test) started from and checks a world
/// against them and the ledger accumulated since.
#[derive(Clone, Debug)]
pub struct InvariantChecker {
    before: Totals,
}

impl InvariantChecker {
    /// Records `world`'s totals as the starting point.
    pub fn new(world: &World) -> Self {
        Self {
            before: Totals::measure(world),
        }
    }

    /// The starting totals.
    pub fn before(&self) -> &Totals {
        &self.before
    }

    /// Checks every invariant of `world` against the starting totals and
    /// `ledger`, the sum of every term since the start.
    pub fn check(&self, world: &World, ledger: &SeasonLedger) -> Result<(), Violations> {
        let want = self.before.expected(ledger, world.eco_rules().steam);
        let have = Totals::measure(world);
        let mut pairs = vec![(Quantity::Water, want.water, have.water)];
        for x in 0..N_SUBSTANCES {
            pairs.push((
                Quantity::Substance(x as u8),
                want.substances[x],
                have.substances[x],
            ));
        }
        for g in 0..N_GASES {
            pairs.push((Quantity::Gas(g as u8), want.gases[g], have.gases[g]));
        }
        pairs.push((Quantity::Magma, want.magma, have.magma));
        let violations: Vec<Violation> = pairs
            .into_iter()
            .filter(|&(_, expected, found)| expected != found)
            .map(|(quantity, expected, found)| Violation {
                quantity,
                expected,
                found,
            })
            .collect();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(Violations(violations))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    use rmc_world::coords::{CellAabb, CellPos};
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::world::World;

    use crate::EcoTables;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    fn id(t: &MaterialTable, key: &str) -> MaterialId {
        t.id(key).unwrap_or_else(|| panic!("material {key}"))
    }

    /// Two chunks side by side: stone below y 8, air above, eco rules from
    /// the tables.
    fn world() -> (World, EcoTables) {
        let t = tables();
        let m: Arc<MaterialTable> = t.materials().clone();
        let mut w = World::new(
            m.clone(),
            CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31)),
        );
        w.set_eco_rules(t.displacement_rules());
        let stone = id(&m, "stone");
        let air = id(&m, "air");
        for cx in 0..2 {
            let mut chunk = rmc_world::chunk::Chunk::uniform(air);
            for i in 0..CHUNK_VOLUME {
                let l = LocalPos::from_index(i as u16);
                if l.0.y < 8 {
                    chunk.storage.set(l, stone);
                }
            }
            w.insert_chunk(rmc_world::coords::ChunkPos::new(cx, 0, 0), chunk)
                .unwrap();
        }
        assert!(Path::new(ASSETS).exists());
        (w, t)
    }

    #[test]
    fn checker_detects_created_water() {
        let (mut w, t) = world();
        let steam = t.role_gas("steam").expect("steam role");
        let wet = CellPos::new(40, 9, 3);
        w.eco_put_layer(wet, LayerId::Moisture, 30);
        w.eco_put_layer(CellPos::new(2, 12, 2), LayerId::Gas(steam), 5);
        let checker = InvariantChecker::new(&w);
        assert_eq!(checker.before().water, 35, "moisture plus steam");
        assert_eq!(checker.check(&w, &SeasonLedger::default()), Ok(()));

        // Moisture appears with no ledger term: water was created.
        w.eco_put_layer(wet, LayerId::Moisture, 50);
        let err = checker
            .check(&w, &SeasonLedger::default())
            .expect_err("created water is caught");
        assert_eq!(
            err.0,
            vec![Violation {
                quantity: Quantity::Water,
                expected: 35,
                found: 55,
            }]
        );
        // The same amount booked as rain balances.
        let rain = SeasonLedger {
            rain_in: 20,
            ..Default::default()
        };
        assert_eq!(checker.check(&w, &rain), Ok(()));

        // Steam created from nothing breaks both water and the steam gas.
        w.eco_put_layer(CellPos::new(2, 12, 2), LayerId::Gas(steam), 6);
        let err = checker.check(&w, &rain).expect_err("created steam");
        let quantities: Vec<Quantity> = err.0.iter().map(|v| v.quantity).collect();
        assert_eq!(quantities, vec![Quantity::Water, Quantity::Gas(steam)]);
        // Booked as evaporation from moisture it balances only if moisture
        // fell by the same amount.
        let evap = SeasonLedger {
            rain_in: 20,
            evap_to_steam: 1,
            ..Default::default()
        };
        assert_eq!(
            checker.check(&w, &evap).map_err(|e| e.0[0].quantity),
            Err(Quantity::Water)
        );
        w.eco_put_layer(wet, LayerId::Moisture, 49);
        assert_eq!(checker.check(&w, &evap), Ok(()));
    }

    #[test]
    fn checker_counts_substances_held_by_plants_gases_and_magma() {
        let (mut w, t) = world();
        // The magma material arrives with F130; any material can hold the
        // role here.
        let magma = id(t.materials(), "dirt");
        let mut rules = t.displacement_rules();
        rules.magma = Some(magma);
        w.set_eco_rules(rules);
        let firedamp = t.role_gas("firedamp").expect("firedamp role");
        let cell = CellPos::new(5, 3, 5);
        w.eco_put_layer(cell, LayerId::Substance(2), 40);
        let mut plant = rmc_world::layers::PlantState::default();
        plant.held[2] = 7;
        w.eco_put_plant(CellPos::new(5, 8, 5), Some(plant));
        w.eco_put_layer(CellPos::new(6, 20, 6), LayerId::Gas(firedamp), 90);
        let checker = InvariantChecker::new(&w);
        assert_eq!(checker.before().substances[2], 47);
        assert_eq!(checker.before().gases[usize::from(firedamp)], 90);
        assert_eq!(checker.before().magma, 0);

        // A pass melts stone to magma: balanced by eco_set's own term.
        let mut ledger = SeasonLedger::default();
        w.eco_set(CellPos::new(1, 1, 1), magma, &mut ledger);
        assert_eq!(ledger.magma_melt_in, 1, "eco_set books the melt");
        assert_eq!(checker.check(&w, &ledger), Ok(()));
        // Unbooked gas loss and uptake are caught.
        w.eco_put_layer(CellPos::new(6, 20, 6), LayerId::Gas(firedamp), 80);
        w.eco_put_layer(cell, LayerId::Substance(2), 30);
        let err = checker.check(&w, &ledger).expect_err("unbooked changes");
        assert_eq!(
            err.0,
            vec![
                Violation {
                    quantity: Quantity::Substance(2),
                    expected: 47,
                    found: 37,
                },
                Violation {
                    quantity: Quantity::Gas(firedamp),
                    expected: 90,
                    found: 80,
                },
            ]
        );
        ledger.uptake_sink[2] = 10;
        ledger.vent_out[usize::from(firedamp)] = 10;
        assert_eq!(checker.check(&w, &ledger), Ok(()));
    }

    #[test]
    fn expected_applies_steam_terms_to_the_steam_gas() {
        let before = Totals::default();
        let mut l = SeasonLedger {
            evap_to_steam: 4,
            boil_to_steam: 6,
            condense_from_steam: 3,
            steam_vent_out: 2,
            ..Default::default()
        };
        l.emit_in[1] = 9;
        l.vent_out[1] = 1;
        let e = before.expected(&l, Some(0));
        assert_eq!(e.gases[0], 4 + 6 - 3 - 2);
        assert_eq!(e.gases[1], 9 - 1);
        assert_eq!(e.water, -2, "only the vent leaves the water total");
    }
}
