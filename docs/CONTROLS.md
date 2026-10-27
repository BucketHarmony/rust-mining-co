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
| In water | Swim: sink slowly, `Space` swims up, `Space` against a bank climbs out |
| `Left mouse` | Swing the held tool (the pickaxe without a bag) at the targeted cell (hold to keep swinging); it aims through water and magma at the solid behind |
| `Right mouse` | Place the selected material against the targeted face (hold to keep placing) |
| `1` – `9` | Select a hotbar slot (with a bag: the windowed game) |
| `I` / `Tab` | Open the character screen (with a bag; see below) |
| `P` | Pause / resume the ecology clock and the time of day (one game hour, a season sub-step, every 5 s by default: `--day-seconds 120`; `--eco-tick-ms N` sets the hour directly, 0 stops the clock) |
| `Mouse wheel` | Select the next (down) / previous (up) hotbar slot; without a bag, the next / previous material in the inventory |
| `E` / `Q` | Select the next / previous hotbar slot; without a bag, the next / previous material |

The player climbs a one-cell rise by walking into it; two cells need a jump.

The chip in the bottom-left corner shows the season, the year, the air
temperature and the sky: dry, frost, or light, steady or heavy rain (snow
below 0 °C). While the season's rain lands, rain falls around you, but not
under a roof or in a cave. While the seasons are paused (`P`), nothing falls
and the chip says so.

Placing spends one of the selected material per cell, and only works on an
open cell (air or water) within the pickaxe's reach (3 m). Nothing is
selected until you turn the wheel or press `E` / `Q`; the selected line of the
inventory is marked `>`. With none of it left, a place is refused and the
selection stays until you pick another. A cell where a player stands is left
open and costs nothing. With the editor open, right mouse, the wheel and
`E` / `Q` do nothing.

The windowed game gives you a bag (`docs/ITEMS.md`). Its hotbar is the row of
chips at the bottom of the screen; the selected slot is outlined. What the
slot holds is in your hand:

- A tool swings with its own profile and is fast on some cells: the pickaxe
  on everything, the shovel on soil, the axe on plants. Elsewhere the shovel
  and the axe do a quarter of their damage.
- A material is placed by right mouse; left mouse swings your bare hand.
- An empty slot, or a thing that is not a tool, is a bare hand.

## Cursor

In the game window the cursor is hidden and held in the window while playing,
and only then does the mouse turn the view.

| Input | Action |
|---|---|
| `Escape` | Release the cursor and open the game menu |
| `Left mouse` | In the window, with the cursor released and no menu open: grab it again (that click does not swing) |

Opening the editor (F1) releases the cursor for its panel, and closing it
grabs the cursor again. Switching to another window releases it too and
opens the game menu. With the editor open, Escape releases the cursor
without opening the menu.

## Character screen

`I` or `Tab` opens the character screen while you have a bag and no menu
or editor is open. It shows what you wear (head, body, legs, feet, hands),
the pack and the hotbar. While it is open the cursor is free and the mouse
does not turn the view.

| Input | Action |
|---|---|
| `Left mouse` drag | Move a stack: drop it on another slot (onto the same thing it stacks, onto something else they swap) |
| `Right mouse` drag | Move half the stack (rounded up) |
| `Shift` + `Left mouse` | Move a stack between the hotbar and the pack (worn things go to the pack) |
| `I` / `Tab` / `Escape` | Close the screen |

A slot that does not take the stack (a pickaxe on your head) leaves it
where it was.

## Menus

The game menu (Resume, Settings, Save and quit) opens when the cursor is
released during play. While a menu is open the cursor stays free and the
mouse does not turn the view; the world keeps running.

| Input | Action |
|---|---|
| `Escape` | Close the top menu (from Settings: back to the game menu; from the game menu: back to play) |
| `Tab` / `Shift+Tab` | Move the focus to the next / previous button |
| `Down` / `Up` | The same as `Tab` / `Shift+Tab` |
| `Enter` / `Space` | Press the focused button |
| `Left mouse` | Press a button |

Settings has the UI scale: Larger and Smaller step it by 10 % (75 % to
200 %), Default size resets it. The UI also scales with the window height,
so it is the same size relative to the screen at any resolution.

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
| `F10` | Reload the tile textures |
| `F11` | Open / close the Surfaces window (noise tiles) |
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
- F10 reloads both `assets/tiles.ron` and `assets/surfaces.ron` from disk. F11's
  Surfaces window edits the noise tiles live; see `docs/TILES.md`.
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
