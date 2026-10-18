## Playtest 2026-10-18 at 78df49b (milestone M6: controller, interpolation, pickaxe, HUD, inventory)

Method: headless rmc_testkit on WARP (software D3D12), 640x360 unless noted.

- Re-run this session: walk_and_jump (pass, 61 s), dig_cell_visible (pass, 42 s), latency_dig (pass, 33 s).
- Not re-run: step_up, dig_tunnel (about 6 min), boot_renders, emissive_glow, fly_camera. For these I read the committed PNGs. PROGRESS.md says all 8 scenarios passed.
- Fresh walk_and_jump output looks the same as the committed PNG, but the frame number in the file name differs (190 vs 195). The same is true for dig_cell_visible (41/68 vs 43/71) and latency_dig (65 vs 67). The committed names are stale, but the images match.

My own scenario is `tests/e2e/tests/playtest_20261018.rs` (run: `scripts/e2e.sh playtest_20261018`, about 270 s, passes). It does the following:
- drops in on the real generated terrain and looks around;
- walks, digs a hard wall and a pit, and places cells with raw edit commands;
- embeds the player in a block.

Its screenshots and log go to `target/e2e/playtest_20261018-<frame>.png` and `target/e2e/playtest_20261018.log`. They are not committed. Below, "pt-N" means `playtest_20261018-N.png`.

Not possible this session:
- `scripts/soak.sh` runs `soak_chaos`, which does not exist yet (F080). Cargo runs 0 tests and exits 0, so a green soak means nothing.
- Season play (swale, pit, Moisture overlay): there is no `advance_season` in rmc_testkit and no Moisture overlay. That is Phase 2.
- `docs/CONTROLS.md` does not exist (F070). I read the controls from the source: W/A/S/D move, Shift sprints, Space jumps, left mouse swings the pickaxe, V toggles the fly camera (Space up, Ctrl down).
- `rmc_app/src/main.rs` is a 6-line stub (F077), so nothing has been played in a window.

### Screens, one line each (docs/screens, file order)
- boot_renders-129 and -130: a rolling green voxel landscape with a flat light-blue sky above a dark slate wall on the horizon. The two images are identical.
- dig_cell_visible-43: a flat tan "dirt" wall filling the screen. A thin dark outline marks the targeted cell, with a crosshair and the label "dirt". There is no inventory yet.
- dig_cell_visible-71: the same wall after the dig. A one-cell hole shows a flat dark-grey backdrop with the hole's side faces lit. The HUD top-left reads "dirt 1".
- dig_tunnel-40: a flat green wall at 320x180, with a target outline, a crosshair and the label "topsoil".
- dig_tunnel-1747: looking back out of the dug tunnel. The walls turn pale towards the right, and a dark square at the end is the world shell. The HUD reads "topsoil 80".
- emissive_glow-23: a plain light-blue field, nothing else.
- emissive_glow-off-27: a plain darker teal field. The pair shows a color change, not a glow.
- fly_camera-199 and -205: the cave test world from above. Brown stone islands with cyan crystal and orange ore patches float over black tubes. The two are nearly identical.
- latency_dig-67: the same dirt-wall hole as dig_cell_visible-71, framed a little higher.
- step_up-611: a close-up of green ledges and a floor. No HUD or crosshair is visible.
- walk_and_jump-195: a green floor with stepped blocks. Behind it is a near-black slab with a diagonal corner.

### What I saw in my own playtest
- pt-62/67: dropped 3 m onto the hills, landed grounded at y 49.00 with no bounce and no penetration.
- pt-122/131/182/195: quarter turns are exact (yaw -1.57, -3.14). Hills fill the lower half of the view and a near-black wall the upper half.
- pt-198: looking fully up shows a featureless flat blue field with no sun or cloud, so nothing gives a sense of direction.
- pt-201: looking fully down shows the targeted topsoil cell with an outline and the label "topsoil". The pitch limit is plus or minus 1.55 rad.
- pt-242/302: walking uphill. The player steps up ledges automatically. It covered 8.89 m in 120 frames (4.3 m/s) and gained 0.5 m without a snag.
- pt-335/367/399/434: a basalt wall dug by holding the button.
  - The HUD damage bar fills in tenths of 60, one step per swing (every 21 frames).
  - 5 of 6 swings landed in 90 frames.
  - The bar is a thin yellow strip under the material name, and it does not cover the name.
- pt-482/506/542/602: a 1-cell pit and its neighbors dug under the feet.
  - The pit has clean edges, and the counter reads "topsoil 1", then "topsoil 3".
  - The player did not fall in. Its 0.6 m box spans three 0.5 m columns, so it bridges a 1-cell hole.
- pt-722/782/842: right up against a placed wall (0.2-0.3 m), the whole screen is blown out to near-white lavender, with the "basalt" label floating on the pale field.
- pt-893: after backing out, the view is normal again.

### What works
- Walk, sprint, jump and mouse look are tight and deterministic.
  - Walk speed is 4.3 m/s and jump height is 1.2 m.
  - Pitch clamps cleanly with no flip, and the yaw sign is consistent.
  - The same pose reached twice gives the same image (pt-67 and pt-323 in the first run).
- The player never overlapped a solid cell by itself: 0 penetration frames in about 900 frames of walking, stepping, jumping and digging.
- Auto step-up over 0.5 m ledges is smooth. A 1.0 m (2-cell) wall blocks the player, as designed.
- Digging:
  - One swing breaks topsoil, and basalt takes 6 swings (2.1 s).
  - The outline, material name and damage bar all appear.
  - Inventory counts are right ("dirt 1", "topsoil 3", "topsoil 80"), and host and replica agree.
  - The hole shows within 1-2 steps of the breaking swing.
- Interpolation: in walk_and_jump the camera stayed between the last two physics eyes on every frame.

### What looks wrong (with screenshot names)
1. The head lantern blows out anything close.
   - Basalt has a base color of (62,62,70) but renders as pale lavender-grey at 1.4 m (pt-335, pt-399).
   - At 0.2-0.3 m it is near-white (pt-722, pt-842), and the label and crosshair are hard to read.
   - The same effect washes out the right-hand wall in dig_tunnel-1747, where tunnel walls should be the darkest thing on screen.
   - The probable cause is the point light's inverse-square falloff plus HDR bloom.
   - A face-on view of rock is the main view of this game, and it is the least readable one.
2. The world shell is a near-black wall with no fog, texture or edge.
   - In pt-122 to pt-195, pt-302, walk_and_jump-195 and fly_camera, the horizon is an RGB-25 slab that reads as a void, not a sky. The sky shows only when you pitch up.
   - This is the 64x128x64 test world. I could not check the default Phase 1 world.
3. The surface has very low contrast. Topsoil is a single green, and only a slight face shade separates top from side (pt-195, step_up-611). Step height and distance are hard to judge.
4. A hole through a one-cell wall shows a uniformly flat dark rectangle (dig_cell_visible-71, latency_dig-67), with no depth cue.
5. emissive_glow-23 and -off-27 are single flat colors with no geometry, so they do not demonstrate glow. A glow review needs a shot with the crystal next to dark stone in a cave.
6. step_up-611 shows no HUD or crosshair, so I cannot tell what it demonstrates; it looks like a camera-only view.

### What feels broken as a player
- You can place a block inside yourself.
  - A raw Set edit over the player's head was accepted by the host. The player was left penetrating cell (32,102,52).
  - The camera ends up inside the block and the screen goes white (pt-842).
  - Nothing pushes the player out. I walked out by backing up (S), which worked.
  - When F071 adds a place control, the host must reject or resolve an overlap with a player's box.
- One-swing materials give no break feedback. Topsoil (hardness 10, damage 10) vanishes at once, with no swing animation, particles or sound, so digging feels weightless.
- A single-cell pit does not drop you in. This is explicable, but nothing says so. The player box is 0.6 m wide against a 0.5 m cell, so you stand on its lip and cannot dig down under yourself.
- Nothing can be placed from the keyboard or mouse yet (F071, F074), so "build a bridge" and "climb by placing" are untestable. The inventory counts up but nothing spends it.
- The M6 work is reachable only through the test kit.
  - There is no CONTROLS.md and no cursor grab: nothing in rmc_client sets CursorGrabMode or handles Escape.
  - `rmc_app` spawns no player, so a human opening the game today would see nothing.
- Held Space bunny-hops on landing. That is standard, but there is no coyote time or jump buffer.
- The world edge is invisible. Walking toward the shell with the view pitched up (my first run) ended at a blank dark-grey screen, with nothing saying "this is the edge".

### Top 3 fixes within Phase 1 scope
1. Fix close-range lantern blow-out: soften the point light (a larger radius, a minimum-distance offset, or lower LANTERN_LUMENS) so a 0.2 m wall is no brighter than a 1.5 m wall. Re-shoot dig_tunnel-1747 and add a pixel assert to dig_tunnel that the center block's luminance stays under a ceiling, not only that it changed.
2. Make the world edge and sky readable: tint or fade the shell with distance, and give the sky a sun direction. Re-shoot walk_and_jump, boot_renders and fly_camera so the next playtest can judge depth. Add some color noise or ambient occlusion to terrain faces for the same reason.
3. Reject or resolve edits that overlap a player's box on the host (F071 `Set`), with a test where a Set over the local player is rejected. Also add a hit effect (flash, chip particles or a short camera kick) for one-swing materials. Finally, make `scripts/soak.sh` fail when no test ran (or add `soak_chaos`, F080), so a green soak means something.
