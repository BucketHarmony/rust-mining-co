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

## Playtest 2026-10-19 at c0d1cd9 (milestone M7: windowed game, overlap safety, lantern, horizon/fog/sky, face shading, hit feedback, cursor grab, glow in a cave)

Method: headless rmc_testkit on WARP (software D3D12), 640x360 unless noted. Nobody has run the windowed game; `cargo run -p rmc_app` needs a display, so F077, F091 and the on-screen feel of mouse look are untested by a human (see "Not possible").

- `scripts/screens.sh` (all scenarios, `CARGO_BUILD_JOBS=8`): 15 of 15 pass, 27 min 30 s. dig_tunnel is the slow one (354 s), then the old playtest_20261018 (287 s). No paging-file link failure this time. The committed docs/screens PNGs came out byte-identical, so they are current and deterministic. The frame numbers in the names are correct now (the M6 stale-name problem is gone).
- `scripts/soak.sh`: now exits 1 with "E2E FAILED: no scenario ran (no test named soak_chaos)". That is the guard the M6 report recommended. It stays red until F080 adds `soak_chaos`, so there is still no soak coverage.
- My scenario: `tests/e2e/tests/playtest_20261019.rs` (`scripts/e2e.sh playtest_20261019`, 555 s, passes). It uses the real 256x128x256 default world, which the M6 playtest did not (it used 64x128x64). It drops in at the spawn column, turns four quarter turns, pitches up and down, walks 2 s, digs topsoil and a placed basalt wall with the real swing input while logging HudState each frame, stands against the wall, places cells on and around the player, then flies up. Shots and log go to `target/e2e/playtest_20261019-<frame>.png` and `.log` (not committed). "pt-N" below means that file. I also re-ran the old playtest_20261018 for a before/after ("old-N" means `playtest_20261018-N.png` from this session).
- Side effect to note: `scripts/screens.sh` with no argument also runs both playtest scenarios and copies their PNGs into docs/screens. I deleted those copies (24 untracked files) so the folder holds only the 14 committed scenarios.

Not possible this session:
- Season play (swale/pit, 3 seasons, Moisture overlay): no `advance_season` in rmc_testkit and no Moisture overlay; that is Phase 2 (F1xx).
- A real window: cursor grab, Escape, click-to-regrab, window title, and the interpolated camera at a real frame rate are covered only by unit tests.
- Place from the mouse or keyboard: still no place control (CONTROLS.md lists only the editor brush, F1). The inventory still only counts up.

### Screens, one line each (docs/screens, file order)
- boot_renders-123: green terrain in the lower third fading through fog to a pale grey-blue haze; the horizon blends into the sky and stepped contour bands read as hills.
- dig_cell_visible-41: a flat brown wall with a dark cell outline, a crosshair and the label "dirt"; no HUD counter.
- dig_cell_visible-68: after the break, a hole with a white outline, lit side faces, a dark back wall, and small brown wireframe chips floating in it; "dirt 1" top-left.
- dig_tunnel-40: a flat green wall (320x180) with outline, crosshair and "topsoil".
- dig_tunnel-1747: looking out of the dug tunnel: dark green walls, a lit green floor patch, and a black square at the end; "topsoil 80".
- editor_brush_sphere-55: flat green ground with a magenta outline on the target cell.
- editor_brush_sphere-63: a sphere crater seen from inside: stepped terraces, each face a distinct shade, soft darkening in the corners. This is the best depth-cue shot.
- emissive_glow-22 and -off-26: a single flat light-blue field, and a single flat darker teal field; still no geometry.
- fly_camera-199: the cave test world from above: brown stone with cyan crystal and orange ore, and now pale grey-blue fogged tubes instead of black ones.
- glow_in_cave-18: a dark grey wall with a light-blue square (the crystal) and a faint halo.
- glow_in_cave-off-22: the same square, darker teal, with no halo.
- horizon_readable-away-from-sun-46: a smooth blue gradient, lighter at the bottom; no clouds or landmarks.
- horizon_readable-sun-42: the same gradient with a white sun disc and a soft glow ring.
- horizon_readable-fog-reference-38: sky above and a flat light-grey fog bank below; terrain emerges from fog only in the bottom 15%.
- horizon_readable-east/north/south/west-86/81/91/96: green stepped hills in the foreground fading to grey in the middle distance, then a flat grey band (about 100 px high) with a hard straight top edge against the blue sky.
- lantern_close_wall-basalt-close-18: a flat mid-dark grey field (about RGB 65,66,72). It is not blown out.
- lantern_close_wall-basalt-no-lantern-34 and -reading-22: darker grey fields; the lantern lifts the wall a little, as it should.
- lantern_close_wall-topsoil-close-26 and -reading-30: flat olive green fields.
- latency_dig-65: the same dirt-wall hole as dig_cell_visible-68, with brown wireframe chips.
- place_and_climb-119: a tan trapezoid (the placed block) in front of a green ground and a dark green wall.
- save_load_roundtrip-world-126, -loaded-world-19, -control-25, -124, -loaded-27: a diagonal light/dark green split with a tan wedge at the bottom and a dark diamond pit; the loaded frame matches the saved one. It is hard to read as a place, but it proves the round trip.
- step_up-595: a one-cell green ledge with a thin outline and "topsoil"; the ledge faces are clearly shaded.
- walk_and_jump-190: green floor and steps with a near-black slab above them (the 64-cell test world's shell).

### What improved since the M6 report
1. Lantern blow-out is fixed (M6 problem 1). Basalt at 1.4 m is now a mid-dark grey (pt-331, pt-397). At 0.2 to 0.3 m it is still grey with a legible label (pt-485, old-722 and old-842), where the M6 shots were near-white lavender. dig_tunnel-1747 now has dark walls.
2. The horizon is no longer a black void in the real world (M6 problem 2). In the default world the sky has a gradient and a sun, distance fades to fog (boot_renders-123, horizon_readable-*, pt-97 to pt-129), and pt-912 from the fly camera shows a readable landscape.
3. Terrain has depth cues now (M6 problem 3). Top faces, sides and the lit/shaded sides of a step have three clearly different shades (pt-182, pt-242, editor_brush_sphere-63, old-602, step_up-595). I could judge a 1-cell step and the depth of a pit at a glance, which I could not in M6.
4. The player-in-block white-out is fixed (M6 "feels broken"). A raw `Set` on the player's own body cell was dropped by the host (cell stayed air, penetration []). A box `Set` around the player filled the surrounding cells and spared the body's cells (pt-782: a new step of topsoil appears in the notch while the player is not embedded). Old-842, which was near-white, is now readable grey.
5. Hit feedback exists (M6 "weightless"). Every swing gives a crosshair and outline pulse: the outline turns amber and about 10% larger, and the crosshair grows (pt-331 vs pt-353). `HudState.swing_pulse` reads 1.0 on the swing and falls 0.1 per frame to 0 (about 10 frames, 0.17 s). A break sets `last_break` to fade 1.00 and drops it over about 55 frames. A topsoil swing (1 hit) logged pulse 1.0 and break 1.00 on the same frame; topsoil goes to the inventory ("topsoil 1") at once.
6. Glow is shown against geometry (M6 problem 5): glow_in_cave shows crystal against dark stone, and the on/off pair differs visibly (glow_in_cave-18 vs -off-22).
7. The scenarios now save failure screenshots, committed frame names are fresh, and the soak script refuses to pass with zero tests.
8. Everything else from M6 still holds: walk 8.89 m in 120 frames (4.3 m/s) on the real terrain, auto step-up (+2.5 m over a 9 m walk with no snag), a clean landing from 3 m (feet y 48.00, no bounce), and 0 penetration frames in about 900 frames.

### What looks wrong (with screenshot names)
1. The fog bank has a hard, perfectly straight top edge. In every level view of the default world (pt-97 to pt-129, horizon_readable-east/north/south/west, boot_renders-123) a flat grey band meets the blue sky along one horizontal line at the same pixel row. Pitching up moves the line, but it stays razor-sharp (pt-132). It reads as a grey box wall around the world, not as haze. The sky gradient and the fog color do not meet at the horizon, and the edge is the top of the world shell. A fog-to-sky blend over the last 10 to 20 degrees would remove it.
2. The break effect is a wireframe, not chips. In dig_cell_visible-68, latency_dig-65 and pt-725 the "chips" are thin line cubes in the broken material's color. They read as a debug gizmo, and next to the shaded terrain they look out of place. In pt-725 (standing 0.2 m from the broken cell) the wireframe fills the bottom of the screen, and two white horizontal lines run across the whole view, over the dark side walls too. These look like the outline or flash drawn without depth testing at close range. The amber flash outline also changes size (pt-331) in a way that suggests the outline is a 3D box viewed from inside.
3. Close to rock, the cell outline collapses into a screen-wide line (pt-485, old-722, old-842) and carries no information. The "basalt" label floats on a flat grey field with no sense of cell boundaries. A wall of a single material has no cell grid, no cracks and no per-cell variation, so damage shows only in the small bar (pt-331, pt-397).
4. Surfaces are flat colors. All topsoil is one green and all basalt is one grey, face-on at any range (lantern_close_wall-*, pt-309, pt-485). Face shading helps at edges but not on a big face-on wall. The tunnel (dig_tunnel-1747) still ends in a hard black square.
5. Small test worlds (64 cells wide) still show the near-black slab above the ground in walk_and_jump-190 and step_up-595. The fog and sky only work in the default world, so test shots from the small worlds are misleading for judging the look. I suspect the shell is the cause there too.
6. The glow does not read as glow. glow_in_cave-18 is a pale-blue flat square with a barely visible halo, and emissive_glow-22/-off-26 are still single flat colors with no geometry at all (the scenario name promises more than it shows). The assertions pass on luminance margin (106 vs 38), but a player looking at the crystal sees "a lighter blue block", not light. There is no light spill onto the neighboring stone.
7. A tiny dark-blue dash sits at about (150,180) in pt-113 (and horizon_readable-south-91). It is at the horizon, in the same spot in both, so it may be a sub-pixel gap in the shell or a stray triangle. I could not identify it.
8. The sky has no features beyond the sun: no clouds, no stars, and the sun is not visible while level or pitched up 80 degrees from the spawn (pt-132, pt-135). Looking at the sun gives a ragged-edged disc (horizon_readable-sun-42), which is fine for now.

### What feels broken as a player
- Nothing can be spent or placed. The inventory shows "topsoil 1, basalt 2" and does nothing; the only way to put a block down is the F1 editor brush. The M7 overlap-safety work protects a feature the player cannot use. This is the largest gap between the game as it exists and a game.
- Digging basalt takes 6 swings, one every 20 frames, so about 2 s per cell, with a 10-frame pulse that has ended by the next swing. It works, but beyond the bar and the pulse the cell itself does not change until it breaks, so progress is hard to see on a face-on wall (pt-331 to pt-397). When the cell breaks at close range, the wireframe fills the view (pt-725).
- A wall one cell thick is not a tunnel. After 3 cells (about 4 s in total on basalt) I walked straight through into open air (pt-749). That is expected, but it also means the "inside a tunnel" look (dark walls, light at the end) was not tested here; dig_tunnel-1747 is the only evidence.
- The world edge and the grey band are the only "end of the world" signal, and the band looks like a wall with a ceiling (see "looks wrong" 1).
- The mouse-grab flow (Escape to release, click to regrab, F1 to release) has never been exercised against a real OS window. The look code is gated on a `CursorGrab` resource, so a missing `CursorPlugin` would silently disable look in some builds. The editor also stops mouse look while open (documented), so aiming the brush needs close F1, aim, open F1, which is awkward.
- Still no coyote time or jump buffer (carried over from M6, unchanged).

### Prioritized issues
1. (High) No place/spend control: build F071/F074 (place a block from inventory against the targeted face, with the host's overlap rule). Without it, no loop of dig, carry, place exists, and the M7 overlap fix cannot be played.
2. (High) Hard top edge of the fog band (pt-97 to pt-132, horizon_readable-*). Blend the shell and fog into the sky gradient, with no seam, so the horizon reads as haze.
3. (High) Hit feedback at close range: draw the break chips as solid small cubes with depth test on (or billboards), and stop drawing the outline and flash through walls (pt-725, dig_cell_visible-68). Show damage on the cell itself (a darkening or crack tint) so progress is visible on a face-on wall.
4. (Medium) Run the game once in a real window and record it: cursor grab, Escape, regrab, F1 flow, the window title. Add a headless-build check that `CursorPlugin` is not required for look in the test kit but is in the windowed build.
5. (Medium) Surface variation: per-cell color jitter (a few percent) or a cheap noise so large faces and walls read as stone and soil, not paint (lantern_close_wall-*, pt-485). This also fixes the "label floating on flat grey" problem.
6. (Medium) Close-range outline: when the targeted cell is closer than about 0.4 m, draw the outline as a thin frame on the target face or drop it, so it does not become a screen-wide line (pt-485, old-722).
7. (Medium) F080 `soak_chaos`, so `scripts/soak.sh` has something to run. It correctly fails now, but that means no soak gate.
8. (Low) Make glow read as light: a little halo or spill onto adjacent cells, and rework emissive_glow-22/-off-26 to show a crystal beside rock, or remove the scenario (glow_in_cave covers it).
9. (Low) The black square at the end of dig_tunnel-1747 and the near-black slab in small test worlds: make the shell the same fog color everywhere, so test shots are representative.
10. (Low) The speck at (150,180) in pt-113; coyote time; scripts/screens.sh should skip the playtest_* scenarios, which add 24 stray PNGs to docs/screens and 10 minutes.
