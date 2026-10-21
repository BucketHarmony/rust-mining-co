//! Derived ecology readings (SAD-P2 §3, §7.6): pH and "spoil". Neither is
//! stored; both are computed from a cell's substance layers when asked.
//!
//! - pH: `clamp(7 + (lime − acid) / 2^ph_shift, 0, 14)` ([`ph`]). The
//!   season tick, the HUD and the editor readout all read it through here.
//! - Spoil: a cell is spoil for a [`ToxProfile`] when any substance with a
//!   limit in the profile holds at least that limit (`substances.ron`
//!   `tox_plant` / `tox_player`: "amount at or above which the cell is
//!   spoil"). A substance without a limit never makes a cell spoil.
//!   [`spoil`] lists the spoil cells of a region; the HUD asks
//!   [`ToxProfile::is_spoil`] of one cell.

use crate::chunk::Chunk;
use crate::coords::{CellAabb, CellPos, LocalPos};
use crate::layers::{LayerId, N_SUBSTANCES};
use crate::world::World;

/// The neutral pH, the `7` of `pH = clamp(7 + (lime − acid) / 2^ph_shift,
/// 0, 14)` (SAD-P2 §7.6).
pub const PH_NEUTRAL: i32 = 7;
/// Upper end of the derived pH scale (SAD-P2 §7.6 clamps to `0..=14`).
pub const PH_MAX: u8 = 14;

/// The derived pH of a cell holding `lime` and `acid` (SAD-P2 §7.6):
/// `clamp(7 + (lime − acid) / 2^ph_shift, 0, 14)`, dividing with
/// truncation toward zero.
pub fn ph(lime: i32, acid: i32, ph_shift: u8) -> i32 {
    (PH_NEUTRAL + (lime - acid) / (1 << ph_shift)).clamp(0, i32::from(PH_MAX))
}

/// Which substance layers pH reads, and its shift: the `acid` and `lime`
/// substance roles and `eco.ph_shift`. A role the table lacks reads 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhRule {
    pub acid: Option<LayerId>,
    pub lime: Option<LayerId>,
    pub ph_shift: u8,
}

impl PhRule {
    /// The pH of `cell`; cells outside resident chunks hold nothing and read
    /// neutral.
    pub fn at(&self, world: &World, cell: CellPos) -> i32 {
        let read = |id: Option<LayerId>| id.map_or(0, |id| world.layer(cell, id));
        ph(read(self.lime), read(self.acid), self.ph_shift)
    }
}

/// Per substance layer, the amount at or above which a cell is spoil for
/// someone (SAD-P2 §7.6: `tox_plant` makes the plant profile, `tox_player`
/// the player's). The default profile has no limits and finds no spoil.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ToxProfile {
    limits: [Option<u8>; N_SUBSTANCES],
}

impl ToxProfile {
    /// From the limit of each substance layer, in layer order.
    pub fn new(limits: [Option<u8>; N_SUBSTANCES]) -> Self {
        Self { limits }
    }

    /// The limit of substance layer `id`; `None` for a substance without one
    /// and for every other layer.
    pub fn limit(&self, id: LayerId) -> Option<u8> {
        match id {
            LayerId::Substance(i) => self.limits.get(usize::from(i)).copied().flatten(),
            _ => None,
        }
    }

    /// Whether no substance has a limit.
    pub fn is_empty(&self) -> bool {
        self.limits.iter().all(Option::is_none)
    }

    /// Every `(layer, limit)` with a limit, in layer order.
    pub fn limits(&self) -> impl Iterator<Item = (LayerId, u8)> + '_ {
        (0u8..)
            .zip(&self.limits)
            .filter_map(|(i, l)| l.map(|l| (LayerId::Substance(i), l)))
    }

    /// The substance layers of `cell` that reach their limit, in layer
    /// order. Empty outside resident chunks.
    pub fn exceeded(&self, world: &World, cell: CellPos) -> Vec<LayerId> {
        if !world.bounds().contains(cell) {
            return Vec::new();
        }
        let (pos, local) = cell.split();
        world.chunk(pos).map_or_else(Vec::new, |chunk| {
            self.limits()
                .filter(|&(id, limit)| chunk.layers.get_u8(id, local) >= limit)
                .map(|(id, _)| id)
                .collect()
        })
    }

    /// Whether `cell` is spoil for this profile. Cells outside resident
    /// chunks are not.
    pub fn is_spoil(&self, world: &World, cell: CellPos) -> bool {
        if !world.bounds().contains(cell) {
            return false;
        }
        let (pos, local) = cell.split();
        world
            .chunk(pos)
            .is_some_and(|chunk| self.chunk_cell_is_spoil(chunk, local))
    }

    fn chunk_cell_is_spoil(&self, chunk: &Chunk, local: LocalPos) -> bool {
        self.limits()
            .any(|(id, limit)| chunk.layers.get_u8(id, local) >= limit)
    }
}

/// The spoil cells of `region` for `profile` (SAD-P2 §3 `query::spoil`):
/// every cell of a resident chunk inside the region and the world's bounds
/// where some substance reaches its limit, sorted by `(y, z, x)`.
pub fn spoil(world: &World, region: CellAabb, profile: &ToxProfile) -> Vec<CellPos> {
    let bounds = world.bounds();
    let lo = region.min.0.max(bounds.min.0);
    let hi = region.max.0.min(bounds.max.0);
    if profile.is_empty() || lo.cmpgt(hi).any() {
        return Vec::new();
    }
    let region = CellAabb::new(CellPos(lo), CellPos(hi));
    let mut out = Vec::new();
    for pos in region.chunks() {
        let Some(chunk) = world.chunk(pos) else {
            continue;
        };
        // A chunk with none of the limited layers has no spoil.
        if !profile.limits().any(|(id, _)| chunk.layers.contains(id)) {
            continue;
        }
        let origin = pos.origin_cell().0;
        let clo = lo.max(origin) - origin;
        let chi = hi.min(origin + crate::coords::CHUNK_MASK) - origin;
        for y in clo.y..=chi.y {
            for z in clo.z..=chi.z {
                for x in clo.x..=chi.x {
                    let local = LocalPos::new(x as u32, y as u32, z as u32);
                    if profile.chunk_cell_is_spoil(chunk, local) {
                        out.push(CellPos::from_chunk_local(pos, local));
                    }
                }
            }
        }
    }
    out.sort_by_key(|c| (c.0.y, c.0.z, c.0.x));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::coords::{CHUNK_SIZE, ChunkPos};
    use crate::materials::{MaterialId, MaterialTable};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// Limits shaped like `substances.ron`'s player profile: layers 0, 3
    /// and 5 have one, the rest none.
    fn player() -> ToxProfile {
        let mut limits = [None; N_SUBSTANCES];
        limits[0] = Some(80);
        limits[3] = Some(128);
        limits[5] = Some(192);
        ToxProfile::new(limits)
    }

    /// Two resident chunks side by side along x, of a 3 × 1 × 1 chunk
    /// world (the third chunk is not resident), all air.
    fn world() -> World {
        let materials = Arc::new(MaterialTable::load(MATERIALS_PATH).expect("materials load"));
        let size = CHUNK_SIZE;
        let bounds = CellAabb::new(
            CellPos::new(0, 0, 0),
            CellPos::new(3 * size - 1, size - 1, size - 1),
        );
        let mut w = World::new(materials, bounds);
        for x in 0..2 {
            w.insert_chunk(ChunkPos::new(x, 0, 0), Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        w
    }

    fn set(w: &mut World, cell: CellPos, id: LayerId, v: u8) {
        let (pos, local) = cell.split();
        let mut chunk = w.chunk(pos).expect("resident").clone();
        chunk.layers.set_u8(id, local, v);
        w.insert_chunk(pos, chunk).unwrap();
    }

    /// Every cell of `region` in `(y, z, x)` order where some limited
    /// substance reaches its limit, by reading each cell on its own.
    fn brute(w: &World, region: CellAabb, p: &ToxProfile) -> Vec<CellPos> {
        let mut out = Vec::new();
        for y in region.min.0.y..=region.max.0.y {
            for z in region.min.0.z..=region.max.0.z {
                for x in region.min.0.x..=region.max.0.x {
                    let c = CellPos::new(x, y, z);
                    let hit = p
                        .limits()
                        .any(|(id, l)| w.get_cell(c).is_some() && w.layer(c, id) >= i32::from(l));
                    assert_eq!(hit, p.is_spoil(w, c), "{c:?}");
                    assert_eq!(hit, !p.exceeded(w, c).is_empty(), "{c:?}");
                    if hit {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn spoil_cells_match_profile() {
        let p = player();
        let mut w = world();
        let all = w.bounds();
        assert!(spoil(&w, all, &p).is_empty(), "clean world has no spoil");

        let cu = LayerId::Substance(0);
        let fe = LayerId::Substance(1);
        let acid = LayerId::Substance(3);
        let salt = LayerId::Substance(5);
        let at_limit = CellPos::new(3, 2, 4);
        let below = CellPos::new(4, 2, 4);
        let unlimited = CellPos::new(5, 2, 4);
        let acid_cell = CellPos::new(31, 9, 0);
        let across = CellPos::new(32, 1, 31);
        let both = CellPos::new(40, 1, 2);
        set(&mut w, at_limit, cu, 80);
        set(&mut w, below, cu, 79);
        set(&mut w, unlimited, fe, 255);
        set(&mut w, acid_cell, acid, 200);
        set(&mut w, across, salt, 192);
        set(&mut w, both, cu, 255);
        set(&mut w, both, acid, 128);
        set(&mut w, CellPos::new(6, 2, 4), salt, 191);

        // At the limit is spoil, one below is not, a substance without a
        // limit never is; sorted by (y, z, x) across both chunks.
        let found = spoil(&w, all, &p);
        assert_eq!(found, vec![both, across, at_limit, acid_cell]);
        assert_eq!(found, brute(&w, all, &p));
        assert_eq!(p.exceeded(&w, both), vec![cu, acid]);
        assert_eq!(p.exceeded(&w, at_limit), vec![cu]);
        assert!(!p.is_spoil(&w, below) && !p.is_spoil(&w, unlimited));

        // Regions clip: one chunk, one cell, a box across the border, a box
        // over the absent chunk and past the bounds.
        let left = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        assert_eq!(spoil(&w, left, &p), vec![at_limit, acid_cell]);
        assert_eq!(spoil(&w, CellAabb::new(below, below), &p), vec![]);
        assert_eq!(
            spoil(&w, CellAabb::new(at_limit, at_limit), &p),
            vec![at_limit]
        );
        let border = CellAabb::new(CellPos::new(30, 0, 0), CellPos::new(33, 31, 31));
        assert_eq!(spoil(&w, border, &p), vec![across, acid_cell]);
        assert_eq!(spoil(&w, border, &p), brute(&w, border, &p));
        let outside = CellAabb::new(CellPos::new(60, -5, -5), CellPos::new(200, 40, 40));
        assert_eq!(spoil(&w, outside, &p), vec![]);
        let wide = CellAabb::new(CellPos::new(-10, -10, -10), CellPos::new(200, 200, 200));
        assert_eq!(spoil(&w, wide, &p), found);

        // A profile with no limits finds nothing; another profile finds by
        // its own limits only.
        assert_eq!(spoil(&w, all, &ToxProfile::default()), vec![]);
        let mut fe_only = [None; N_SUBSTANCES];
        fe_only[1] = Some(255);
        let fe_only = ToxProfile::new(fe_only);
        assert_eq!(spoil(&w, all, &fe_only), vec![unlimited]);
        assert_eq!(fe_only.limit(fe), Some(255));
        assert_eq!(fe_only.limit(cu), None);
        assert_eq!(fe_only.limit(LayerId::Moisture), None);

        // Spoil is never stored: clearing the substance clears the spoil.
        set(&mut w, both, cu, 0);
        set(&mut w, both, acid, 127);
        let found = spoil(&w, all, &p);
        assert_eq!(found, vec![across, at_limit, acid_cell]);
        assert_eq!(found, brute(&w, all, &p));

        // A seeded scatter agrees with the cell-by-cell reading.
        let mut s: u64 = 0x5eed;
        for _ in 0..400 {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = (s >> 33) as i32;
            let c = CellPos::new(r % 64, (r >> 6) % 32, (r >> 11) % 32);
            let id = LayerId::Substance(((r >> 16) % 6) as u8);
            set(&mut w, c, id, ((r >> 19) & 0xff) as u8);
        }
        assert_eq!(spoil(&w, all, &p), brute(&w, all, &p));
        assert_eq!(spoil(&w, border, &p), brute(&w, border, &p));
    }

    #[test]
    fn ph_reads_roles_and_shift() {
        let mut w = world();
        let rule = PhRule {
            acid: Some(LayerId::Substance(3)),
            lime: Some(LayerId::Substance(4)),
            ph_shift: 2,
        };
        let c = CellPos::new(1, 1, 1);
        assert_eq!(rule.at(&w, c), PH_NEUTRAL);
        set(&mut w, c, LayerId::Substance(3), 13);
        assert_eq!(rule.at(&w, c), 4);
        set(&mut w, c, LayerId::Substance(4), 255);
        assert_eq!(rule.at(&w, c), i32::from(PH_MAX));
        assert_eq!(PhRule::default().at(&w, c), PH_NEUTRAL);
        assert_eq!(rule.at(&w, CellPos::new(70, 1, 1)), PH_NEUTRAL);
        assert_eq!(ph(0, 255, 0), 0);
    }
}
