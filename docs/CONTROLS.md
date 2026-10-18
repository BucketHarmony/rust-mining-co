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
| `Left mouse` | Swing the pickaxe at the targeted cell (hold to keep swinging) |

The player climbs a one-cell rise by walking into it; two cells need a jump.

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
| `F8` | Regenerate the world from the seed |
| `F9` | Load the saved world |

- Brush size is a radius in cells, from 1 to 15. A Sphere has that radius. A
  Box is a cube 2 × size + 1 cells on a side. A Cell ignores the size.
- The brush material cycles through every solid material in id order. Until
  you pick one, the brush uses the first material tagged `placeable`.

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
