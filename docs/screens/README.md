# Screens

`scripts/screens.sh [scenario]` copies each E2E scenario's frames here as `<scenario>-<label>-<frame>.png`.

## Trees

- `trees_render-*` (summer, autumn, winter, bare): **the old block trees.** The `trees_render` scenario puts the trees back on the cell-tree forms they had before docs/TREES.md T7 (wood, branch and leaf cells, docs/VEGETATION.md V5), which a `plants.ron` row may still choose, and its test stays (CLAUDE.md rule 6). The shipped trees are graph trees; these shots are not their look.
- `tree_graph_render-*`: **the graph trees** (docs/TREES.md), as the game draws them. One tree of each species stands on a fed bed and grows for four years.
  - Season shots are named `<ticked>-to-<shown>`. A frame shows the cells and tree graphs the host left after ticking `<ticked>` (that season's snow and leaf drop), under the client's weather and leaf colors for `<shown>`.
  - `close`: under the first crown. `evergreen`: the pine from above its crown's base. `forest`: a player's eye height in front of the densest stand of trees that worldgen seeded, grown over the same seasons.
