//! The season ledger (SAD-P2 §7). The type lives in `rmc_world::ledger` so
//! that `World::apply` and `World::eco_set` can write displacement terms;
//! `rmc_eco` re-exports it and adds the merge used to fold per-chunk and
//! per-edit ledgers into a season's.

pub use rmc_world::ledger::SeasonLedger;

/// Adds every term of `from` to `into`. Transition counts for a (from, to)
/// pair `into` already lists are added to that entry; new pairs are
/// appended in `from`'s order, so the result depends only on the order of
/// merges.
pub fn merge(into: &mut SeasonLedger, from: &SeasonLedger) {
    fn add<const N: usize>(a: &mut [i64; N], b: &[i64; N]) {
        a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
    }
    into.rain_in += from.rain_in;
    into.steam_vent_out += from.steam_vent_out;
    into.water_edit_in += from.water_edit_in;
    into.water_edit_out += from.water_edit_out;
    into.water_saturation_loss += from.water_saturation_loss;
    into.evap_to_steam += from.evap_to_steam;
    into.boil_to_steam += from.boil_to_steam;
    into.condense_from_steam += from.condense_from_steam;
    add(&mut into.weather_in, &from.weather_in);
    add(&mut into.litter_in, &from.litter_in);
    add(&mut into.uptake_sink, &from.uptake_sink);
    add(&mut into.harvest_out, &from.harvest_out);
    add(&mut into.edit_in, &from.edit_in);
    add(&mut into.edit_out, &from.edit_out);
    add(&mut into.react_delta, &from.react_delta);
    add(&mut into.saturation_loss, &from.saturation_loss);
    add(&mut into.emit_in, &from.emit_in);
    add(&mut into.vent_out, &from.vent_out);
    add(&mut into.react_out, &from.react_out);
    add(&mut into.react_in, &from.react_in);
    add(&mut into.gas_edit_in, &from.gas_edit_in);
    add(&mut into.gas_edit_out, &from.gas_edit_out);
    add(&mut into.gas_saturation_loss, &from.gas_saturation_loss);
    into.magma_set += from.magma_set;
    into.magma_melt_in += from.magma_melt_in;
    into.magma_edit += from.magma_edit;
    for &(a, b, n) in &from.transitions {
        match into
            .transitions
            .iter_mut()
            .find(|(x, y, _)| (*x, *y) == (a, b))
        {
            Some(entry) => entry.2 += n,
            None => into.transitions.push((a, b, n)),
        }
    }
    into.weathered_cells += from.weathered_cells;
    into.plants_born += from.plants_born;
    into.plants_died += from.plants_died;
    into.ignitions += from.ignitions;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmc_world::materials::MaterialId;

    #[test]
    fn merge_adds_every_term_and_combines_transition_pairs() {
        let mut a = SeasonLedger {
            rain_in: 3,
            transitions: vec![(MaterialId(1), MaterialId(2), 4)],
            ..Default::default()
        };
        a.saturation_loss[2] = 5;
        let mut b = SeasonLedger {
            rain_in: 7,
            magma_set: 1,
            weathered_cells: 2,
            transitions: vec![
                (MaterialId(3), MaterialId(1), 1),
                (MaterialId(1), MaterialId(2), 6),
            ],
            ..Default::default()
        };
        b.saturation_loss[2] = 1;
        b.gas_saturation_loss[0] = 9;
        merge(&mut a, &b);
        assert_eq!(a.rain_in, 10);
        assert_eq!(a.magma_set, 1);
        assert_eq!(a.weathered_cells, 2);
        assert_eq!(a.saturation_loss[2], 6);
        assert_eq!(a.gas_saturation_loss[0], 9);
        assert_eq!(
            a.transitions,
            vec![
                (MaterialId(1), MaterialId(2), 10),
                (MaterialId(3), MaterialId(1), 1)
            ]
        );
    }
}
