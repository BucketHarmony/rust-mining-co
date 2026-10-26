# Tiles

Chunk faces are drawn with tile textures: one square image per material and face direction, repeated once per cell (0.5 m). The tiles are read by the client only (`rmc_client::tiles`). They are not part of the material table or its hash, so changing them never breaks a save or the host.

## Files

- `assets/tiles.ron` maps a material key to its images:
  - `all` for every face, or `top`, `side` and `bottom`.
  - `bottom` falls back to `all`, then to `side`.
  - `size` is the edge length every tile is resized to (a power of two, 128 now).
- `assets/tiles/*.png` holds the images. Any size and any PNG color type works; each one is resized to `size` and mipmapped when it loads.
- A material that is not listed, or whose file is missing or does not decode, keeps its flat color. A warning names the file. Art can be added one tile at a time.
- Materials that are not tiled: `air` and `water` (water is drawn in the translucent liquid pass), and anything past material id 255.

The files shipped now are placeholders from `scripts/placeholder-tiles.py`. That script never overwrites an existing file unless it is given `--force`.

## What a tile must be

- **Seamless.** It repeats edge to edge on every cell. Check with `python scripts/tile-preview.py assets/tiles/<name>.png`, which writes a 3 × 3 repeat to `target/tile-preview/`.
- **Evenly lit.** The engine adds the light, the face shading and the corner shadows. A tile with a lit side or cast shadows looks wrong on faces turned the other way. Aim for flat, diffuse "albedo" lighting.
- **The material's color.** The tile replaces the flat color, so keep it close to the `color` in `assets/materials.ron`. That color is still what the HUD, the editor and untextured faces use.
- **Up is up on side tiles.** The top edge of a `side` image is the top of the cell. That is how grass overhangs the dirt on `topsoil_side.png`.
- **sRGB PNG.** Alpha is ignored; faces are opaque.
- **Glow.** Glowing materials (`glowcrystal`, `magma`) glow in the tile's own colors, scaled by the material's `emissive`.

## Making tiles in ComfyUI

1. **Generate square at 1024 or 512.** The game scales down to 128, so detail finer than about 1/128 of the image is lost. Bold, readable shapes work better than fine noise.
2. **Make it tile.** Use a seamless method: circular (wrap-around) padding on the model's convolutions, e.g. a "seamless tiling" or "asymmetric tiling" custom node, or a tiled sampler. Or generate normally and fix the seams with an inpainting pass over a half-offset copy.
3. **Prompt for a texture, not a scene.** Something like:
   `seamless tileable texture, <material>, flat even lighting, no shadows, top-down orthographic, game texture, albedo, high detail`
   and a negative like `perspective, shadow, vignette, border, frame, text, watermark`.
4. **Keep a set consistent.** Use one model, one sampler and similar wording for the whole set, so the stones look like they belong to one world.
5. **Save** as `assets/tiles/<name>.png` and list it in `assets/tiles.ron`.
6. **Check** with `scripts/tile-preview.py`, then in the game: run `cargo run --release -p rmc_app`, press `F1` to open the editor, then `F7` to reload `tiles.ron` and every image without restarting.

### Suggested subjects

| Material | Faces | Subject |
|---|---|---|
| `topsoil` | top / side / bottom | short grass and clover from above; side: dirt with a ragged grass fringe along the top edge; bottom: dirt |
| `dirt` | all | brown earth, small pebbles and roots |
| `clay` | all | smooth reddish clay, fine cracks, faint layers |
| `stone` | all | grey rock face, chisel marks, small fractures |
| `granite` | all | pink-grey granite, coarse speckled crystals |
| `basalt` | all | dark blue-grey basalt, columnar joints, vesicles |
| `limestone` | all | pale cream limestone, sedimentary bands, tiny fossils |
| `coal_seam` | all | black coal seam, glossy facets |
| `sulfide_ore` | all | rock with brassy copper-sulfide veins and crystals |
| `iron_ore` | all | rusty red-brown rock with dark iron nodules |
| `glowcrystal` | all | cyan crystal cluster in dark rock (it glows in the tile's colors) |
| `bedrock` | all | very dark, dense, fractured stone |
| `brass_plate` | all | riveted brass panel, steampunk, worn edges |
| `ice` | all | blue-white ice, trapped bubbles and cracks |
| `obsidian` | all | black volcanic glass, conchoidal fractures |
| `magma`, `magma_source` | all | glowing orange-red molten rock with a dark crust |
| `crushed_sulfide` | all | gravel of crushed brassy ore |
| `copper_dust`, `iron_dust`, `salt_dust` | all | heaped powder: green-blue copper, grey-black iron, white salt |
| `ochre_crust`, `salt_crust` | all | orange-yellow iron-oxide crust; white crystalline salt crust |

## Later

- Normal and roughness maps per tile (ComfyUI can estimate normals from an image) would let light catch the surface relief. The shader would need two more texture arrays and tangent frames per face. The layout of `tiles.ron` leaves room for that.
- Several variants per tile, picked per cell from the cell hash, would hide repetition on large faces.
