# Data maps

Heat maps of the soil and water over the terrain, in the style of a city
builder's data views: the world turns pale, and one value shows in its own
color. `M` steps to the next map, `Shift+M` to the previous one; past the
last map (or before the first) the maps turn off.

Code: `crates/rmc_client/src/overlay.rs` (the table, the keys, the panel,
the runoff arrows) and `crates/rmc_client/src/mesher.rs` (`ColorSource`,
`ColumnField`, `water_table_depth`, `runoff`). Data:
`assets/overlays.ron`.

## The maps

| Map | Reads | Scale |
|---|---|---|
| Nitrogen (N), Phosphorus (P), Potassium (K) | substance layers `n`, `p`, `k` | 0 to 255, pale to green / orange / purple |
| Moisture | the moisture layer | 0 to 64, pale to blue |
| Water table | per column: cells from the surface down to the first saturated cell | 0 (deep blue, at the surface) to 16 (pale, deeper) |
| Runoff | per column: how far the surface drops to its lowest neighbour | 0 (flat) to 4, pale to teal; arrows show the way |
| Organic matter | substance `c` | 0 to 128, pale to brown |
| Temperature | the temperature layer (0.1 °C) | -20 °C blue to 40 °C red |
| Sunlight | the light layer | 0 (slate) to 255 (yellow) |
| Plant growth | the growth layer | 0 to 255, pale to green |
| Copper, Acidity | substances `cu`, `acid` | pale to teal / pink |

A map row is `(key, label, source, range, low, high, legend)`. `source` is
`Substance("<key>")`, `Moisture`, `Temperature`, `Light`, `Growth`,
`Biomass`, `Regolith`, `WaterTable` or `Runoff`. The colors run linearly
from `low` at `range.0` to `high` at `range.1`, clamped outside it. A
substance map waits until the client has the eco tables. Adding a map is a
new row; no code changes.

**Saturated** (the water table): a cell that holds water (a liquid-pass
cell) or a porous cell whose moisture is at its `field_capacity`. The search
goes down from the column's surface (which includes liquids) for at most
`range.1` cells.

**Runoff**: the lowest of the four horizontal neighbours' surfaces that is
lower than the column's own; ties go to +x, -x, +z, -z in that order. The
arrows are drawn every `arrows.spacing` columns within `arrows.radius`
columns of the camera. They are re-read when the camera moves to another
column, or every 30 frames while the replica changes.

## The panel

A white panel on the right (`HudSlot(Right)`), shown while a map is picked.
It contains:

- the heading and the key hint;
- one row per map, each with a dot in the map's color, the picked row filled
  blue;
- the legend: the map's name, a color bar of `legend_steps` swatches from
  `low` to `high`, the names of the two ends, and the value at the targeted
  cell ("Here: ..."). Temperature uses the Settings unit; the water table
  reads as "N cells down", runoff as "flat" or "drops N".

## Decisions

- **Column values.** The water table and runoff are computed per column
  while a chunk meshes, over every y of the chunk, so walls and the top read
  alike. A chunk is not re-meshed when the chunk below it changes; the map
  catches up at the next re-mesh of that chunk (a pick, an edit, a season).
- **Interplay with the editor overlay.** The editor's F7 overlay and the
  data maps both write `MeshColorSource`, each only when its own pick
  changes, so the last pick wins.
- **Arrows are gizmos.** They are drawn only in apps with Bevy's
  `GizmoPlugin`. Tests read `RunoffArrows` instead.
- **The panel keeps its own palette.** The rest of the HUD is dark umber
  (`assets/ui.ron`); the data map panel is light, and its colors are in
  `assets/overlays.ron`. Its text contrast is tested over black and white.

Shots: `docs/screens/overlays_render-*.png` (e2e `overlays_render`).
