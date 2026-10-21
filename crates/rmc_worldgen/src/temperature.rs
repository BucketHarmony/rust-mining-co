//! The initial temperature layer (SAD-P2 §8): `profile(y)` below the
//! surface, season 0's `air_temp` at and above it, magma at
//! `eco.magma_init_temp`, heat sources at their clamp. The bottom cell
//! layer therefore starts at `profile(bottom_y)`, the temperature pass's
//! bottom clamp, and a column of uniform conductivity starts at its steady
//! state.

use rmc_eco::EcoTables;
use rmc_eco::temperature::Profile;
use rmc_world::chunk::Chunk;
use rmc_world::coords::{CHUNK_VOLUME, CellPos, ChunkPos, LocalPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;

/// What worldgen needs to write a chunk's temperature layer.
#[derive(Clone, Debug)]
pub struct TemperatureInit {
    profile: Profile,
    /// Season 0's `air_temp`.
    air_temp: i16,
    /// Role `magma`, if the material table has it.
    magma: Option<MaterialId>,
    magma_init_temp: i16,
    /// `heat_source` per material id.
    sources: Vec<Option<i16>>,
}

impl TemperatureInit {
    /// From the eco tables, whose material table must be the one worldgen
    /// generates with.
    pub fn new(tables: &EcoTables) -> Self {
        let seasons = &tables.seasons().seasons;
        Self {
            profile: Profile::new(tables.params()),
            air_temp: seasons.first().map_or(0, |s| s.air_temp),
            magma: tables.role_material("magma"),
            magma_init_temp: tables.params().magma_init_temp,
            sources: tables.materials().iter().map(|d| d.heat_source).collect(),
        }
    }

    /// `profile(y)` for a world whose bottom cell layer is `bottom_y`.
    pub fn profile(&self, bottom_y: i32, y: i32) -> i16 {
        self.profile.at(bottom_y, y)
    }

    /// The initial temperature of a cell of material `m` in a column whose
    /// topmost solid cell is `surface`.
    pub fn at(&self, m: MaterialId, cell: CellPos, surface: i32, bottom_y: i32) -> i16 {
        if let Some(t) = self.sources.get(usize::from(m.0)).copied().flatten() {
            return t;
        }
        if Some(m) == self.magma {
            return self.magma_init_temp;
        }
        if cell.0.y >= surface {
            return self.air_temp;
        }
        self.profile(bottom_y, cell.0.y)
    }

    /// Writes the temperature layer of the generated chunk at `pos`.
    /// `surface(x, z)` is the column's topmost solid cell.
    pub fn fill(
        &self,
        chunk: &mut Chunk,
        pos: ChunkPos,
        bottom_y: i32,
        surface: impl Fn(i32, i32) -> i32,
    ) {
        for i in 0..CHUNK_VOLUME as u16 {
            let l = LocalPos::from_index(i);
            let cell = CellPos::from_chunk_local(pos, l);
            let t = self.at(
                chunk.storage.get(l),
                cell,
                surface(cell.0.x, cell.0.z),
                bottom_y,
            );
            chunk.layers.set_i16(LayerId::Temperature, l, t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rmc_world::coords::CHUNK_SIZE;
    use rmc_world::materials::MaterialTable;

    use crate::{GenParams, Generator};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn setup() -> (Generator, Generator, EcoTables) {
        let m = Arc::new(MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap());
        let tables = EcoTables::load_with_materials(Arc::clone(&m), ASSETS).unwrap();
        let params = GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap();
        let plain = Generator::new(7, &params, &m).unwrap();
        let hot = Generator::new(7, &params, &m)
            .unwrap()
            .with_temperature(TemperatureInit::new(&tables));
        (plain, hot, tables)
    }

    #[test]
    fn sources_then_magma_then_surface_then_profile() {
        // A test core, and magma if the table lacks it.
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<_> = base.iter().cloned().collect();
        let rows = [
            r#"(id: 0, key: "magma", phase: Liquid, tags: [], hardness: 0, diggable: false,
                yield_count: 0, color: (255, 90, 0, 255), emissive: 2.0)"#,
            r#"(id: 0, key: "test_core", phase: Solid, tags: [], hardness: 0, diggable: false,
                yield_count: 0, color: (255, 60, 0, 255), emissive: 2.0,
                heat_source: Some(13000))"#,
        ];
        let missing = rows.iter().filter(|text| {
            let d: rmc_world::materials::MaterialDef = ron::from_str(text).unwrap();
            base.id(&d.key).is_none()
        });
        for text in missing {
            let mut d: rmc_world::materials::MaterialDef = ron::from_str(text).unwrap();
            d.id = MaterialId(defs.len() as u16);
            defs.push(d);
        }
        let m = Arc::new(MaterialTable::from_defs(defs).unwrap());
        let tables = EcoTables::load_with_materials(Arc::clone(&m), ASSETS).unwrap();
        let init = TemperatureInit::new(&tables);
        let p = tables.params();
        let air_temp = tables.seasons().seasons[0].air_temp;
        let (magma, core) = (m.id("magma").unwrap(), m.id("test_core").unwrap());
        let (stone, air) = (m.id("stone").unwrap(), m.id("air").unwrap());
        let (bottom, surface) = (0, 40);
        let deep = CellPos::new(3, 10, 3);
        let high = CellPos::new(3, surface + 5, 3);
        assert_eq!(init.at(core, deep, surface, bottom), 13000);
        assert_eq!(init.at(core, high, surface, bottom), 13000);
        assert_eq!(init.at(magma, deep, surface, bottom), p.magma_init_temp);
        assert_eq!(init.at(magma, high, surface, bottom), p.magma_init_temp);
        assert_eq!(init.at(air, high, surface, bottom), air_temp);
        assert_eq!(
            init.at(stone, CellPos::new(3, surface, 3), surface, bottom),
            air_temp
        );
        assert_eq!(
            init.at(air, deep, surface, bottom),
            init.profile(bottom, 10)
        );
        assert_eq!(
            init.at(stone, deep, surface, bottom),
            init.profile(bottom, 10)
        );
    }

    #[test]
    fn profile_bottom_equals_profile_fn() {
        let (plain, g, tables) = setup();
        let init = TemperatureInit::new(&tables);
        let profile = Profile::new(tables.params());
        let bottom_y = g.bounds().min.0.y;
        let air_temp = tables.seasons().seasons[0].air_temp;
        let pos = ChunkPos::new(2, bottom_y >> 5, 3);
        let chunk = g.generate(pos);
        assert_eq!(chunk.storage, plain.generate(pos).storage, "same cells");
        let mut checked = 0;
        for i in 0..CHUNK_VOLUME as u16 {
            let l = LocalPos::from_index(i);
            let cell = CellPos::from_chunk_local(pos, l);
            let t = chunk.layers.get_i16(LayerId::Temperature, l);
            if cell.0.y == bottom_y {
                assert_eq!(t, profile.at(bottom_y, bottom_y), "{cell:?}");
                assert_eq!(t, init.profile(bottom_y, bottom_y));
                checked += 1;
            } else {
                assert_eq!(t, profile.at(bottom_y, cell.0.y), "{cell:?} is underground");
            }
        }
        assert_eq!(checked, CHUNK_SIZE * CHUNK_SIZE);
        assert_eq!(
            profile.at(bottom_y, bottom_y),
            tables.params().geothermal_base
        );

        // A surface chunk: underground cells on the profile, the surface
        // cell and the sky above at season 0's air temperature.
        let (x, z) = (70, 90);
        let top = g.surface_height(x, z);
        let pos = CellPos::new(x, top, z).chunk();
        let chunk = g.generate(pos);
        for y in pos.origin_cell().0.y..pos.origin_cell().0.y + CHUNK_SIZE {
            let cell = CellPos::new(x, y, z);
            let t = chunk.layers.get_i16(LayerId::Temperature, cell.local());
            let want = if y >= top {
                air_temp
            } else {
                profile.at(bottom_y, y)
            };
            assert_eq!(t, want, "{cell:?}, surface {top}");
        }
        assert!(
            !plain.generate(pos).layers.contains(LayerId::Temperature),
            "without eco tables worldgen writes no layer"
        );
    }
}
