# Controls

Rust Mining Co reads only Bevy's keyboard, mouse-button and mouse-motion
input, so these bindings behave the same in the game and in the headless
test kit. Keys are named by their position on a US layout.

## Player

| Input | Action |
|---|---|
| `Mouse` | Look around (pitch stops at ±89°) |
| `W` / `S` | Walk forward / back |
| `A` / `D` | Strafe left / right |
| `Shift` | Sprint while held |
| `Space` | Jump (held: jump again on landing) |
| `Ctrl` | Crouch while held: fits 3-cell gaps, slower, no stepping off ledges; stands when there is headroom |
| `Left mouse` | Swing the pickaxe at the targeted cell (hold to keep swinging) |
| `Right mouse` | Place the selected material against the targeted face (hold to keep placing) |
| `P` | Pause / resume the ecology clock (one season sub-step per second by default, `--eco-tick-ms`) |
| `Mouse wheel` | Select the next (down) / previous (up) material in the inventory |
| `E` / `Q` | Select the next / previous material in the inventory |

The player climbs a one-cell rise by walking into it; two cells need a jump.

Placing spends one of the selected material per cell, and only works on an
open cell (air or water) within the pickaxe's reach (3 m). Nothing is
selected until you turn the wheel or press `E` / `Q`; the selected line of the
inventory is marked `>`. With none of it left, a place is refused and the
selection stays until you pick another. A cell where a player stands is left
open and costs nothing. With the editor open, right mouse, the wheel and
`E` / `Q` do nothing.

## Cursor

In the game window the cursor is hidden and held in the window while playing,
and only then does the mouse turn the view.

| Input | Action |
|---|---|
| `Escape` | Release the cursor |
| `Left mouse` | In the window, with the cursor released: grab it again (that click does not swing) |

Opening the editor (F1) releases the cursor for its panel, and closing it
grabs the cursor again. Switching to another window releases it too.

## Fly camera

| Input | Action |
|---|---|
| `V` | Turn the fly camera (noclip) on / off |
| `Mouse` | Look around |
| `W` / `S` | Fly forward / back along the view direction |
| `A` / `D` | Fly left / right |
| `Space` / `Ctrl` | Fly up / down |
| `Shift` | Fly faster while held |

While the fly camera is on, the player and the pickaxe do nothing. Turning it
off drops the player where the camera is, facing the same way. If the camera
is inside rock, the player lands on the nearest spot it fits within 8 cells;
with no such spot it goes back to where it stood before flying.

## Editor

F1 opens and closes the debug editor. The other editor keys work only while
it is open. Each key acts once per press. The egui panel changes the same
settings.

| Key | Action |
|---|---|
| `F1` | Open / close the editor |
| `1` | Brush op: Set |
| `2` | Brush op: Remove |
| `3` | Brush op: Paint |
| `4` | Brush shape: Cell |
| `5` | Brush shape: Box |
| `6` | Brush shape: Sphere |
| `[` | Brush size down |
| `]` | Brush size up |
| `,` | Previous brush material |
| `.` | Next brush material |
| `F2` | Toggle chunk-bounds gizmo |
| `F3` | Toggle targeted-cell gizmo |
| `F4` | Toggle mesh-queue gizmo |
| `F5` | Save the world |
| `F6` | Advance one season |
| `F7` | Next layer overlay (after the last: none) |
| `F8` | Regenerate the world from the seed |
| `F9` | Load the saved world |

- Brush size is a radius in cells, from 1 to 15. A Sphere has that radius. A
  Box is a cube 2 × size + 1 cells on a side. A Cell ignores the size.
- The brush material cycles through every solid and liquid material in id
  order, so it can place water, ice, magma, crushed sulfide and copper dust.
  Until you pick one, the brush uses the first material tagged `placeable`.
  A brushed water cell is full; a brushed magma cell starts hot.
- The layer overlay colors terrain by one ecology layer instead of its
  material: temperature, moisture, regolith, light, growth, then each
  substance and each gas, then back to the normal look. Light and gas color
  a face by the open cell in front of it. The overlay stays on when the
  editor closes. The panel's Overlay picker selects a layer directly.
- The panel's Season section shows the last season's report: chunks
  changed, rain and vented water, the net change of each substance and gas,
  plants born and died, transitions, and what the edits before it moved.
- The panel's Cell section reads the cell under the crosshair, including a
  plant's species, growth (255 is mature), age, stress and held substances.

## Brush

While the editor is open, the left mouse button is the brush, and the
pickaxe does not swing.

| Input | Action |
|---|---|
| `Left mouse` | Apply the brush once at the cell under the crosshair (up to 32 m away) |

- The ray starts at the fly camera while it is on, else at the player's eye.
- Set fills on the near side of the face you look at, so a placed block
  lands against it. Remove and Paint act on the cell you look at.
- Box and Sphere center on that cell. One click sends one edit; hold does
  not repeat.
