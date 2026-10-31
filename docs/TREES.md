# Trees as geometry: research and plan

Owner request, 2026-10-30: trees should not be blocks. Trees get real geometry for their splits and grow realistically. This is a plan, not an implementation. It replaces the tree half of docs/VEGETATION.md V4 and V5. Vines and one-cell plants keep the cell model. Several steps change SAD-P2 (§5 layers, §6 save and snapshot, §7.8 plants) and CLAUDE.md rules 2 and 14, so the owner signs off before T1 starts (see §6).

## 1. What exists

- A tree is cells.
  - The root is a wood cell with a `PlantState`.
  - Every other cell of the tree carries a `LayerId::Part` value: the kind (trunk, branch, leaf, fruit) and the face its parent is on. The root's `LayerId::Biomass` holds its growth budget.
- Form (`rmc_eco::parts::ExtendPass::next_tree_part`):
  - The trunk grows straight up to a seeded height: oak 4 to 6 cells, birch 5 to 7, pine 6 to 9.
  - Then an ellipsoid crown fills nearest-first, with ties broken by a seeded hash.
  - Branches are only the cells on the four horizontal axes through the crown center.
  - One part grows every `part_hours` (4 game hours) when biomass covers `cost`.
- Each cell is 0.5 m, so a tree is at most about 4.5 m tall and a few cells wide. Every angle is 90°.
- Coupling to the rest of the game:
  - Leaves are passable solids with `light_attenuation` 64, so crowns shade through the light pass (SAD-P2 §7.7).
  - Leaves earn biomass when lit.
  - Parts dig as items, and cutting a part drops everything above it as litter (ledgered).
  - Shedding, fruit and seed fall run in the Extend pass.
- Rendering: wood and leaf cells mesh as cubes. The leaf tuft (2026-10-28) is the only geometry that is not a cube.

The cube model limits the shape regardless of the rule that grows it. The owner's direction is to leave the grid for trees.

## 2. Goals

1. A tree is a branching structure with real angles, lengths and radii: a trunk that tapers, splits at species-typical angles, limbs that bend, and twigs that carry leaves.
2. Growth looks realistic over time. Buds flush and extend shoots, branches compete for light, shaded limbs die and drop, crowns lean into gaps, and trunks thicken with the load above them.
3. Species are table rows. Oak, birch and pine differ by their numbers (internode length, branch angle, divergence, apical dominance, tropisms, shedding), not by code.
4. The simulation stays deterministic, integer, table-driven and conserving (rules 3, 4, 16, 18, 19). The same seed gives the same tree on every machine, and chunk borders change nothing.
5. Trees still take part in the ecology. They shade, take up water and nutrients through their root cell, drop litter, fruit and seed. The player can collide with them and chop them.
6. The look can be iterated without the game: a tree lab grows one tree headless and exports it for a browser viewer (see T4).

## 3. Research notes

- **Self-organizing trees (Palubicki et al. 2009).** A tree is a graph of *metamers*. Each metamer is an internode with a leaf and an axillary bud at its tip.
  - Every growth cycle, each bud receives light from a voxel shadow grid.
  - Light flows down to the base. Vigor flows back up, split at each fork by the Borchert–Honda rule: `v_main = v · λQ_m / (λQ_m + (1−λ)Q_l)`. Here λ is apical dominance.
  - A bud with vigor makes a shoot of `floor(v)` metamers, turned by tropisms (up, toward light, down under weight).
  - Branches that stay unproductive are shed.
  - This model gives competition, gap-seeking and self-pruning from local rules. The shadow grid in the paper is a voxel grid, and this game already has one: the light layer.
- **Space colonization (Runions et al. 2007).** Attraction points in a crown envelope pull the nearest buds. It is good for a target silhouette, but it does not respond to light or neighbors. Palubicki's paper shows it as one form of bud fate. The plan keeps it as an optional envelope term per species.
- **Phyllotaxis (Vogel 1979).** Successive lateral buds turn by a divergence angle around the parent axis: 137.5° for spiral species and 180° for distichous ones such as beech and elm. Opposite or whorled species set buds per node (2 for maple, 4 to 6 for a pine whorl).
- **Branch angle and tropism (Honda 1971; Weber & Penn 1995).** A lateral leaves its parent at a branching angle: oak about 45°, birch 30° to 40° with drooping tips, pine near 70° to 90° with an upturned tip. Gravitropism and phototropism bend each new internode toward up, or toward the brighter neighbor cell.
- **Pipe model, Leonardo's rule (Shinozaki et al. 1964; Eloy 2011).** The radius is `r^n = Σ r_i^n` over the children, with n about 2 to 2.5. It is derived from the graph, never stored, so trunks thicken as the crown grows.
- **Growth pace.** A real oak extends 20 to 60 cm a year and reaches 15 m in about 40 years. The game cannot wait that long, so each species row sets `flushes` (growth cycles per growing season) and an internode length. Realistic proportions come at an accelerated pace (§5).

## 4. Design

### 4.1 State: a tree graph per tree

- `rmc_world::tree::TreeGraph`, owned by the chunk of the root cell, beside the plant map:
  - `Chunk.trees: BTreeMap<u16, TreeGraph>`, keyed by the root's local index like `plants`.
- A graph is a `Vec<Metamer>` in growth order, so a parent index is always below its children.
- `Metamer` (fixed point, all integer):
  - `parent: u16`, `u16::MAX` for the first internode, which starts at the root.
  - `dir: [i16; 3]`, a unit vector in Q14 (1.0 = 16384).
  - `length_mm: u16`.
  - `born: u16`, the cycle it grew in.
  - `bud: BudState`: Dormant, Active, Flower, Fruit or Dead.
  - `leaf: u8`: leaf area in steps; 0 when bare.
  - `shade: u8`: cycles in a row below `shed_light`.
- Positions are not stored. They are prefix sums of `dir · length` from the root, computed in i32 millimeters (rule 3: integers in world logic).
- Radii are not stored. They come from the pipe model over the subtree, in integer math with a Q8 lookup for `x^(1/n)`.
- Encoding: fixed little-endian bytes per metamer, into the chunk hash, the save (format version 3) and the snapshots (protocol version 3).
  - A cap of `max_metamers` per species (oak about 4,000) bounds memory and messages.
- The root stays a cell: the wood material, its `PlantState` and its `Biomass` layer. It is the anchor for needs, uptake and soil chemistry exactly as today.

### 4.2 Growth: the tree pass

- It runs in the Extend slot (`PassId::Extend`, after Spread), once per flush. Flushes are spread through the species' growing seasons on the `part_hours` clock.
- The pass order is unchanged (rule 17).
- Per tree, in sorted root order, computed from the previous state (a Jacobi pass, rule 15). Trees read only the light grid and the cells they would grow into.
  1. **Light.** Each active bud reads the light of the cell it sits in from the last light pass. Q accumulates base-ward.
  2. **Vigor.** Vigor is `biomass_in · min(1, Q/Q_full)`, capped by the Liebig sufficiency the root already computes for water and nutrients (VEGETATION V2). It is split up the graph by Borchert–Honda with λ from the row, in Q8.
  3. **Shoots.** A bud with vigor `v ≥ 1` adds `floor(v)` metamers, up to `max_shoot`.
     - Each new internode's direction is the previous one plus weighted tropisms: up (`gravitropism`), the brightest neighbor cell (`phototropism`), and optionally the envelope pull. The sum is renormalized in Q14.
     - Lateral buds are placed by `divergence` around the axis and `branch_angle` off it. Rotations use a 4,096-step integer sine table and Rodrigues' formula in i32.
  4. **Collision.** A new internode whose end lands in a solid cell, not a plant, turns once toward the open neighbor. If still blocked, its bud goes dormant. Trees may interpenetrate each other; competition happens through light.
  5. **Shedding.** A branch whose `shade` count reaches `shed_after` is cut off. Its wood and leaves become litter in the cell under its center, through the same ledgered path as today's `part_litter` (rule 16). Deciduous leaves (`leaf`) drop in the `shed` season and regrow in the next flush.
  6. **Cost.** Each metamer costs `cost_per_dm3` of biomass by its volume (from the pipe radius). Thickening the trunk is paid from the same budget, so old trees slow down by themselves (the Richards-curve behavior, without a curve table).
- Fruit and seed: Flower and Fruit bud states on lit tips, in the row's seasons. Seeds fall to the cell below and plant there or rot (V5 rules, unchanged).

### 4.3 Coupling back to the world

- **Light.** The light pass rasterizes every tree before its column pass.
  - Each leafy metamer adds `leaf · leaf_attenuation` to the cell holding its tip. Each internode with radius over `solid_radius` adds wood attenuation to the cells it crosses (a 3D DDA in integer mm).
  - The result is a per-pass canopy grid, added to the material attenuation. It is derived, not stored.
  - A tree change sets the light dirty flag.
  - Shade, self-shading and competition all come from the existing pass.
- **Collision.**
  - The player collides with capsules for the metamers over `solid_radius` (the trunk and limbs).
  - Twigs and leaves are passable.
  - `rmc_world` gains `World::tree_capsules(aabb)` for the controller's sweep.
- **Raycast and chopping.**
  - The raycast tests capsules after cells. A hit names a tree and a metamer.
  - Chopping is an edit, `EditOp::Chop { root, metamer }`.
    - It severs the subtree. It drops wood items by volume (`wood_per_item_dm3`) and leaves as litter, ledgered.
    - The damage model applies, so thick limbs take more hits.
  - Chopping the first internode fells the tree. Digging the root cell kills it, and the standing graph falls as items and litter.
- **Placement.** Placing a cell that intersects a solid capsule is refused, like placing into the player.
- **Replication.** `ChunkSnapshot` and `LayerSnapshot` carry `trees`.
  - The client applies them only inside the `ServerMsg` handlers, under rule 2 as extended in §6.

### 4.4 Rendering (client only)

- One mesh per tree, rebuilt when its graph changes (keyed by a graph hash). It is not part of chunk meshes.
- Wood is generalized cylinders along each axis chain:
  - 4 to 10 sides by radius, with tapered pipe-model radii.
  - Joints are smoothed by averaging the directions at a node.
  - Bark uses the tile pipeline (`<wood>_bark.png`). UVs run along the length.
- Leaves are clusters of alpha-tested leaf cards per leafy metamer:
  - Card count follows `leaf`, set by phyllotaxis around the twig.
  - Season colors come from the existing rows.
  - Shed leaves vanish. Autumn tints apply before shedding.
- Wind sway uses a vertex weight from the distance to the root and `1/r`, in the existing plant sway path.
- LOD comes later: far trees use a reduced graph (the main axes only) and leaf-cluster impostors.
- The data-map overlays color the tree mesh by the root's values.

### 4.5 What goes away

- Tree species stop writing `LayerId::Part` cells and `Branch`, `Trunk` and `Leaf` part kinds. The Part layer stays for vines (V6), which are on the grid by nature.
- Wood and leaf materials stay as item and litter materials. They are no longer placed in the world by trees.
- `ExtendPass::next_tree_part`, the tree branch of `mesher` plant cells, and the leaf tuft are removed once T7 lands. Tests for removed behavior are retired under `test-fix:` with reasons (rule 6).

## 5. Pacing

| Species | Internode | Flushes per growing season | Branch angle | Divergence | λ | Shape after about 3 game years | Cap |
|---|---|---|---|---|---|---|---|
| Oak | 25 cm | 4 | 45° | 137.5° | 0.52 | 6 to 7 m, broad, irregular crown | 4,000 metamers |
| Birch | 30 cm | 5 | 35°, tips droop | 137.5° | 0.58 | 7 to 8 m, narrow, light crown | 2,500 |
| Pine | 35 cm leader, laterals per whorl | 1 whorl and 3 extension flushes | 80° | whorl of 5 | 0.65 | 6 m cone, tiered | 3,000 |

These are starting numbers for the tree lab, and the balance pass (T8) owns them. Three game years is about three hours of play at the current calendar.

## 6. Rule changes the owner must make

The owner approved these on 2026-10-30 ("Make it so"). **Applied 2026-10-31** with the owner's explicit permission: CLAUDE.md rules 2 and 14 and SAD-P2 §18 now carry the text below.

Exact text to add:

- CLAUDE.md rule 2, appended: "Tree graphs (`docs/TREES.md`) change only in the tree pass inside `season_tick`, through `EditOp::Chop` and root death applied by `World::apply`, in worldgen, in save migration, and through delta/snapshot application."
- CLAUDE.md rule 14, appended: "Tree graphs follow rule 2's tree sentence. Tree math is fixed-point integer (millimeters, Q14 directions), as rule 3 requires."
- SAD-P2, a new section "18. Trees as geometry", with this body: "Trees follow docs/TREES.md §4. It amends §6 (`Chunk.trees`, folded into `Chunk::hash`, save format 3, snapshots and `ChunkDelta::Layers` carry tree graphs, protocol 3), §7.7 (the light pass adds the derived canopy grid) and §7.8 (the tree pass replaces tree parts in the Extend slot)."

The list below is the summary.

- **CLAUDE.md rule 2 / rule 14:** add "tree graphs change only in the tree pass, through `EditOp::Chop` and root death applied by `World::apply`, in worldgen, and through snapshot application."
- **SAD-P2 §5 / §6:** add `Chunk.trees` to chunk state, the hash, the save (format version 3, with a v2 → v3 migration that replaces Part-cell trees with a young graph of equal biomass) and the snapshots (protocol 3).
- **SAD-P2 §7.7:** the light pass adds the derived canopy grid.
- **SAD-P2 §7.8:** the tree pass replaces tree parts in the Extend slot.

## 7. Steps

Each step is one session under CLAUDE.md, with a test named first.

- **T1. Tree graph data.**
  - `rmc_world::tree`: `Metamer`, `TreeGraph`, fixed-point vectors, the integer sine table and rotations, prefix positions, pipe radii, and the encoding.
  - `Chunk.trees`, the hash, save v3, snapshot and protocol 3.
  - Tests:
    - a graph round-trips through save and snapshot;
    - the hash changes with any field;
    - positions and radii are exact for a hand-built Y fork;
    - integer rotation by 137.5° five times stays unit length within 1 Q14 step.
  - **Status (2026-10-30): done.** Notes:
    - The pipe exponent is fixed at 2, so radii are `isqrt(Σ r²)` (u64, micrometers) and no `x^(1/n)` table is needed.
    - The golden angle is `GOLDEN = 1564` steps of `TURN = 4096`, which is 137.46°.
    - The sine table is a quarter wave, built at compile time from an i64 Q30 Taylor series. It uses no floats.
    - `normalize` shifts short vectors up before taking the integer length.
    - Graphs are validated as they decode (`serde(try_from)`): the first metamer has no parent, every other parent comes earlier, and directions are unit within `UNIT_SLACK`. Snapshots and saves check only root indices.
    - A chunk without trees keeps its old hash. The trees hash is folded in only when the chunk has trees.
    - Version 2 saves load with no trees, and saving one again writes version 3. This covers T7's save conversion; T7 keeps the worldgen and garden part.
    - Trees in `ChunkDelta::Layers` (needed for chop and per-edit growth) wait for T5. Until then trees change only by snapshot.
- **T2. Growth.**
  - The tree pass: bud light, Borchert–Honda vigor, shoots, tropisms, phyllotaxis, collision and the metamer cap, with the `plants.ron` `form: Graph(...)` row.
  - Tests:
    - the same seed gives the same graph;
    - a tree across a chunk border grows like one inside a chunk;
    - a tree under a roof grows toward the open side;
    - λ high gives a leader and λ low gives a bush;
    - growth stops at the cap.
  - **Status (2026-10-31): done.** Notes:
    - The bud model lives in `rmc_eco::trees` and runs inside the extension sweep (`ExtendPass`, the Extend slot), per root in sorted order. A tree reads only the light layer and the cells it grows into, and writes only its own graph (`World::eco_put_tree`) and its root's biomass. So the order of trees doesn't matter, and the sweep equals a Jacobi pass. Parallel trees wait for T8.
    - `Metamer.lateral` is new (16 bytes per metamer). A metamer's `bud` is its terminal bud while it is a tip, and its axillary bud once it has a continuation: Active until it sprouts (then Dead) or is blocked (then Dormant).
    - Budget: a flush spends `biomass / cost` metamers. Vigor is that budget in Q8, split by Borchert–Honda. Each bud gets `floor(v)` metamers, capped by `max_shoot`, and the budget's remainder goes to the largest fractions. Without that step, a crown of many weak buds stalls with all of them under one metamer.
    - Light enters as each bud's light-layer value, or 0 below `bud_light`. The tree does not shade the light grid until T3, so in T2 shading comes only from cells.
    - Seeded rolls (one phase per axis) are keyed by the world seed and the metamer index, never by position, so a tree grows the same anywhere with the same light.
    - The shipped oak, birch and pine stay `Tree` rows until T7. The tests swap the oak's form text for a `Graph` row. `plants.ron` documents the row format with the oak's starting numbers.
    - Measured after 10 flushes (open sky, budget 31): λ 230 grows 7.0 m tall with 1.0 m reach, λ 133 grows 4.0 m and 1.3 m, λ 40 grows 2.0 m and 2.1 m. Under a roof open to +x, the mean metamer end leans 1.6 m toward the open side.
- **T3. Light and shedding.**
  - The canopy grid in the light pass, self-pruning and seasonal leaf drop, all ledgered.
  - Tests:
    - a crown darkens the ground under it;
    - a shaded lower limb is shed after `shed_after` flushes and its mass appears as litter;
    - the ledger balances.
  - **Status (2026-10-31): done.** Notes:
    - Canopy: `LightPass` adds attenuation to the cell each metamer ends in. That is `eco.leaf_attenuation` (3) per leaf step, plus `eco.wood_attenuation` (64) when the pipe radius is at least `eco.solid_radius_um` (80,000 µm). The sum over all trees stays below opaque, so canopy cells still take part in the lateral step. The canopy sits at the metamer ends rather than along a 3D DDA: internodes (25 cm) are shorter than a cell (50 cm), so every cell an axis crosses holds an end. `World::eco_put_tree` sets the light dirty flag.
    - `trees::tend` runs before a graph tree feeds, on every flush, including bare seasons:
      - Leaves: a metamer carries the row's `leaf` while it is younger than `leaf_age` seasons (0 means any age), and none in the `shed` season. Leaves regrow after the shed season at no cost.
      - Shade: `shade` counts flushes in a row below `shed_light`.
      - Shedding: a lateral subtree whose least `shade` reaches `shed_after` (0 never sheds) is cut (`TreeGraph::prune`). The trunk axis is never shed.
    - Litter: each fallen leaf, each shed metamer and each leaf on one lays `eco.metamer_litter` organic on the first anchor below its end, ledgered as `litter_in` through the same path as part litter.
    - New `Graph` fields `shed_light`, `shed_after` and `leaf_age` default to 0, so T2 rows still load.
- **T4. Tree lab and look.**
  - `rmc_app --tree-lab <species> --seed S --years N --out <file.json>` grows one tree headless and writes the graph per flush.
  - A browser viewer artifact draws it with the same cylinder and leaf-card rules for fast look iteration.
  - The client mesh (cylinders, bark, leaf cards, seasons, sway).
  - Tests: the semantic mesh test (vertex count follows the metamers; radii decrease along every axis) and e2e `trees_render` re-shot (rule 21).
  - **Status (2026-10-31): done.** Notes:
    - Lab: `rmc_eco::tree_lab::grow` plants the species on a 3×3×3-chunk field of its first anchor material and, per flush, tops the root's biomass up to full, recomputes light and runs the season's extension sweep. `rmc_app --tree-lab <species> --years N [--seed S] [--flushes F] [--out file.json]` writes every flush: the species look (bark, leaf and season colors, the Graph form) and each metamer's parent, direction, length, birth, leaf, end (mm) and pipe radius (µm).
    - `assets/tree_lab.ron` gives oak, birch and pine `Graph` forms for the lab and the T4 e2e until T7 moves them into `plants.ron`. Three lab years (seed 1, 4 flushes): oak 1039 metamers, 10.5 m tall, 2.2 m reach; birch 1398, 13.2 m, 3.6 m; pine 1047, 15.5 m. All are taller and thinner than §5's targets; T8 tunes them.
    - Viewer: https://claude.ai/artifact/9JPzknLdRZxM5Nk6HZ8VXz (three.js). It embeds the season-end frames of those three runs and opens any lab file; its geometry mirrors `treemesh.rs` constant for constant.
    - Client (`rmc_client::treemesh`): one mesh entity per graph tree, rebuilt when the graph hash or the season changes. Each metamer is a tapered cylinder of 4 + r/6 mm sides (at most 10) from its parent's end ring; continuations share the parent's ring, so axes are seamless. Each leaf is a double-sided lens card 16 cm long, turned by the divergence per metamer and spread over the leaf count, leaning out along the twig and up. Bark uses the chunk shader's triplanar tile by material id (`uv.x`); sway (`uv.y`) grows with path length up to 3 m and with thinness, so the trunk stands still and twigs move.
    - The look test is a new e2e `tree_graph_render` (summer, close, autumn, winter shots), not a re-shoot of `trees_render`: that scenario draws the shipped cell trees until T7, which re-shoots it.
- **T5. Interaction.**
  - Capsule collision, the raycast hit, `EditOp::Chop`, felling, root death and placement refusal.
  - Tests:
    - the player cannot walk through a trunk but walks through leaves;
    - chopping a limb drops wood by volume and leaves as litter;
    - a felled tree's chunk hash matches host and replica.
  - **Status (2026-10-31): done.** Notes:
    - Solid limbs: a metamer whose pipe radius is at least `eco.solid_radius_um` is solid. Its collision shape is the axis-aligned box around the internode (from the parent's end, or the root's top face, to its own end) grown by the radius on every axis (`World::limb_boxes`, `tree_boxes(region)`). That is an AABB stand-in for the capsule: exact for upright trunks, a little generous for slanted limbs. Twigs and leaves are never solid.
    - The controller stops at limb boxes as it does at cell layers. A box the player already overlaps (a tree grew into it) neither stops nor pushes the player.
    - `raycast_tree` slab-tests the limb boxes along the ray. The pickaxe chops the nearest solid limb when it is closer than the target cell, and otherwise damages the cell as before. `PickTarget` and the HUD still show the cell.
    - `EditOp::Chop { metamer, dmg }` acts on `Shape::Cell(root)`. Damage builds up host-side per (root, metamer), and the limb breaks at wood hardness × max(1, r / solid_radius). A thick trunk takes several hits. The cut subtree (`prune`) yields wood items: Σ π r² L / (`eco.wood_per_item_dm3` = 5 dm³). Its leaves fall as ledgered litter on the first anchor below each leaf's end. Chopping metamer 0 fells the whole graph, and the root cell and plant stay as a stump.
    - Digging the root cell (Damage) fells the graph as a chop would. Any other removal (`Remove`, displacement, a season pass) turns the graph to litter: `metamer_litter` per metamer plus the leaves, ledgered.
    - `Place` into a cell a solid limb overlaps is refused (`PlaceInTree`).
    - Tree changes ride `ChunkDelta::Layers { trees }`. The protocol stays at 3 because 3 already carries trees. Chop damage is host-only and resets whenever the graph changes, growth included.
- **T6. Fruit and seed.**
  - Flower and fruit buds, seed fall and planting, on the graph.
  - Tests ported from V5's fruit tests.
  - **Status (2026-10-31): done.** Notes:
    - Flowers and fruit are bud states on tips (metamers without a continuation). They need an Active bud and leaves. A flower also needs `leaf_light`; fruit needs any light.
    - Once a tree has `mature` metamers (a new Graph field, default 0), step 3 opens flowers in a flower season, then sets fruit in the fruit's `grow` season, each up to its `count`. Each costs `cost` and is paid before growth. Tips go by the least roll of (seed ^ season, bud, index).
    - A flowering or fruiting tip stops extending. Once the flower or fruit drops, its bud turns Active again.
    - Drops use the V5/V6 rules, starting from the cell the tip ends in:
      - Fruit falls in its `fall` season. It takes root with `take`, otherwise it rots as `part_litter`.
      - Flowers wilt out of their seasons.
      - A bare season drops both.
    - `mature` exists because a seedling's only tip would otherwise flower and stall. Lab values: oak 200, birch 150, pine 250. The lab oak (seed 1) first fruits in its first autumn at 202 metamers: six acorns every autumn, all down by winter.
    - Client: Flower and Fruit buds draw as small octahedra in the flower and fruit material colors (`GraphLook.fruit`, `GraphLook.flower`); fruit hangs below the tip. The lab JSON gains each metamer's `bud`.
- **T7. Migration and cleanup.**
  - Save v2 → v3 conversion, worldgen and the garden plant graph trees, and the tree code paths on Part cells are removed.
  - Test retirements go in a `test-fix:` commit.
  - **Status (2026-10-31): done.** Notes:
    - `plants.ron` ships oak, birch and pine as `Graph` rows, with the lab numbers. `tree_lab.ron` stays as the lab override and is equal to them for now.
    - Worldgen and the garden plant a seed (a root cell with a `PlantState`), so both grow graphs with no change.
    - Saves already converted in T1. An old world's part cells on a Graph root rot in the extension sweep: each turns to air and rots as `part_litter` at its landing (ledgered). The graph then grows from the root.
    - The `Tree` form, `next_tree_part`, the cell-tree mesher path and the tuft stay as a data option rather than being deleted. Removing them would mean deleting their tests (rule 6), and a modder can still pick them. The cell-tree tests restore the V4 forms through `tables::fixtures::cell_tree_sources` (`test-fix:` commit 940b750).
    - `trees_render` is not re-shot as graph trees. It keeps drawing cell trees from its own Tree-form tables (`test-fix:`). The shipped look is `tree_graph_render`.
- **T8. Balance and performance.**
  - The ten-year headless run with trees, Extend-pass and mesher benches, and the metamer caps tuned.
  - Numbers go in this file.
  - **Status (2026-10-31): done.** Notes:
    - The T4 forms grew without limit. The leader gets `max_shoot` in every flush once λ is above about 0.51, and the crown collapses to a bush below that. Oak reached 10.5 m in three lab years, pine 21 m, and reach stalled near 2 m. λ has no setting in between: 128 stalls at 5 m and 130 runs to 14 m.
    - Two Graph fields bound the crown: `max_height_mm` and `max_reach_mm` (0 = no limit). A bud ending at or past either collects no light (Q = 0), so Borchert–Honda routes its vigor to the rest of the crown. A grown tree then fills and spreads inside its envelope, and its trunk keeps thickening by the pipe model, much as real trees do once they reach their height.
    - Shipped numbers:
      - Oak: `max_shoot` 2, `tip_um` 2500, envelope 7 m by 4.5 m.
      - Birch: `tip_um` 1800, envelope 8 m by 2.5 m.
      - Pine: `max_shoot` 1, envelope 6.5 m by 2.5 m.
    - Lab, seed 1, 4 flushes (height, reach, trunk radius, height over trunk diameter):

      | Species | Year 3 | Year 10 |
      |---|---|---|
      | Oak | 7.2 m, 3.1 m, 47 mm, 77 | 7.2 m, 4.7 m, 100 mm, 36 |
      | Birch | 8.3 m, 2.8 m, 40 mm, 104 | 8.3 m, 2.8 m, 58 mm, 72 (at its 2 500 cap) |
      | Pine | 6.8 m, 2.8 m, 49 mm, 70 | 6.8 m, 2.8 m, 85 mm, 40 (at its 3 000 cap) |

    - The caps stay (4 000 / 2 500 / 3 000). They bound a graph at 64 kB (4 000 metamers of 16 bytes) and the per-tree costs below.
    - Ten-year headless world (seed 1, 128 × 64 × 128, `--seasons 40`):
      - At the end: 86 birch graphs (203 704 metamers, the largest at the cap, tallest 8.3 m) and 62 pine graphs (165 667, tallest 6.8 m). Invariants held every season.
      - The oaks die out over the ten years (62 → 0). The nitrogen and stress limits kill them. The cell-tree run before T7 lost them on the same curve (54 → 1), so this is an ecology balance item, not a graph one. It is left to the owner.
    - Performance (docs/perf-p2.md):
      - One flush of a ten-year oak takes 194 µs and its mesh 1.32 ms.
      - A season of the ten-year world takes 1.78 s in release, of which Extend is 218 ms and Light 201 ms.
      - Trees stay serial in the sweep. `tree_boxes` is not cached: a player's sweep touches a few trees at O(metamers) each.
    - The headless report (`VegetationStocks.trees`) gains per-species graphs, metamers, largest and tallest.

- **T9. Crown shape, competition, conifers** (owner request after T8).
  - The problem: trees grew tall and thin, like a packed stand, even alone. Pines did not read as conifers.
  - **Status (2026-10-31): done.** Notes:
    - Why trees were thin: a game tree gets `biomass / cost` metamers per flush, a handful while it is young. Borchert–Honda gave them all to the best-lit bud, which is the leader, so a young tree was a pole before it had a crown.
    - Light sharing: buds that end in the same cell share its light (Q = light / buds in the cell). A dense crown no longer out-competes its own lit edge, and the vigor goes to buds with room.
    - Crown shape: `crown_ratio` (width over height, 256ths), `crown_peak` (where it is widest, 256ths of the height, 0 = a cone) and `crown_damp` (256ths). A bud outside that shape for the tree's current height sees `crown_damp` of its light.
    - The tree may only rise as high as two things allow, plus one cell of slack:
      - what its reach supports (`2 · reach / ratio`);
      - what its mass carries (`internode_mm · √metamers`).
      So a thin young tree fills out before it rises. Once its laterals reach out, it climbs again.
    - `max_reach_mm` is dropped from the shipped rows. The shape bounds the width, and `max_height_mm` still caps the height.
    - Lateral setpoint: lateral axes bend toward `lateral_setpoint` from straight up (gravitropism toward that angle, not toward up). Oak 55°, birch 40°, pine 88° (level branches).
    - Whorls: `whorl` short nodes (`whorl_mm` long) every `whorl_gap` long internodes, and only whorl nodes sprout laterals. Pine: 5 nodes, gap 2, 30 mm.
    - Needles: with `needles` above 0, the client draws `leaf` rings of that many long, thin cards per leafy metamer, staggered by half a step and swept forward along the twig, instead of leaf cards. Pine draws 4 (6 at first; see the follow-ups).
    - A tree that has stopped growing (at its cap or envelope) keeps the leaves of its newest wood (within `leaf_age` flushes of its newest metamer). Before this, a capped birch shed its whole crown.
    - Competition needs no new mechanism. Leaves already shade the light grid (T3), so a neighbor's crown darkens a tree's side buds. Those buds lose vigor, and the tree reaches out less and rises more. The test grows a 3 × 3 stand at 4-cell spacing against a lone tree.
    - Shipped shapes: oak ratio 300 peak 140 damp 2, birch 150 / 110 / 2, pine 180 / 0 / 32. The pine's damp is higher so its lower whorls keep growing into the cone. Pine λ is 128.
    - Lab, seed 1, 10 years, full budget: oak about 6.5 m tall and 9 m across, irregular and broad; birch an oval, 10.3 m by 7.5 m; pine a cone, 6.9 m tall and 6.4 m across its base.
    - At the shipped pine cost (10), the pine is a round bush of 3.3 m at year 2, a cone of 5.8 m by 5.6 m at year 5, and 6.8 m by 6.2 m at year 7. The `tree_graph_render` scenario grows two years, so its pine (new `evergreen` close shot) is still a sapling. The cone shows from about year 4.
    - Follow-ups (owner picks, 2026-10-31):
      - No shoot ends below the trunk's base (`clear` treats such an end as blocked). Before, low pine laterals ended half a cell down, beside the root cell.
      - `tree_graph_render` grows four years (17 seasons, into the fifth summer), so its pine shows its cone. The scenario now takes about 20 minutes on lavapipe instead of about 5.
      - Bench `tree_mesh_pine`: a ten-year pine (3 000 metamers) took 7.0 ms and 39 931 quads with 6 needles a ring, against the oak's 1.24 ms and 15 748 quads. Building each twig's frame once in floats saved 5 %. 4 needles a ring (2 rings, staggered: 8 directions) bring it to 4.1 ms and 31 115 quads. About 13 500 of those are bark, so the rest of the gap is wood, not needles.

## 8. Defaults chosen (the owner can overrule)

- Pace is accelerated with realistic proportions (§5), not real time.
- Trunks and limbs over `solid_radius` (8 cm) block the player. Twigs and leaves do not.
- Any tool chops, with the damage model for hits. A dedicated axe is a later item.
- Vines and one-cell plants stay on the grid.
