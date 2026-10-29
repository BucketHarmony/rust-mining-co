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
- **Status: done (2026-10-28).**
  - `worldgen.ron` gains `soil_horizon` (loam 1–3 cells, `max_slope: 1`, a mask noise, starting `moisture: 32`) and `fertility` (`(substance, lo, hi)` per soil material: loam, topsoil, dirt). `rmc_worldgen::SoilInit` lays both per chunk, before the water table; the game's `generate_world` uses it (`Generator::with_soil`).
  - Basalt weathers at 16; `decompose` gives 1 n, 1 p and 1 k per 4 c.
  - Tests: `soil::tests::{loam_lies_damp_on_gentle_ground_only, surface_soil_holds_nutrients_in_range, unknown_names_are_refused}`, `weathering::tests::basalt_weathers_phosphorus_out`, `chemistry::tests::decompose_returns_n_p_and_k_and_the_ledger_conserves`, `garden::tests::a_fresh_world_seeds_moss_and_grass`.
  - Decisions:
    - Basalt's rate is 16, not 1. §7.5 releases `total · Δ / 256` per pass, truncating, so with p 20 any rate under 13 releases nothing. 16 releases 1 p a pass for 16 passes, then the regolith is full and basalt stays basalt.
    - Granite keeps rate 0 and no `k`: every weathering test fixture uses granite as inert ground (one asserts its rate is 0). K comes from fertility and from decompose.
    - The horizon starts damp (moisture 32 of loam's 48) because the water table wets only cells below level 64, far under the surface.
    - Fertility is the mean of a noise and the column's depth under the highest possible surface; amounts are `lo + (hi − lo) · f / 256`.
    - `rain_n` (optional) is not done.
    - The test kit and the eco benches keep generating without `with_soil`, so the E2E and bench worlds are unchanged.

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
- **Status: done (2026-10-28).**
  - `plants.ron` rows gain `demand` and `fix` (both optional); `eco.ron` gains `starve_s: Some(64)`. `rmc_eco::plants::sufficiency` gives `s` and the limiting row. The growth pass scales growth, the demand's uptake and `fix` by `s / 256`; below `starve_s` a demanding plant's stress rises.
  - Clover (material 30, the fifth `plants.ron` row): needs light 64 and moisture 16, demands 2 p and 2 k, fixes 2 n into its anchor, worldgen chance 24. The garden's lawn around the spawn is clover.
  - Tests: `plants::tests::{sufficiency_is_the_most_limiting_factor, graded_growth_is_monotone_in_each_nutrient, graded_uptake_and_fixation_are_scaled_and_ledgered, starving_plants_stress_and_fed_ones_do_not, a_clover_field_on_poor_soil_grows_slower_than_on_rich_soil}`.
  - Decisions:
    - Additive, not a replacement: `substance_min` stays a gate, and the four pioneers keep their rows (no demand, so `s = 256` and they behave as before). V7 may move them to `demand`.
    - Sufficiency covers the demanded substances only. Moisture, light and temperature stay gates, as in §7.8.
    - `PlantState` is unchanged (no `limit`, no `reserve`), so saves keep format 2. The client can compute the limiting factor from the anchor's layers with `sufficiency`. Demand uptake books `uptake_sink` (or `held` for a `holds` species).
    - Fixation is booked as `litter_in` (a per-substance source) rather than a new ledger field, which would change the protocol codec.
    - The test field is clover, not grass, because clover is the species with a demand.

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
- **Status: done (2026-10-28).**
  - Tables: `plants.ron` rows gain `sway` (0 to 255) and `season_colors` (per `seasons.ron` key); `substances.ron` rows gain `deficiency_color` (n yellow, p purple, k brown). `EcoTables::plant_looks` and `deficiency_colors` hand them to the client (`ClientEco`), with each species' `needs`: its demand, or its `substance_min` without one.
  - Mesher: a `plant`-tagged solid is no longer a cube (`is_cube`): it hides no faces, takes no AO, and draws as two crossed planes, each from both sides (16 vertices), standing on the cell's floor. `MeshInput::with_plants` gives each plant cell its stage (growth layer; 255 for a tall plant's cells), its color (the material color, moved toward the season's color and toward the limiting substance's deficiency color by how short it is, from `sufficiency` over the anchor), and its sway.
  - Shader: `emissive.wesl` gains a vertex entry, the PBR mesh vertex shader plus wind: the mesher writes a top vertex's sway (meters) in uv.y, and the vertex moves by `sin(time × sway_rate + phase(x, z))` (`#[uniform(107)] sway_rate`, `SWAY_RATE` 1.6 rad/s).
  - The mesh pipeline re-meshes the chunks with plants when the season or the last finished season changes (`follow_plants`).
  - Tests: `mesher::tests::{plants_are_crossed_quads_sized_by_growth_with_the_ground_beneath, plants_neither_occlude_nor_merge_with_cubes}`, `material::tests::plants_sway_in_the_vertex_shader`, `tables::tests::plant_looks_resolve_needs_sway_and_season_colors`, e2e `plants_render` (screenshots `docs/screens/plants_render-{spring,later}-*.png`).
  - Decisions:
    - test-fix: `material::tests::emissive_extension_compiles` pinned the default vertex shader; it now pins the embedded one (and the default prepass vertex shader). `mesher::tests::face_shade_differs_by_normal` checks every cube material, since plants are no longer cubes.
    - Vertex colors, not textures: the crossed planes are solid colored (no alpha-cut sprite yet), so the tile pipeline is untouched. A full-grown cross spans 80 % of the cell (`PLANT_SPREAD`), so neighbors stay apart.
    - Leaf volumes, trunks and vines come with V4 to V6, which add those cells.
    - The grow-in animation is not done: the mesh steps with the growth layer, which changes a little each growth pass.
    - Shadows use the default prepass vertex shader, so they do not sway. The project has no depth prepass, so the main pass alone is consistent.
    - The deficiency color uses the client's copy of the anchor's layers, so the host's state is unchanged (no `limit` in `PlantState`).
    - Rule 21: semantic assertions and screenshots; no pixel checks.

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
- **Status: done (2026-10-28).**
  - Layers: `LayerId::Part` and `LayerId::Biomass`, appended after the existing layers so saves keep their format (version 2). A part's value is `dir | kind << 3`: `dir` 1 to 6 indexes `PART_DIRS` (the parent's face), `kind` is `PartKind` (trunk, branch, leaf, vine, flower, fruit); 0 is "not a part". `Part::{toward, parent, value, from_value}` in `rmc_world::layers`.
  - Biomass: the root's `Biomass` layer. With a `parts` row, growth past maturity (what the cap would throw away) goes there instead.
  - Tables: `plants.ron` rows gain an optional `parts: (cost, leaf_feed, leaf_light, wood, leaf, form: Tree(trunk: (lo, hi), crown: (radius, height)))`; `eco.ron` gains `part_litter`; `calendar.ron` gains `part_hours: Some(4)`.
  - Extension: `rmc_eco::parts::ExtendPass`, a sweep in global sorted order (`PassId::Extend`, after Spread). Per root: lit leaves (light ≥ `leaf_light`) add `leaf_feed / 256` each to biomass; at `cost` the plant adds one part. The trunk grows straight up to a seeded height in `trunk`; then the crown fills an ellipsoid (radius, height) over the trunk top, nearest first, ties by a seeded hash. A part grows into air or water (water gives way by the §7.0 rules).
  - Severing: `World::apply`'s displacement clears a removed plant cell's `Part` and `Biomass` and, recursively, every neighbor part whose parent it was. A severed part over an anchor drops `part_litter` organic matter there (ledger `litter_in`, the amount actually deposited).
  - Oak: species 6 (`oak_wood` trunk, `oak_leaves` crown, trunk 4 to 6, crown 2 × 3), materials 31 and 32. The garden plants an oak near each corner of the meadow.
  - Tests: `layers::tests::part_values_round_trip_and_point_at_their_parent`, `calendar::tests::parts_grow_every_part_hours_and_in_the_plant_hour_without_it`, `parts::tests::{a_tree_grows_its_trunk_then_fills_its_crown, a_part_costs_biomass_and_lit_leaves_make_it, growth_past_maturity_becomes_biomass, cutting_a_part_cuts_off_exactly_what_hangs_from_it, a_cut_off_part_leaves_litter_and_the_ledger_balances, a_tree_across_a_chunk_border_grows_like_one_inside_a_chunk, a_tree_grows_through_seasons_and_its_parts_reach_its_root}`; the last runs the invariant checker and `parts::check_parts` (every part's chain reaches a root) after every pass.
  - Decisions:
    - test-fix: the pinned pass order (`tick::tests::invariants_hold_after_each_pass_20_worlds`) ends with Extend; the pinned species list ends with oak.
    - Biomass is a layer rather than a `PlantState` field, so the save format stays 2 (a bump would break the pinned save tests and every save).
    - The Extend pass runs every `part_hours` calendar hours (owner answer: every 4 game hours) and every sub-step of a flat season; stress stays daily.
    - Leaves are passable solids with `light_attenuation` 64, so a crown shades without walling the player in.
    - Oak's `substance_min` is n 4 (not 2), which also keeps a worldgen test's one-match text edit of `plants.ron` unique.
    - Not yet: branches, shedding of shaded leaves, fruit and seeds, worldgen trees (oak's `worldgen_chance` is 0), and render shapes for wood and leaves (they mesh as cubes). These are V5. The editor's overlay list is unchanged (the colormap knows both layers).

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

- **Status: done (2026-10-28).**
  - Seeds and wood:
    - A tree's material is its seed (`acorn`, `birch_seed`, `pine_cone`; passable, `plant`-tagged).
    - Planted, the seed grows like any plant. Once grown with biomass for a part, the Extend pass ripens it into the species' `wood` (`World::eco_set`).
    - The §7.0 displacement keeps the plant when a root's seed turns into its own wood (`EcoRules::plant_woods`). Wood placed by hand is never a tree.
    - The growth and spread passes read a plant's body as its seed or its wood (`Species::is_body`).
  - Shape:
    - The trunk grows to a seeded height, then the crown ellipsoid.
    - With `branches: true`, the crown cells on the four horizontal axes through the trunk top, next to wood, grow as `Branch` parts in the wood material. The rest grow as leaves.
  - Shedding:
    - `shed: Some("winter")` names the season the tree stands bare.
    - In that season, each run of the Extend pass drops every leaf and fruit as `part_litter` organic matter on the soil below (ledger `litter_in`), and the tree grows nothing.
    - The next season, the crown regrows from biomass.
  - Fruit:
    - `fruit: (grow, fall, count, take)`. In its `grow` season, a full crown grows up to `count` fruit parts: the seed material, beside a leaf, chosen by a seeded roll.
    - In the `fall` season each fruit drops to the first ground below. With a chance of `take` in 256, and where a seedling's needs hold (`PlantStarts`), it starts a new plant (ledger `plants_born`). Otherwise it rots as litter.
    - Trees spread only by fruit (`spread_chance` 0).
  - Leaves in the client:
    - `PlantLook::leaf` names a tree's leaf material.
    - `MeshInput::with_plants` sets `recolor`, which holds the leaf cubes' color mixed toward the season row's color by `PLANT_SEASON_MIX`.
    - Chunks with a `Part` layer re-mesh when the season changes.
    - Oak turns orange-brown and birch yellow in autumn; pine keeps its color.
  - Harvest: digging wood, leaves or fruit yields that material's item. A fruit item is the seed, so it can be planted.
  - Species:

    | Species | Material id | Trunk | Crown | Leaves | Fruit |
    |---|---|---|---|---|---|
    | oak | 33 (acorn) | 4–6 | 2 × 3 | shed in winter | 6, autumn → winter |
    | birch | 34–36 | 5–7 | 1 × 3 | shed in winter | 8, summer → autumn |
    | pine | 37–39 | 6–9 | 2 × 4 | evergreen (`light_attenuation` 80) | 4, summer → autumn |

    - Birch is a poor-soil pioneer with no `substance_min`.
    - Pine tolerates acid up to 96.
    - Each species has `worldgen_chance` 2, so worldgen sows tree seedlings.
  - Tests:
    - `parts::tests::{a_seed_grows_as_a_seedling_then_ripens_into_its_wood, a_bare_tree_sheds_its_leaves_as_litter_and_regrows_them, a_full_crown_fruits_in_its_season_and_fallen_fruit_takes_root_or_rots, wood_leaves_and_fruit_dig_as_items_and_the_seed_plants, tree_rows_name_known_seasons_and_bear_fruit, a_seed_grows_into_a_tree_that_stands_bare_in_winter}`
    - `tables::tests::tree_looks_name_their_leaves_and_turn_in_autumn`
    - `mesher::tests::tree_leaves_take_their_season_color`
    - e2e `trees_render` (screenshots in `docs/screens/`)
  - Decisions:
    - test-fix 524d52e: the crown test allows `Branch` cells exactly on the crown axes. The pinned species list ends with oak, birch and pine. The mesher's plant-look literal gains `leaf: None` (a new field, no assertion changed).
    - The client's season comes from the weather plugin. The test kit adds it only with `weather: true`, so `trees_render` turns it on. V3's `plants_render` does not, so its "later" shot still shows spring colors. Re-shooting it is left for V7.
    - Branches are fixed cells on the crown axes rather than space colonization. This keeps them deterministic and cheap, and readable as a silhouette.
    - The bare season stops all growth, including fruit. Falling leaves and fruit pass through plant cells (the trunk) to the ground.
    - Not done:
      - Willow (needs water-side placement).
      - Old-age crown thinning.
      - Seedling and sapling sprites (a seedling draws as its seed's crossed quads).
      - Autumn leaf fall staggered within the season.

### V6. Vines

- `form: Vine(climb_tags: ["stone", "wood"], reach: 24, flower: "morning_glory_flower", hang: true)`.
- Growth:
  - The tip grows to an air cell that touches a climbable face.
  - Upward is preferred; sideways is allowed along a wall. Over a ledge the vine hangs down when `hang` is set.
  - A vine with no support within one cell stops.
- Flowers open in late spring and summer on cells in light; berries follow in autumn.
- Rendering: thin quads flat against the support face, so a stone wall gets a living skin.
- Starting species: ivy (evergreen, shade-tolerant), clematis (flowering), grape (berries, needs K).
- **Status: done (2026-10-28).**
  - Rows:
    - `parts.form` is `Tree(trunk, crown)` or `Vine(climb_tags, reach, hang)`.
    - `parts.flower: Some((material, seasons, count))` is optional on any form.
    - The vine's seed ripens into `vine_stem` (shared by all vines), which is the root. Its vine cells are the row's `leaf` material, tagged `leaf` and `vine`, passable, and they shade a little.
  - Growth (`ExtendPass::next_vine`):
    - Below `reach` vine cells, the next part is an open face-neighbor of the root or of a vine cell.
    - The cell must touch a material with one of `climb_tags` outside the plant, along a face or an edge. The edge rule lets a vine crest a wall and turn around corners.
    - Up is preferred over sideways, and sideways over down. Ties go by a seeded roll of the offset from the root.
    - With `hang`, a cell straight below a supported or hanging vine cell needs no support, so the vine hangs as a curtain below a ledge.
    - A vine with nothing to climb stays a stem and spends nothing.
    - Vine cells feed like leaves when lit.
  - Flowers and fruit:
    - In a flower season, below `count`, a flower opens once the form can grow no further (before fruit): on an open cell beside a lit vine cell or leaf, chosen by the least seeded roll.
    - Out of its seasons the flower wilts: it falls as `part_litter` on the soil below, ledgered like a shed leaf.
    - Fruit works as for trees (V5), beside any vine cell.
    - A bare (`shed`) season drops flowers and fruit. Vine cells stay, so a wall stays covered in winter.
  - Rendering:
    - `VINE_TAG` materials are not cubes. A vine cell draws as a thin panel `VINE_INSET` (0.05 cell) off each face it shares with a cube, from both sides, in its season color with that face's shade.
    - A vine cell with no cube beside it (a curtain) draws as a full plant cross.
    - Vines hide nothing behind them.
  - Species:

    | Species | Material ids | Reach | Leaves | Flowers | Fruit |
    |---|---|---|---|---|---|
    | ivy | 41, 42 | 24 | evergreen, `leaf_light` 8 | — | 4, autumn → winter |
    | clematis | 43–45 | 16 | shed in winter, yellow in autumn | 5, spring and summer | 3, autumn → winter |
    | grape | 46, 47 | 20 | shed in winter, orange in autumn | — | 8, summer → autumn (demands k) |

    - `vine_stem` is material 40.
    - Each species has `worldgen_chance` 2.
  - Garden: a stone trellis wall along the meadow's south edge (6 high, with a gap at the path), with a vine at its foot every 4 cells. Trees take the corners as before.
  - Tests:
    - `parts::tests::{a_vine_climbs_a_wall_up_first_and_stops_at_its_reach, a_vine_without_support_stays_a_stem, a_vine_hangs_over_a_ledge_only_when_its_row_says_so, vines_flower_in_their_season_and_wilt_after, vine_rows_reject_empty_rules_and_unknown_flower_seasons, an_ivy_seed_by_a_wall_grows_up_it_through_the_seasons}`
    - `mesher::tests::vines_lie_flat_on_the_cubes_they_climb_and_hang_as_crosses`
    - e2e `vines_render` (screenshots in `docs/screens/`)
  - Decisions:
    - test-fix (own commit):
      - The pinned species list ends with ivy, clematis and grape.
      - The oak form helper reads the `Tree` variant.
      - `trees_render` plants only `Tree` forms; vines need a wall.
    - The plan's `flower` field moved from the form to `parts.flower`, so a flowering tree can use it too.
    - "No support within one cell" became "touches a climb material along a face or an edge". The edge rule is what lets a vine crest a wall.
    - Hanging is straight down only. Sideways growth under a ledge still needs support.
    - Clematis needs n 3 and grape's fruit has take 52. Both keep strings unique that existing tests edit once in `plants.ron`.
    - Not done: tendrils that grip other plants' leaves, and vines climbing trees' leaves (they climb wood, so they climb trunks).

### V7. Ecology and balance pass

- A headless report (`report_p2`) gains vegetation stocks per season: cover, biomass, soil NPK, and the limiting-factor histogram.
- Tune for a world that greens in spring, flowers in summer, colours and sheds in autumn, and rests in winter, with soil that runs down under crops and recovers under fallow and legumes (clover, a nitrogen-fixing groundcover).
- **Status: done (2026-10-28).**
  - `rmc_eco::stocks::VegetationStocks::measure` runs after every season. The headless report (`report.json`, `SeasonEntry::vegetation`) and the `--headless` printout carry:
    - plants per species and part cells per kind;
    - cover (columns holding a plant or a part) and the roots' summed biomass;
    - soil nutrients: every substance a species needs, demands or takes up, summed over anchor cells;
    - the limiting-factor histogram. Each plant counts once under the first need it fails (`moisture`, `light`, `temp`, `n >= 2`, `cu <= 16`, ...), else its short demanded substance (V2 sufficiency), else `fed`.
  - Balance: the shipped rates already give the intended year, so no rate changed. The garden (`build_garden`, seed 1) over 8 seasons:

    | Season | Plants | Leaf cells | Flowers | Fruit | Soil n | Held back by temp |
    |---|---|---|---|---|---|---|
    | spring 0 | 6390 | 0 | 0 | 0 | 3.21 M | 0 |
    | summer 1 | 6640 | 218 | 4 | 3 | 3.11 M | 0 |
    | autumn 2 | 7096 | 802 | 0 | 0 | 3.00 M | 0 |
    | winter 3 | 7290 | 433 | 0 | 8 | 2.92 M | 2542 |
    | spring 4 | 7753 | 1601 | 75 | 0 | 2.83 M | 0 |
    | summer 5 | 8282 | 2391 | 76 | 513 | 2.75 M | 0 |
    | autumn 6 | 7292 | 3053 | 0 | 2 | 2.67 M | 0 |
    | winter 7 | 7499 | 1878 | 0 | 56 | 2.62 M | 2369 |

    - Leaves come in spring, flowers open in spring and summer, fruit sets in summer, leaves turn in autumn and drop in the winter tick, and winter holds a third of the plants back by temperature.
    - The grass-heavy garden draws nitrogen down by about 3% a season. Grass short of nitrogen (`n >= 2`) appears from the second autumn.
  - Soil recovery: one year on an 8 by 8 plot (`stocks::tests::soil_runs_down_under_grass_and_recovers_under_clover_and_fallow`). Nitrogen in the loam goes 2560 → 733 under grass, 256 → 3595 under clover, and 256 → 2176 fallow with litter (`decompose`).
  - `plants_render` now runs with the weather plugin, so its "later" shot shows autumn colors (test-fix, own commit).
  - Tests:
    - `stocks::tests::{stocks_count_plants_parts_cover_soil_and_limits, soil_runs_down_under_grass_and_recovers_under_clover_and_fallow}`
    - `cli::tests::headless_seasons_writes_report` (now also checks the vegetation)
  - Decisions:
    - The plan's `report_p2` is the existing headless `report.json`. The stocks are a `#[serde(default)]` field, so older reports still load.
    - Deciduous leaves drop in their `shed` season (winter). They turn in autumn, so on screen autumn colors and winter is bare.
    - Soil counts anchor cells only. Substance deeper in the ground is not what roots see.

### Leaf tuft (owner request, 2026-10-28)

A tree grows its whole trunk before its crown, so a young birch (white bark)
looked like an ice pillar. A tree row with `parts.tuft: true` now carries a
tuft of leaves on its growing tip. The client draws a plant cross in the
tree's leaf color (season color included) in the open cell above the top
trunk cell, or above the ripened root before the trunk grows. The tuft
disappears once the crown covers the tip, and it is hidden in the tree's
`shed` season. It is a look only: the simulation, the save and the ledger
are unchanged. `PlantLook` gains `tuft` (the wood) and `bare` (per season
row). Oak, birch and pine have it. Test:
`mesher::tests::a_growing_trunk_carries_a_leaf_tuft_until_its_crown`; shot:
`docs/screens/trees_render-summer-*.png`.

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
