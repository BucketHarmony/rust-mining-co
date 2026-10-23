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

## Playtest 2026-10-19 at 105d692 (milestone M8: soak_chaos F080, criterion benches F081, performance pass / initial-load burst / mesh memory in stats panel F082, CI F083)

Method: headless rmc_testkit on WARP (software D3D12), 640x360 unless noted, the real 256x128x256 default world. Nobody has run the windowed game. Compared against the M7 report above (c0d1cd9). "pt-N" means `target/e2e/playtest_20261020-N.png` (not committed). The log is `target/e2e/playtest_20261020.log`.

### Results
- `scripts/screens.sh` (no argument, `CARGO_BUILD_JOBS=8`): 17 of 17 scenarios pass, about 52 min of scenario time. Slowest: soak_chaos at 3,600 frames (1,031 s), playtest_20261019 (534 s), dig_tunnel (345 s), playtest_20261018 (282 s). No scenario failed and no failure screenshot was written. The log has 36 "waiting for the GPU: Wait timed out" WARNs from the screenshot readback; the capture retried every time and none failed. All 35 committed PNGs in docs/screens came out unchanged (git shows no modified tracked file), so M8 changed nothing a player sees in those scenes.
- `scripts/soak.sh` (seed 17247, 3,600 frames) passes in 960 s. `soak_chaos` also passed inside screens.sh with its default seed 1. Recorded step times on WARP at 320x180 (the scenario records them and does not assert):
  - seed 1: p50 278.40 ms, p95 373.36 ms, max 456.77 ms; 82 actions (Walk 17, Look 16, Dig 16, Place 17, Jump 16), 38 swings, 35 brush clicks, 16 jumps, 0 penetrating frames.
  - seed 17247: p50 267.02 ms, p95 351.57 ms, max 449.29 ms; 90 actions (18 of each), 44 swings, 38 brush clicks, 18 jumps, 0 penetrating frames.
  - Both are "over target" against 33 ms. That is the software rasterizer, not the game: the step includes the render. The number cannot show a CPU regression. The scenario does assert that host and replica hash the same after the drain, that the mesh queue drains, and that the world changed; all held on both seeds.
- My scenario: `tests/e2e/tests/playtest_20261020.rs`, run with `scripts/e2e.sh playtest_20261020` (539 s, passes). It boots and logs the initial load, drops in at the middle column, turns four quarter turns, opens F1 and toggles F2/F3/F4, walks 2 s and sprints 2 s, jumps, swings at topsoil, places with the F1 brush by mouse click, digs a shaft straight down, and flies up. My first draft used BuilderBot and TunnelBot in the natural terrain. Both refused ("... is not an air cell on a solid one", "no solid cell within 32 cells ahead"): those bots need flat ground or a wall, which only the test courses have. I replaced them with direct input.
- Not possible: season play (no `advance_season` in rmc_testkit, no Moisture overlay; Phase 2), a real window, and viewing the egui panel (see below).

### M8 features as a player meets them
- Initial load (F082): `initial_load_done` after 33 steps, with exactly 8 uploads per frame from step 2 to 33 (0, 8, 8, ... 8). That is the in-flight cap, as the F082 notes say, not the 32 budget. 256 meshes, 0 discarded, 0 queued, 0 awaiting at the end. It never dropped to the post-load budget of 4, so the early-end race is not visible. 41 s wall on WARP.
- Mesh memory: `mesh_bytes` 58,361,672 (55.7 MiB), 255 chunks with geometry, 317,183 quads. Identical to docs/perf.md, so the record is reproducible. After my edits (a handful of cells) it was 58,370,136 with 273 uploads.
- Stats panel: the panel cannot be seen in any screenshot. pt-143 (F1 open) is pixel-for-pixel the closed view (pt-97), and pt-625 (editor open, four clicks) equals pt-640 (editor closed). The egui layer is not in the headless capture, so the "Mesh memory: N MiB" line is covered only by the unit test. F1 itself works: `editor.open` went true, then false. The gizmos do draw: pt-161 shows a cyan/magenta box on the brush target and one thin white vertical line from the top edge (the chunk-bounds gizmo), over the scene.
- Brush by mouse click (the only place path): four clicks with F1 open, Set and Cell chosen by keys 1 and 4, each changed the host world hash. One brass cube shows in pt-625 and pt-640 (the orange cube at the right edge). It works, but the editor has to be open, which stops mouse look.
- CI (F083) is outside what I can play; the screens run's pass/fail is the local equivalent.

### A. BROKEN (bug, crash, failing scenario, camera in a wall, flat grey, a control that does nothing)
1. A one-cell hole cannot be descended, so "dig down" does nothing for movement. In the log, swings straight down (pitch -1.55) broke topsoil, dirt, dirt (inventory topsoil 1, dirt 2; the target cell went y 103, 102, 101) while the feet stayed at y 52.00, `grounded true`, for the whole shaft (pt-670, pt-770). The player is 0.6 m wide (`PLAYER_WIDTH_M`) and a cell is 0.5 m, so the box rests on the cell rims either side of the hole and a hole one cell wide holds nothing. The player hovers over a 1.5 m pit and looks at the surface (pt-864, "level view from inside the shaft", is the top-of-ground view). TunnelBot already knows this (`width_cells()` is 2). A player digging down will think the pickaxe is broken, since the only feedback is the inventory counter. Fix within Phase 1: make the player box narrower than a cell (for example 0.45 m), or have a dig-down swing also take the cell(s) the box overlaps, or let the player slide into the hole.
2. The fly camera (V) can leave the world and then shows a single flat grey. After Space with Shift held for 120 frames (pt-722 and pt-749 in my first run, pt-1113 in the final run) the view is a uniform grey-blue (about RGB 156,172,184) with only the crosshair and HUD text. Sixty frames up (pt-983, pt-662) still shows readable terrain, so the fog hides the ground once the camera is beyond the fog distance and above the shell. Nothing in the view says where the world is. Fix within Phase 1: clamp the fly camera to the world bounds plus a margin, or fade the fog out with height.
3. Nothing crashed, no scenario failed, the player was never inside rock (0 penetration frames in about 1,100 scenario frames and 7,200 soak frames), and host and replica hashes agree after both soaks.

### B. Look/feel wishes and missing features
Still standing from M7 (same shots, unchanged):
- No place/spend control. CONTROLS.md still lists only the F1 brush. The inventory (topsoil 1, dirt 2) only counts up. Largest gap. (M7 #1)
- The fog band has a hard top edge. New evidence that it is geometry: looking along a diagonal the edge tilts. In pt-864 (yaw -4.54) it runs from y=45 at the left to y=28 at the right, which is a corner of a box, not haze. Same edge in horizon_readable-east/north/south/west, boot_renders-123 and pt-97. (M7 #2)
- Break feedback is still a wireframe cube in dig_cell_visible-68 and latency_dig-65. Not re-shot at close range this time. (M7 #3)
- Surfaces are flat colours: lantern_close_wall-*, the steep-down shots (pt-536 is one green field), and the shaft wall at pt-670. (M7 #5)
- Glow still reads as a lighter flat colour; emissive_glow-22 and -off-26 are still full-screen single colours with no geometry. (M7 #8)
- Black end of the tunnel and black shell in small worlds: dig_tunnel-1747 and walk_and_jump-190. (M7 #9)
- The dark-blue dash at about (150,180) is still in horizon_readable-south-91. (M7 #10)
- `scripts/screens.sh` still runs the playtest scenarios and leaves their PNGs in docs/screens. (M7 #10)

New this time:
- The mining HUD is silent when a swing has no target. Standing on a ledge and looking 20 and 40 degrees down, `target` was None (ground beyond the 3.0 m reach) and two swings gave no label, no outline and no counter change (pt-498, pt-536). A "too far" hint or a dimmed outline would explain it.
- The HUD target label disappears while F1 is open, even though a click still changes the cell (`target None, world changed true`, four of four). With the editor open you can see what you will change only through the gizmo (F3).
- A dug shaft looks good from above (pt-770): a lit near wall and a dark floor give depth.
- soak p95 is not a performance signal on WARP (350 to 370 ms). A CPU-only timer around the sim and mesh systems, separate from the render, would make the number comparable to the 33 ms target.
- Walk and sprint feel right: 22.0 m in 240 frames (2 s walk, 2 s sprint), +4.0 m of climb with no snag over rolling terrain; landing from 3 m was clean (feet y 48.00 at frame 97).

### What changed since M7's "Prioritized issues"
- #7 (F080 soak_chaos): done. It runs, passes on two seeds, and the soak script has something to run. Its p95 reads "over target" on WARP, which is expected and not a finding.
- #1, #2, #3, #5, #6, #8, #9, #10: unchanged (see B). #4 (run once in a real window): still impossible from here.
- New: A1 (cannot descend a one-cell hole) and A2 (fly camera blank above the world).

### Top 3 fixes within Phase 1 scope
1. A1: let a dug hole be entered (narrower player box than a cell, or dig-down takes the overlapped cells).
2. M7 #1: a place/spend control from the inventory against the targeted face, using the host's overlap rule.
3. M7 #2 and A2: blend the fog and shell into the sky so the world no longer ends in a straight line, and clamp the fly camera or fade fog with height. Both come from the same fog and shell setup.

### Files added to docs/screens by `scripts/screens.sh` (untracked; please delete)
All 57 are `docs/screens/playtest_20261018-*.png` (24 files) and `docs/screens/playtest_20261019-*.png` (33 files). No playtest_20261020 PNG was copied there (it was run with `scripts/e2e.sh`, so its shots are only under target/e2e). The one new source file is `tests/e2e/tests/playtest_20261020.rs` (untracked until you commit it).

## Playtest 2026-10-19 at 0433f7b (M8 re-check after F085: docs/REPORT.md and scripts/report.sh)
Scope: read docs/REPORT.md as the owner would, check its links, play instructions and claims against the repo, CI and PROGRESS.md; then re-check that the game boots and plays. The full M8 playtest is the section above (F084, playtest_20261020.rs). No new scenario, no screens.sh run. The only code change since F084 is scripts/report.sh plus the generated report.

What works:
- All 35 `![...](screens/...)` links resolve to existing PNGs in docs/screens, and the gallery lists every committed PNG (35 of 35). The links are relative to docs/, so they render on GitHub and in a local viewer.
- The report is easy to scan at the top: "68 / 128 features passing", a per-milestone table (M0-M8 all full, E0-E5 listed by id), a per-feature table with test names, the perf table, then decisions. The perf section matches docs/perf.md (mesh_chunk 1.68 ms, gen_world 241 ms, host_tick_100 61 us, mesh memory 58.4 MB).
- The play commands are right for the code. `cargo run --release -p rmc_app -- --seed 42` matches the `--seed` flag in cli.rs. `assets_dir()` finds `assets/` next to the cwd and falls back to the workspace, so the CI artifact exe works from its own folder. ci.yml's `windows-release` job packages exactly `rust-mining-co.exe`, `assets/` and `CONTROLS.md`, as the report says. The latest main run (39331584563, at d364abf) is green in all five jobs and its `windows-release` artifact is live (34 MB, not expired).
- Game still boots and plays: `scripts/e2e.sh boot_renders` passed (92.8 s on WARP; the shot is the foggy green terrain, no regression from boot_renders-123.png) and `scripts/e2e.sh walk_and_jump` passed (78 s; green ledge with crosshair, black shell at the top edge as already known). Both ran on the real device path, no new warnings except the usual "waiting for the GPU" timeout from the software adapter.

What looks wrong (with screenshot names):
- Nothing new in the screenshots. The gallery images are the same ones judged in the M7 and M8 reports (fog band edge in boot_renders-123.png, black shell in walk_and_jump-190.png and dig_tunnel-1747.png). The report embeds them with no captions, so an owner cannot tell which scenario a shot belongs to or what to look for, and the 35 images are shown full size in one long scroll (lines 172-208).

What feels broken as a player (report-reading edition):
1. BROKEN: the one real owner action is missing from the report. PROGRESS.md F005 has `Blockers:` on its own line followed by an "OWNER ACTION" bullet (verify-features.sh --all fails on native Windows because jq.exe writes CRLF). report.sh treats an empty text after `Blockers:` as "none" (the regex `^[[:space:]]*(none\.?|-)?[[:space:]]*$` matches empty), so every multi-line Blockers section is dropped. `grep -n "OWNER ACTION" docs/REPORT.md` finds nothing. The report's "Decisions and blockers" section therefore shows no blocker at all, and the owner sees a clean bill while there is a known open item. The same F005 Decisions text says the item is "left for the owner".
2. The clone line cannot work for "anyone". The repo is private (`gh repo view`: PRIVATE). The script strips `BucketHarmony@` from the URL on the theory that this makes it work for anyone; the owner needs GitHub credentials either way, and the artifact download also needs a login. The text should say "private repo, sign in first" or leave the user in the URL.
3. The report is stale by two commits the moment it lands: "Generated ... at commit 1bd70d0", HEAD is 0433f7b. It lacks the F085 block (its own Decisions) and the last two commits. PROGRESS.md records this as intended, but the headline "68 / 128" is already as current as it will get, and an owner reading "Generated 2026-10-20T00:03:30Z" on the evening of 10-19 will think the clock is wrong (it is UTC; the local time was 20:03).
4. The headline "60 not passing" reads as failure. The Phase 2 backlog (E0-E5) is counted with the same weight as a broken Phase 1. The per-milestone table shows it, but the first line should say "Phase 1 (M0-M8): 68 / 68; Phase 2 (E0-E5): 0 / 60, not started" so the owner does not open the report worried.
5. Noise buries the signal. The "Decisions and blockers" section is 700 lines (lines 250-1000); 34 blocks repeat "Blockers: none new (the Windows CRLF issue ... still stands)". The three things an owner needs in the morning (open blockers, playtest "BROKEN" items A1/A2 from the M8 report, CI status) are not in the first screen. The playtest report is only a link; its two items marked BROKEN (cannot descend a one-cell hole; blank grey fly camera above the world) are not mentioned, nor that F084 closed both as "designed behavior".
6. Local main is ahead of origin by 6 commits (F083 follow-ups through F085). The artifact the report points at was built from d364abf, so it lacks F084/F085 changes; harmless today (only scripts and docs changed) but the report does not say which commit the artifact is from.

Top 3 fixes within Phase 1 scope:
1. scripts/report.sh: keep a Blockers section when its first line is empty and bullets follow (test for text on the following lines, not the rest of the same line), and drop the "none new (...)" repeats so the F005 OWNER ACTION appears under a "Blockers" heading near the top.
2. scripts/report.sh: add a short "Read this first" block under the headline: Phase 1 vs Phase 2 counts, open blockers, latest CI result and its commit, and the open playtest items with a link to docs/PLAYTEST.md. Fix the "works for anyone" clone line (private repo) and add the HEAD at generation time as UTC plus local.
3. scripts/report.sh: collapse the Decisions section (`<details>` per block) and give each screenshot a caption with its scenario name, or show a short curated gallery (8 to 10 shots) with the rest as a link list.

## Playtest 2026-10-21 at fc8cfb5 (milestone E3b: gas drift, venting, emission, ignition, gas tint; F151-F156)

Method: headless rmc_testkit on WARP (software D3D12), 640x360, a 64 x 128 x 64 world. Run by name only (machine limits): `firedamp_vented_by_shaft` (56 s), `gas_tint_shows` (88 s), `magma_meets_water` (159 s); all three pass. Not run: the whole `screens.sh`, `soak.sh` (not E3b). The six committed PNGs of `firedamp_vented_by_shaft` and `gas_tint_shows` are byte-identical to the fresh ones. My scenario is `tests/e2e/tests/playtest_20261021.rs` (459 s render stage plus about 2 min of render-free sims; passes; fmt and clippy clean). Its shots are `target/e2e/playtest_20261021-<what>-<frame>.png`, notes `target/e2e/playtest_20261021.log`. `PT_STAGE=study|late|render` runs one part. Seasons only advance on F6 (editor key, no clock), so "pace" below is counted in seasons.

What I played: (1) four sealed 8x4x8 coal workings with a shaft 1, 2, 4 and about 5 cells wide, 10 seasons each (sims); (2) a real player (1.7 m, eye 1.55 m, so the eye is in the ceiling row) standing in a sealed 4-high coal working, waiting season by season, then digging a one-cell shaft up with the pickaxe and waiting more; (3) a working with a magma chamber in its east wall at ceiling height, the player facing it; (4) the same with a lone magma cell, with a lone magma_source, and with a pocket that filled for 12 seasons before a heat source touched it.

What works:
- The HUD tells the truth about the camera cell. Under the roof the readout goes `firedamp 18` (season 1), 33, 47, 62, 77 and the icon goes blue (season 1) then red (season 5, at 77 >= ignite_min 64); the sealed-s0/s1/s5 PNGs show exactly that. The numbers agree with the host's layers cell for cell.
- Digging up by hand works as a player expects. Looking straight up (pitch 1.5 rad) in the cave and holding the swing broke coal in 44 frames, stone in 61, topsoil in 19 and 19: the whole 4-cell shaft in 143 frames (2.4 s). `shaft-dug-looking-up-258.png` is the best picture of the milestone: a square shaft, grey stone, green topsoil, a framed patch of blue sky, the outline cube on the target. The readout in that frame still says `firedamp 77`.
- Opening the roof does change the cell under it: firedamp at the eye 77 -> 40 in one season, icon red -> blue. The fully opened cave in `firedamp_vented_by_shaft-vented-34.png` reads `firedamp 5` with sky and sun filling the view: unmistakable relief.
- The tint works and reads as intended at 255: steam (`gas_tint_shows-steam-47.png`) washes the scene to pale grey-white but wall, floor and the white `steam 255` readout stay legible; smoke (`-smoke-57.png`) dims the view to a brown-grey dusk. Firedamp correctly does not tint (`-lantern-67.png` differs from clean only by the red icon and `firedamp 64`).
- Ignition next to a real heat source happens and conserves: with a magma chamber (3x3x3 of magma around a magma_source) in the wall, season 4 gave `ignitions 2`, 131 firedamp burnt to 131 smoke, air in the zone 256 -> 282. Facing the chamber, magma is a glowing orange block with its own label (`magma`, ignite-s3/s4 PNGs).
- Nothing crashed and no render failure in any of the five parts.

What looks wrong (with screenshot names):
1. A coal working is a black box. In `playtest_20261021-sealed-s0-52.png` to `-sealed-s5-98.png` and `looking-up-at-roof-104.png` the walls, floor and roof are the same near-black (coal_seam is (34,32,32)); no faces, no depth, no sign of the room's shape. Only the target outline and the label show. The same holds for `firedamp_vented_by_shaft-sealed-22.png`, which shows only a faint horizon where wall meets floor. There is no lantern light you can see.
2. The lantern "flares" only as a 10x16 px icon. In the sealed-s0, s1, s5 shots the scene is pixel-identical between steady, flaring and danger apart from that icon and the readout text. Nothing in the world (light colour, intensity, flicker) changes. The icon colours (yellow, blue, red) are fine, but the blue "flaring" state is unexplained: nothing in CONTROLS.md or the HUD says what the icon or the `firedamp N` line mean, and CONTROLS.md does not mention gas at all.
3. Ignition is invisible. `ignite-s3-363.png` (before) and `ignite-s4-373.png` (the season the 2 ignitions fired) differ only in the temperature and firedamp digits. No flash, no smoke in view, no new hole in view: smoke at the eye stayed 0 through season 6 even though the zone held 387 smoke, because it sat by the chamber. The only trace is rubble: `ignite-s6-393.png` has blocks on the floor.
4. `magma_meets_water` shows a pool with a darker square (`target/e2e/magma_meets_water-22.png`): no steam, no obsidian, no magma, no sign that anything happened in the season it asserts. The committed `docs/screens/magma_meets_water-20.png` is stale: it lacks the lantern icon F154 added, and the fresh shot lands on frame 22, so `screens.sh magma_meets_water` will replace it under a new name.
5. The magma glows but lights nothing: in every ignite shot the coal around the orange block is the same black.

What feels broken as a player:
1. A shaft is not a fix, and the game says it is. My player dug a one-cell shaft straight above their head, stood under it, and watched the readout fall 77 -> 40 and the icon go red -> blue. That is the relief the scenario promises, but it is local. Over the six seasons after the shaft the whole ceiling kept rising: mean 90 -> 134, hottest cell 121 -> 204, and the eye reading crept back (40, 41, 44, 47, 50, 53). The render-free sweeps say the same: after four sealed seasons (ceiling mean 73.9, max 103), a 1-wide shaft gives ceiling means of 83, 93, 102, 110 ... 146 after 10 seasons (still rising); 2-wide: 73 -> 87 and rising; 4-wide: settles at mean 46, max 132 from season 3; the widest I tried (5x5 of the 8x8 roof) settles at mean 39, max 128. None gets the hottest ceiling cell under 64, so the danger state stays true somewhere in the cave for ever. Only the scenario's full-width skylight vents (4720 -> 78 in one season). This is not the "narrow shafts vent in about 7 seasons" of the F151 notes: that figure is for a cave that does not emit; a coal-lined working's own emission (2 per sub-step per exposed face, about 4,000 a season for this cave) beats a narrow shaft. A player who opens a small vent will think the problem is solved because their own cell reads blue.
2. Danger warns about your cell only, and it can arrive too late. In the chamber working the first ignition fired in season 4 while the eye cell read `firedamp 58` and the icon was still blue; the ceiling cell next to the heat was already at 64 or more (cave max 88). A player watching the lantern was never told. The blue "flaring" state also turns on at a single unit (18 in season 1; `lantern_state` treats any c > 0 as flaring), so in any coal working it is on from season 1 and carries no information until it jumps to red.
3. The firedamp build-up pace is good as drama and hard to read as a rule. In a 4-high working the player's eye hits 64 in season 5 (62 in season 4); in the 2-high F156 cave it is season 3. Because the icon is blue from season 1 and nothing visible changes, five presses of F6 pass with no sense of a clock ticking. I would not make it faster (a 4-high cave then fills in 3-4 seasons, already quick); I would make it legible: a gradient on the icon or the readout before 64, and a visible gas.
4. Ignition is a nibble, not a blast, and nothing stops the gas building. The blast radius is (c / 64) * 2 cells and ignition fires at c >= 64, so a pocket that meets heat while it is still filling always blasts at radius 2 (1 m) and burns 64 to 100 units of a zone holding about 15,000 (under 1%). Chamber working: ignitions 2, 1, 3 in seasons 4, 5, 6; zone air 256 -> 282 -> 292 -> 325 (about 3 m3 a season); firedamp in the zone still rising 15.5k -> 23.8k; the player stands in 114 C air (the readout climbs 22, 80, 98, 109, 115 C) and nothing happens to them (by design, SAD-P2 section 16). The one real blast needs a pocket left to fill first: 12 sealed seasons (ceiling mean 202, max 238), then a magma_source touched the wall: one ignition, zone air 256 -> 891 (+635 cells, about 79 m3, radius 6), 192 gas burnt, then 0 ignitions in the next two seasons because the cave is now too big to re-reach 64 near the source (firedamp total still +4,900 a season). That is a good scene and the only dramatic one, but only an automated test builds it; a player has no reason to wait 12 seasons.
5. A lone magma cell next to the working never ignites anything. In the `single magma` study the cell is air after season 1 (the liquid ran out onto the cave floor and froze: basalt +1), and the ceiling max reached 255 by season 14 with ignitions 0 throughout. The chamber and the magma_source both ignite at season 4. A player who puts magma beside the coal with the F1 brush will see nothing happen and conclude ignition is broken: the rule (a heat source or a magma cell face-adjacent to a cell with gas >= 64) holds only while the magma stays put.

Pacing opinion (asked): slow firedamp build-up is right for a working you must walk into (4-5 seasons to red). Slow vent-down is not "slow": it never completes for any shaft narrower than most of the roof. The risk is not that venting takes 7 seasons but that the game rewards a 1-cell shaft with a blue icon and a falling number. The wide shaft in `firedamp_vented_by_shaft` proves the mechanism and is a fair test, but the player's natural action (a small shaft, a pickaxe) is the one that does not work, and no screenshot or number tells them why. Placing a non-emitting block over the coal faces (emission needs exposure) is the other obvious fix; I did not test it.

Top 3 fixes within Phase 1 scope (E3b):
1. Make the working readable. The coal's near-black colour and the absence of any visible lantern light make every sealed shot a black box. Give the lantern a visible light that follows its state (flare colour and intensity at Flaring, a pulse at Danger) and lift the coal colour or give faces a distinguishable shade, so the cave shape and the flare are seen in the world and not only in a 10x16 px icon.
2. Make the gas legible before it is dangerous and say what the icon means: a gradient (flaring should start at a data-driven share of ignite_min in `eco.ron`, not at 1), the `firedamp N` readout coloured with the icon, and a short HUD legend in CONTROLS.md (readout, blue and red icon, F6 as the season key). Optionally report the room's ceiling maximum, not only the eye cell, so a 1-cell shaft does not read as solved.
3. Make ignition and venting visible: a one-frame flash or smoke puff and a brief tint at the blast, visible haze at the shaft mouth; re-shoot `magma_meets_water` (stale committed shot, no sign of steam, obsidian or magma in view). Put a line in CONTROLS.md or the design notes that a lone magma cell flows away without igniting.

Files created or changed by this playtest: `tests/e2e/tests/playtest_20261021.rs` (new, untracked) and this section of `docs/PLAYTEST.md`. Nothing under `docs/screens/` changed.

## Playtest 2026-10-23 at fda024b (milestone E5 and Phase 2 report; F165-F175)

Method: headless rmc_testkit on a software adapter, run by name. `grass_greens_swale` (57 s), `fireweed_takes_copper` (82 s), `tailings_kill_moss` (59 s) and `layer_overlay_renders` (57 s) all pass, and their PNGs are byte-identical to the committed `docs/screens/` ones. Not run, per the machine limits: `screens.sh`, `soak.sh`, the whole e2e suite and `soak_seasons` (I used its record in `target/e2e/soak_seasons-stats.txt` and the committed `soak_seasons-*.png`). I also built `rmc_app` in release and ran `--headless --seasons 8` (14.6 s) and `--seasons 20` (27 s) with `--save` under `target/pt23/`. `report.json` is written, `invariants_ok` is true, and no season has violations. My scenario is `tests/e2e/tests/playtest_20261023.rs` (about 3.2 min for the main stage, about 1.6 min for `PT_STAGE=world`; fmt and clippy clean). The main stage builds a loam swale meadow (grass, moss, shrub, N and P at 96 per loam cell), a copper patch with a checkerboard of fireweed and grass, and runs 6 seasons of the shipped weather. It then takes overlay shots, spawns a real player and harvests a fireweed with the pickaxe. The world stage uses a generated 64x128x64 world with no fixtures. Shots and notes are in `target/e2e/playtest_20261023-*.png` and `target/e2e/playtest_20261023.log`. Nothing was copied into `docs/screens`.

What works:
- The plant mechanics do what the SAD says and are conserved. A fireweed on copper grew to 96 growth at season 1 and 255 at season 3, and held cu 96 by season 6. Its anchor cu fell 120 -> 24 by season 6, which is the amount held. N and P uptake are ledgered. Grass on the same copper dies, as designed.
- Harvest with the real pickaxe works. Holding the swing on a mature fireweed broke it in 2 frames. The inventory went to 12 copper_dust and 1 fireweed, both shown in the HUD, and the crosshair label names the plant ("fireweed").
- Growth and Copper overlays are readable where there is something to show (`playtest_20261023-overlay-growth-s2-*.png`, `overlay-copper-s2-*.png`): the copper patch is easy to find in the overlay.
- `grass_greens_swale-34.png` reads in one frame: a green strip on the swale lip, bare terraces around it, a clear pool. (It is a drought override, not the shipped weather.)
- Ledgers are honest. A 20-season headless run has no violations. The report's 59/60 count is right and its links and commands resolve.

(A) BROKEN
A1. Rain never drains: the default world floods and freezes into a water/ice layer cake that buries the terrain and the plants. This is the biggest finding.
  - Generated e2e world (64x128x64, shipped worldgen), no edits. Mean liquid above ground per column is 4.07 cells at season 2, 13.52 at season 4 and 26.09 at season 8 (max 34). There are 60,463 ice cells at season 8.
  - Column (32,32) reads `wwwww` at season 2, `wwwwwwIIIIIIIIw` at season 4 and `wwwwwwIIIIIIIIwwwwwIIIIIIIIw` at season 8. The replica matches the host, so this is what a player sees.
  - Shots: `playtest_20261023-world-overview-s2-20.png` (the world under a blue sheet), `-s4-32.png` (a white ice slab), `-s8-48.png` (camera inside ice, frame dark grey). My meadow shots at seasons 4 and 6 also have the camera inside ice.
  - The soak record shows the same on the default world. Seed 1 ledger: `water -> ice` 1.23 M against `ice -> water` 0.13 M. `rain_in` is 0 in seasons 14-19 on all three seeds. Evaporation and vent are about 2% of rain. The headless 8- and 20-season runs plateau at about 40.9 M water with `rain_in` 0 from season 11.
  - The committed shots `fireweed_takes_copper-24.png`, `season_swale_fills-20.png` and `soak_seasons-s*` are affected.
  - Likely cause, not confirmed in code: `Rain::target` gives no rain to ice at surface+1 and nothing removes water, so the column fills to the sky. All conservation tests pass because water is conserved: right bookkeeping, wrong ecology.
A2. The shipped world has no plants and cannot grow any.
  - The generated world holds 0 plants (my log, STAGE0), and the 20-season headless run shows born 0 and died 0. REPORT-P2's soak shows `plants_born` 0 on all seeds and `plants_died` 63 on all seeds, which is only the fixture patch.
  - F168 passes, but worldgen plants need moisture and nitrogen, and neither exists at generation time. No UI adds nitrogen (the brush has only Set, Remove and Paint), so a player cannot grow a meadow.
  - Even on a fertilized meadow under default weather, grass fell 792 -> 441 and shrub 20 -> 0 over 6 seasons (19 born, 385 died). Nitrogen was exhausted by season 6 and spread is negligible. Part of this is A1.
A3. `docs/REPORT-P2.md` embeds the stale `docs/screens/magma_meets_water-20.png`. It lacks the F154 lantern icon (the scenario now writes `-22`), and it shows no magma, steam or obsidian. This was already reported on 2026-10-21 and is still open.
A4. REPORT-P2 soak section:
  - No units. `rain_in` 270M is moisture units (255 per full cell), roughly 1/255 of that in cells.
  - Unexplained: the "Cycle sums" line, `water_saturation_loss` varying 201k to 1.17M, and `plants_born` 0 everywhere. The section does not say the soak never exercises spread or harvest.
  - Per-season ms (9-21 s) is unlabeled and looks contradictory to the perf section.
  - The numbers come from the untracked `target/e2e/soak_seasons-stats.txt`, so a clean checkout cannot regenerate them.
  - The soak PNGs are 320x180 with no captions. The ten layer-view shots appear twice (once under the layer views, once in the scenario gallery). Some scenarios have no "Features:" line.
A5. The headline hides the misses: "50 edits" at 3.6-4.4 s against the 3 s target is **missed** and "snapshots + remesh" is unmeasured (both per PROGRESS and `docs/perf-p2.md`), yet F173 is passed and the summary says only 59/60.
A6. Testkit: `gen_params` does not call `fit_to`, so a 64-high `TestConfig` world is solid to the ceiling (surface_y 63, `rain_in` 0). Any E2E on a short world silently tests a solid block. I lost one run to it.
A7. The editor panel cannot be judged from screenshots. `editor-panel-on-plant-*.png` equals `pov-fireweed-*.png`, because the testkit never renders egui. F169's panel is tested only as strings.

(B) Look and feel, missing features
B1. Plants are flat, opaque, uniform 1-cell cubes that look the same at growth 0 and 255, and healthy or stressed. Grass on loam is an indistinguishable green carpet (`closeup-bank-s0-21.png`, `closeup-bank-s2-79.png`). Moss barely differs from the ground, and the moss/terrain contrast in `tailings_kill_moss-30.png` is invisible. Fireweed (pink) is readable.
B2. Moisture overlay is low contrast. No overlay has a legend or an on-screen name, and the overlay stays on after the editor closes.
B3. A player cannot tell what a season did. F6 and F7 work only with the editor open, and the HUD shows no season, weather or plant info. `season_lines` has no plant census, standing-water or ice totals, or evaporation, so it could not show a flood building. The HUD plant label gives no growth or held copper.
B4. No before/after pair or caption exists for any plant scenario, so the playtester judges one frame each, and in `fireweed_takes_copper-24.png` the flood hides the dead grass, the control and the fireweed.

Are plants visible and readable? Only fireweed, and only in fixtures. Grass and moss are green on green. Can a player tell what the season did? Only by the world changing, mostly by flooding.

Top 3 fixes within Phase 2 scope
1. Give water somewhere to go (A1): runoff or a sink at the world edge or bottom, rain that lands on or melts ice, and a ledger term for it. Add a soak or invariant check that mean standing-liquid depth stays bounded over 20 seasons on the default world. Then re-shoot the E5 scenes.
2. Put plants into the shipped game (A2, B1): seed moisture and nitrogen (or loam) at worldgen so F168 plants exist, give the editor a way to add nutrients (SetLayer in the brush), and make plants legible (scale with growth, tint on stress, brown loam).
3. Fix the report and legibility (A3-A5, A7, B2-B4): refresh the stale magma shot, add units and "what this proves / does not prove" for the soak, put the perf misses in the headline, de-duplicate the gallery, add captions and before/after pairs, show the active overlay's name on the HUD, add a one-line season summary after F6, and make the testkit capture the egui panel.
