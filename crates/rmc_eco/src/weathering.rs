//! Weathering (SAD-P2 §7.5).
//!
//! **Freeze-thaw bit.** `Chunk.freeze_thaw` is cleared at the start of each
//! season ([`season_tick`]). In each sub-step the temperature pass sets a
//! cell's bit when [`crossed_zero`] holds for the cell's temperature before
//! the pass (before the clamps) and after it. The bit stays set for the
//! rest of the season; the weathering pass doubles its rate where it is
//! set.
//!
//! [`season_tick`]: crate::tick::season_tick

/// Whether a temperature went from one side of 0 °C to the other:
/// `(T_before < 0) != (T_after < 0)`.
pub fn crossed_zero(before: i32, after: i32) -> bool {
    (before < 0) != (after < 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, CellPos, LocalPos};
    use rmc_world::layers::LayerId;
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::materials::MaterialId;
    use rmc_world::world::World;

    use crate::activity::Activity;
    use crate::runner::{Exec, run_jacobi};
    use crate::tables::EcoTables;
    use crate::temperature::TemperaturePass;
    use crate::tick::season_tick;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// The `winter` row of `seasons.ron`, whose `air_temp` is below 0.
    const WINTER: u32 = 3;

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    /// Deterministic per-cell noise.
    fn noise(cell: CellPos, salt: u64) -> u64 {
        let c = cell.0;
        let mut h = salt ^ 0x9E37_79B9_7F4A_7C15;
        for v in [c.x, c.y, c.z] {
            h = (h ^ (v as u32 as u64)).wrapping_mul(0x100_0000_01B3);
            h ^= h >> 29;
        }
        h
    }

    /// Two chunks side by side; `cell` gives each in-bounds cell's material
    /// and temperature.
    fn world(t: &EcoTables, cell: impl Fn(CellPos) -> (MaterialId, i16)) -> World {
        let bounds = CellAabb::new(CellPos::new(-8, 0, 0), CellPos::new(7, 15, 7));
        let mut w = World::new(t.materials().clone(), bounds);
        w.set_eco_rules(t.displacement_rules());
        for pos in bounds.chunks() {
            let mut c = Chunk::uniform(MaterialId::AIR);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let at = CellPos::from_chunk_local(pos, l);
                if bounds.contains(at) {
                    let (m, temp) = cell(at);
                    c.storage.set(l, m);
                    c.layers.set_i16(LayerId::Temperature, l, temp);
                }
            }
            w.insert_chunk(pos, c).unwrap();
        }
        w
    }

    /// Mixed ground under an uneven surface, air above it, temperatures
    /// scattered around 0 so that many cells cross it in one pass.
    fn near_zero(t: &EcoTables, seed: u64) -> World {
        let id = |k: &str| t.materials().id(k).unwrap();
        let ground = [id("topsoil"), id("dirt"), id("stone"), id("water")];
        world(t, |c| {
            let h = noise(c, seed);
            let surface = 6 + (noise(CellPos::new(c.0.x, 0, c.0.z), seed) % 5) as i32;
            let m = if c.0.y <= surface {
                ground[(h % ground.len() as u64) as usize]
            } else {
                MaterialId::AIR
            };
            (m, ((h >> 16) % 301) as i16 - 150)
        })
    }

    fn cells(w: &World) -> Vec<CellPos> {
        let b = w.bounds();
        let mut v = Vec::new();
        for y in b.min.0.y..=b.max.0.y {
            for z in b.min.0.z..=b.max.0.z {
                for x in b.min.0.x..=b.max.0.x {
                    v.push(CellPos::new(x, y, z));
                }
            }
        }
        v
    }

    fn temp(w: &World, c: CellPos) -> i32 {
        w.layer(c, LayerId::Temperature)
    }

    fn bit(w: &World, c: CellPos) -> bool {
        let (pos, local) = c.split();
        w.chunk(pos).unwrap().freeze_thaw.contains(local)
    }

    fn bits(w: &World) -> usize {
        w.iter_chunks_sorted()
            .map(|(_, c)| c.freeze_thaw.len())
            .sum()
    }

    #[test]
    fn freeze_thaw_bit_set_on_zero_crossing() {
        assert!(crossed_zero(0, -1) && crossed_zero(-1, 0));
        assert!(!crossed_zero(0, 5) && !crossed_zero(-5, -1));

        let t = tables();
        let pass = TemperaturePass::new(&t, WINTER);
        assert!(pass.air_temp < 0, "the winter row freezes the surface");
        for seed in 0..4 {
            let mut w = near_zero(&t, seed);
            // A sky-open air cell stored above 0: its clamp to the winter
            // air_temp is a crossing, measured from the stored value.
            let sky = CellPos::new(3, 15, 3);
            w.eco_put_layer(sky, LayerId::Temperature, 50);
            // One already below 0 is clamped without crossing.
            let cold_sky = CellPos::new(-3, 15, 3);
            w.eco_put_layer(cold_sky, LayerId::Temperature, -50);
            let all = cells(&w);
            let mut serial = w.clone();
            let mut crossed = vec![false; all.len()];
            let (mut down, mut up, mut kept) = (0, 0, 0);
            for step in 0..3 {
                let before: Vec<i32> = all.iter().map(|&c| temp(&w, c)).collect();
                let mut ledger = SeasonLedger::default();
                run_jacobi(&mut w, &pass, Exec::Parallel, &mut ledger);
                run_jacobi(&mut serial, &pass, Exec::Serial, &mut ledger);
                for (i, &c) in all.iter().enumerate() {
                    let (b, a) = (before[i], temp(&w, c));
                    if crossed_zero(b, a) {
                        crossed[i] = true;
                        if a < 0 { down += 1 } else { up += 1 }
                    } else if b != a {
                        kept += 1;
                    }
                    // Set on a crossing in this pass, kept from earlier ones.
                    assert_eq!(
                        bit(&w, c),
                        crossed[i],
                        "seed {seed} step {step} {c:?}: {b} -> {a}"
                    );
                    assert_eq!(bit(&serial, c), bit(&w, c), "serial vs parallel");
                }
                if step == 0 {
                    assert_eq!(temp(&w, sky), i32::from(pass.air_temp));
                    assert!(bit(&w, sky), "seed {seed}: clamp crossing");
                    assert_eq!(temp(&w, cold_sky), i32::from(pass.air_temp));
                    assert!(!bit(&w, cold_sky), "seed {seed}: no crossing");
                }
            }
            assert!(down > 0 && up > 0 && kept > 0, "{down} {up} {kept}");
            // The bit is not part of the hash.
            assert_eq!(w.hash(), serial.hash());
        }
    }

    #[test]
    fn freeze_thaw_bit_cleared_at_season_start() {
        let t = tables();
        let stone = t.materials().id("stone").unwrap();
        let topsoil = t.materials().id("topsoil").unwrap();

        // A quiet world: all stone at the bottom clamp's temperature, so no
        // pass changes anything. Bits left from an earlier season go.
        let bottom = t.params().geothermal_base;
        let mut w = world(&t, |_| (stone, bottom));
        let marked = [
            CellPos::new(-8, 0, 0),
            CellPos::new(-1, 7, 3),
            CellPos::new(0, 7, 3),
            CellPos::new(7, 15, 7),
        ];
        for &c in &marked {
            assert!(w.eco_mark_freeze_thaw(c));
        }
        assert_eq!(bits(&w), marked.len());
        let hash = w.hash();
        let mut activity = Activity::new(t.params().quiet_skip);
        season_tick(&mut w, &t, WINTER, &mut activity, Exec::Parallel);
        assert_eq!(w.hash(), hash, "nothing to compute");
        assert_eq!(bits(&w), 0);

        // Topsoil to y = 7 at 2 °C, air above it stored at 5 °C. The first
        // winter freezes the air clamps and the top of the ground; their
        // bits stay set to the end of the season.
        let mut w = world(&t, |c| {
            if c.0.y <= 7 {
                (topsoil, 20)
            } else {
                (MaterialId::AIR, 50)
            }
        });
        let air: Vec<CellPos> = cells(&w).into_iter().filter(|c| c.0.y > 7).collect();
        let mut activity = Activity::new(t.params().quiet_skip);
        season_tick(&mut w, &t, WINTER, &mut activity, Exec::Parallel);
        for &c in &air {
            assert!(temp(&w, c) < 0);
            assert!(bit(&w, c), "{c:?}: the crossing's bit lasts the season");
        }
        // The next winter (row 3 again) starts with every bit clear. The air
        // stays at its clamp, so it does not cross again and ends the season
        // without a bit.
        season_tick(&mut w, &t, WINTER + 4, &mut activity, Exec::Parallel);
        for &c in &air {
            assert!(!bit(&w, c), "{c:?}: an earlier season's bit is cleared");
        }
    }
}
