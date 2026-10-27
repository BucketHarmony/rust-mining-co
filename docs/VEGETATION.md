# Vegetation: research and plan

Owner request, 2026-10-27: plants that grow based on NPK needs and availability; vines and trees that are pretty and grow naturally over time, visibly on screen. This is a plan, not an implementation. The steps below need owner sign-off because several of them change SAD-P2 (§5, §6, §7.8, §8) and the tables hash.

## 1. What exists (SAD-P2 §7.6, §7.8)

- **Species** (`plants.ron`): moss, grass, fireweed and shrub.
  - Each species is one cell of a passable solid material (shrub takes two cells once `growth > 192`).
  - The growth layer is a `u8` that saturates at 255; spread needs 255.
  - The `PlantState` is `{ species, stress, age_seasons, held[10] }`.
- **Needs**:
  - Light and temperature are read at the plant cell. Moisture, `substance_min` and `substance_max` are read at the anchor below.
  - The needs are all or nothing. Any unmet need adds 1 stress; all met sets stress to 0; death comes at `stress_death` (16).
  - Uptake is a fixed amount per pass (for example grass takes 1 n and 1 p) and does not change the growth rate.
- **Nutrient cycle**:
  - `n`, `p` and `k` are substances (`plant_nutrient: true`).
  - Litter adds `c` to the anchor. `decompose` turns 4 c into 1 n on soil and gives off chokedamp.
  - Basalt holds `p: 20`, but it does not weather, so the phosphorus is locked in.
  - Nothing in the world supplies `k`.
  - Worldgen seeds no soil nutrients.
- **Water**: topsoil, dirt and clay are impermeable (field capacity 0), so their moisture is 0. Loam (porous, field capacity 48) exists but worldgen never lays it.
- **Result in the shipped game**:
  - Worldgen plants only where needs pass at season 0. Grass needs moisture 16 and n 2; moss needs moisture 8. Neither is met on topsoil, so the world starts essentially bare.
  - A plant placed with the brush stresses out unless it stands on painted loam with added nitrogen.
  - The green on hills is the topsoil tile, not plants.
- **Rendering**: plant cells mesh as full cubes coloured by material (tiles.ron or noise surfaces). There is no growth stage, sway, seasonal colour or form.
- **Calendar (2026-10-27)**: growth and spread run once a game day (`plant_hour`), so a season gives 7 growth passes and stress counts days.

## 2. Goals

1. Growth responds to how much N, P and K the soil holds, not only to whether a threshold is met. A plant short of one nutrient grows slowly and shows it.
2. Nutrients cycle: rock weathers out P and K, litter returns what plants took, nitrogen comes from decay, rain and nitrogen-fixing plants. A field can be exhausted and recover.
3. Trees and vines are multi-cell organisms that grow cell by cell along a species form, visible as they grow.
4. Plants look like plants: cross sprites and leafy volumes instead of cubes, with growth stages, wind sway, spring flowers and autumn colours.
5. Keep the SAD-P2 rules: integer logic, table-driven species, deterministic, conserving ledgers, Jacobi growth and a sweep for anything that claims cells.

## 3. Research notes (what the plan borrows)

- **Liebig's law of the minimum.** Growth is limited by the scarcest resource relative to need. In integer form, for each nutrient `x` with demand `d_x`, the sufficiency is `s_x = min(256, anchor_x * 256 / d_x)` and growth is `growth_rate * min_x(s_x) / 256`. Light, moisture and temperature use the same form, with a ramp instead of a step. One limiting factor at a time is easy to show and easy to tune.
- **NPK roles, for visual feedback.**
  - N drives leaf mass; deficiency yellows old leaves (chlorosis).
  - P drives roots, flowers and fruit; deficiency gives a purple-dark tint and no flowers.
  - K drives water use and hardiness; deficiency browns leaf edges and makes the plant more vulnerable to frost and drought stress.
- **Nutrient sources in soils.**
  - K comes from feldspar and mica weathering (granite) and from ash.
  - P comes from apatite (basalt, phosphorite) and is poorly mobile, which already matches `p` mobility 8.
  - N comes from organic decay, from legumes (fixation) and a little from rain and lightning; it is highly mobile and leaches, which matches `n` mobility 96.
- **Plant form generation.**
  - Space colonization (Runions et al. 2007) grows a branch network toward attraction points spread in a crown volume. It suits a voxel grid well and is deterministic given seeded points.
  - L-systems are compact but tend to look regular.
  - Plan: space colonization for tree crowns, and a simple tip rule for vines.
- **Voxel games.** Minecraft and Vintage Story place a complete tree in one step. Grow-in-place, as in Terra Nil and Timberborn, reads as "alive". Animating each new cell in (scale from 0 to 1 over about 1 s) gives that feeling cheaply.

## 4. Plan

Each step is a feature-sized unit with its own tests. Steps 1 and 2 are prerequisites; steps 4 to 6 build on step 3.

### V1. Soil that can feed plants

- Worldgen lays a loam (porous) A-horizon of 1 to 3 cells on gentle slopes, from a noise mask; slopes and peaks keep topsoil and stone.
- Worldgen seeds soil N, P and K from a fertility noise (`worldgen.ron` ranges per soil material), with richer valleys and poorer ridges.
- Basalt gets a slow `weathering_rate` (1), so P comes out over years. Granite gains a `k` composition and weathers at 1.
- `decompose` (reactions.ron) also returns p and k from `c`, by ratio, so litter returns what the plant took. Rows: `decompose_p`, `decompose_k`, or one row with three products.
- Optional: a rain nitrogen trickle as a calendar column (`rain_n`), ledgered as `rain_in`.
- Tests:
  - every surface column's soil holds nutrients within the configured range;
  - basalt weathers P out over N seasons (release proportional, as F11x);
  - the decompose ledger conserves amounts;
  - worldgen seeds moss and grass on a fresh world (a count above zero).

### V2. Graded growth (Liebig)

- In `plants.ron`, replace `substance_min` as a gate with `demand: [("n", 4), ("p", 2), ("k", 2)]` per growth pass.
- Sufficiency `s = min over (moisture, n, p, k, light, temp ramp)`, each in 0..256.
- Growth is `growth_per_substep * s / 256`. Uptake is `demand * s / 256`, taken from the anchor; uptake becomes the plant's `reserve`.
- Stress adds 1 only when `s < eco.starve_s` (for example 64). Toxicity (`substance_max`) stays a hard gate.
- `PlantState` gains `limit: u8`, the index of the limiting factor, so the client can show the deficiency, and `reserve: [u8; 3]` for stored N, P and K. These are a save and hash change.
- Tests:
  - growth is monotone in each nutrient;
  - the limiting factor is the minimum;
  - uptake is conserved (ledger `uptake_sink`, or `held`);
  - a grass field on poor soil grows slower than on rich soil, with a deterministic hash.

### V3. Plant rendering

- New mesher path for `plant`-tagged cells: a herb is 2 crossed quads (alpha-cut texture); a leaf volume is a cube with alpha-cut faces; a trunk or vine is a narrow box.
- Per-vertex data:
  - a `stage` (from the growth layer) scales the sprite from 30 % to 100 %;
  - a `health` tint comes from the limiting factor (N yellow, P purple, K brown edge);
  - a seasonal tint comes from the calendar (spring fresh green, summer deep, autumn per-species palette, winter bare or dull).
- Shader: a wind sway in the vertex shader, driven by `GameTime` and a cell hash, with the amplitude in `plants.ron` (grass more, trunk none).
- Growth animation: when a plant cell appears or its stage rises, the client eases its scale over about 1 s (client-only state keyed by cell).
- Tests (rule 21):
  - semantic: the mesh has the cross-quad vertex count and the stage, health and season attributes;
  - an e2e `plants_render` saves screenshots for spring, autumn and a deficient field.

### V4. Multi-cell organisms

- New layer `PlantPart` (u8):
  - bits 0-2: the parent direction (0 is the root, 1 to 6 a face neighbour);
  - bits 3-5: the part kind (trunk, branch, leaf, vine, flower, fruit).
- The root cell keeps the `PlantState`; parts carry no state.
- A tree or vine is a tree graph of parent pointers, which works across chunk borders because a pointer only names a neighbour.
- **Growth budget.** The root's growth (V2) fills a `biomass` counter. Each part costs `part_cost` from the species row.
- **Extension (a sweep, global sorted order).** For each root with enough biomass, add one part at the best tip by the species form rule.
  - Candidates are scored by the form rule plus a seeded hash tie-break.
  - The new cell must be air and must not collide.
  - The sweep applies at once, like spread, so no two plants claim one cell.
- **Severing.** When a part cell is dug or burned, the parts whose parent chain no longer reaches the root die. A small flood fill from the cut is applied in the same edit, through `World::apply` displacement (§7.0). They drop litter.
- Leaves in light feed the root: the leaf count in light (the light layer, 7.7) scales the root's growth. Shaded inner leaves are shed.
- Tests:
  - parent chains always end at a root (an invariant after every pass);
  - severing kills exactly the disconnected parts;
  - a tree across a chunk border grows the same as in one chunk (rule 20);
  - growth is deterministic.

### V5. Trees

- A species row gets `form: Tree(trunk: (4, 9), branch_from: 3, crown: (radius: 3, height: 4), leaf: "oak_leaves", wood: "oak_wood", deciduous: true)`.
- Shape:
  - The trunk grows straight up to a seeded height.
  - Branches start at `branch_from` and grow by space colonization toward seeded attraction points inside the crown ellipsoid.
  - Leaf cells fill around the branch tips.
- Stages: seed, then seedling (a herb sprite), sapling (trunk 2 plus a tuft), young (trunk plus a crown sketch), mature (full crown), old (crown thins). Lifespan is in years.
- Deciduous species:
  - leaves take the autumn palette in the calendar's autumn;
  - leaves fall as litter in late autumn, returning c, n, p and k to the soil around the trunk;
  - leaves regrow in spring from the root's reserve.
- Evergreen species (pine) keep their needles and have a narrow cone crown.
- Seeds:
  - mature trees drop fruit cells in autumn, which become seedlings nearby (spread radius 4 to 8);
  - fruit is harvestable by digging.
- Starting species: oak (broad, deciduous, hungry for N), birch (light, pioneer, poor soil), pine (evergreen cone, acid-tolerant), willow (beside water, hanging crown).

### V6. Vines

- `form: Vine(climb_tags: ["stone", "wood"], reach: 24, flower: "morning_glory_flower", hang: true)`.
- Growth:
  - The tip grows to an air cell that touches a climbable face.
  - Upward is preferred; sideways is allowed along a wall. Over a ledge the vine hangs down when `hang` is set.
  - A vine with no support within one cell stops.
- Flowers open in late spring and summer on cells in light; berries follow in autumn.
- Rendering: thin quads flat against the support face, so a stone wall gets a living skin.
- Starting species: ivy (evergreen, shade-tolerant), clematis (flowering), grape (berries, needs K).

### V7. Ecology and balance pass

- A headless report (`report_p2`) gains vegetation stocks per season: cover, biomass, soil NPK, and the limiting-factor histogram.
- Tune for a world that greens in spring, flowers in summer, colours and sheds in autumn, and rests in winter, with soil that runs down under crops and recovers under fallow and legumes (clover, a nitrogen-fixing groundcover).

## 5. Pacing

At 5 s a game hour, a season lasts 14 minutes and holds 7 growth passes, and a year lasts 56 minutes.

| Plant | Time to visible size |
|---|---|
| Grass | covers a meadow in about a season |
| Vine | climbs a 6-cell wall in about 2 seasons |
| Tree sapling | in the first year |
| Mature tree crown | 2 to 3 years, about 2 to 3 hours of play |

The `part_cost` and `growth_per_substep` columns set this; owner playtests decide.

## 6. Open questions for the owner

1. Does each step get an F2xx feature with failing tests first (the usual flow), or should V1 to V3 land as owner chores like the calendar?
2. A save format change (the `PlantState` fields and the `PlantPart` layer) breaks old saves. Is a migration needed, or are fresh worlds acceptable?
3. Plant growth runs once a game day. Should trees and vines extend more often (every few hours) so growth is visible while watching?
4. Should the pickaxe yield wood, fruit and seeds as inventory materials (so the player can plant them), or keep plants dig-only?

## 7. Owner answers (2026-10-27)

| Question | Answer |
|---|---|
| Process | F2xx features, failing tests first |
| Old saves | Fresh worlds are fine; no migration |
| Tree and vine cadence | A part every 4 game hours (stress stays daily) |
| Harvest | Wood, fruit and seeds as plantable inventory items |
| First species | Clover (nitrogen-fixing groundcover) |
| Day length | Keep `--day-seconds 120` |

Owner notes for other work, outside vegetation:
- Footsteps: less ticky and lower pitched, more of a thud, varying by the material underfoot.
- Rotate the pickaxe 90 degrees.
- A tool menu with different tools.
- An inventory with drag and drop between the character and the inventory: tools, equipment, armor and clothes.
