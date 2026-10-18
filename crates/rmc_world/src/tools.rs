//! Tool profiles loaded from `assets/tools.ron` (SAD §11.5).
//!
//! Phase 1 has one tool, the pickaxe. Its profile says how much damage a
//! swing does, how often it can swing, how far it reaches and which cells
//! around the hit cell a swing damages. The client turns a swing into an
//! `EditOp::Damage` command over [`ToolShape::at`] the hit cell.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::coords::CellPos;
use crate::shape::Shape;

/// The cells one swing affects, relative to the hit cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolShape {
    /// The hit cell only.
    Cell,
    /// Every cell within `radius` cells of the hit cell (see [`Shape::Sphere`]).
    Sphere { radius: u16 },
}

impl ToolShape {
    /// The edit shape of a swing that hits `cell`.
    pub fn at(self, cell: CellPos) -> Shape {
        match self {
            ToolShape::Cell => Shape::Cell(cell),
            ToolShape::Sphere { radius } => Shape::Sphere {
                center: cell,
                radius,
            },
        }
    }
}

/// One tool's profile.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProfile {
    /// Damage added to each cell of the shape per swing. At least 1.
    pub damage: u16,
    /// The shortest time between two swings, in seconds. Finite and > 0.
    pub interval_s: f32,
    /// How far from the eye a cell can be hit, in meters. Finite and > 0.
    pub reach_m: f32,
    pub shape: ToolShape,
}

/// Every tool's profile: the contents of `tools.ron`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolTable {
    pub pickaxe: ToolProfile,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing tools RON: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("tool {tool:?}: damage must be at least 1")]
    NoDamage { tool: &'static str },
    #[error("tool {tool:?}: {field} is {value}; it must be finite and > 0")]
    NotPositive {
        tool: &'static str,
        field: &'static str,
        value: f32,
    },
}

impl ToolTable {
    /// Loads and validates the table from a RON file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ToolError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ToolError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let table = Self::from_ron_str(&text)?;
        tracing::info!(path = %path.display(), "tool table loaded");
        Ok(table)
    }

    /// Parses and validates the table from RON text.
    pub fn from_ron_str(text: &str) -> Result<Self, ToolError> {
        let table: ToolTable = ron::from_str(text)?;
        table.pickaxe.validate("pickaxe")?;
        Ok(table)
    }
}

impl ToolProfile {
    fn validate(&self, tool: &'static str) -> Result<(), ToolError> {
        if self.damage == 0 {
            return Err(ToolError::NoDamage { tool });
        }
        for (field, value) in [("interval_s", self.interval_s), ("reach_m", self.reach_m)] {
            if !value.is_finite() || value <= 0.0 {
                return Err(ToolError::NotPositive { tool, field, value });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tools.ron");

    fn ron(damage: u16, interval: &str, reach: &str, shape: &str) -> String {
        format!(
            "(pickaxe: (damage: {damage}, interval_s: {interval}, reach_m: {reach}, shape: {shape}))"
        )
    }

    #[test]
    fn asset_loads_with_sad_defaults() {
        let t = ToolTable::load(TOOLS_PATH).expect("assets/tools.ron loads");
        // SAD §11.5 and §19: reach 3 m, swing every 0.35 s, one cell.
        assert_eq!(t.pickaxe.reach_m, 3.0);
        assert_eq!(t.pickaxe.interval_s, 0.35);
        assert_eq!(t.pickaxe.shape, ToolShape::Cell);
        assert!(t.pickaxe.damage >= 1);
    }

    #[test]
    fn shapes_center_on_the_hit_cell() {
        let c = CellPos::new(3, -4, 5);
        assert_eq!(ToolShape::Cell.at(c), Shape::Cell(c));
        assert_eq!(
            ToolShape::Sphere { radius: 2 }.at(c),
            Shape::Sphere {
                center: c,
                radius: 2
            }
        );
        let t = ToolTable::from_ron_str(&ron(7, "0.5", "2.0", "Sphere(radius: 1)")).unwrap();
        assert_eq!(t.pickaxe.shape, ToolShape::Sphere { radius: 1 });
        assert_eq!(t.pickaxe.damage, 7);
    }

    #[test]
    fn rejects_bad_profiles() {
        assert!(matches!(
            ToolTable::from_ron_str(&ron(0, "0.35", "3.0", "Cell")),
            Err(ToolError::NoDamage { .. })
        ));
        for (interval, reach, field) in [
            ("0.0", "3.0", "interval_s"),
            ("-1.0", "3.0", "interval_s"),
            ("inf", "3.0", "interval_s"),
            ("NaN", "3.0", "interval_s"),
            ("0.35", "0.0", "reach_m"),
            ("0.35", "-3.0", "reach_m"),
        ] {
            match ToolTable::from_ron_str(&ron(10, interval, reach, "Cell")) {
                Err(ToolError::NotPositive { field: f, .. }) => assert_eq!(f, field),
                other => panic!("{interval} {reach}: {other:?}"),
            }
        }
        assert!(matches!(
            ToolTable::from_ron_str(
                "(pickaxe: (damage: 1, interval_s: 1.0, reach_m: 1.0, shape: Cell, extra: 1))"
            ),
            Err(ToolError::Parse(_))
        ));
    }
}
