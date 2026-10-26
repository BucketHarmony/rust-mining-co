# UI kit

`rmc_client::ui` is the foundation for the game's interface: menus,
settings, notices and (next) the HUD. This document covers the practice it
follows, how it is built, and how to extend it.

Screenshots: `docs/screens/ui_menu_renders-*.png` (play, menu, settings,
toast), from `scripts/screens.sh ui_menu_renders`.

## What good game UI does

These are the rules the kit enforces or makes easy. Each comes from
published practice, and the sources are listed at the end.

1. **Legible at any resolution and distance.** The minimum text size is
   18 px at 1080p on PC and 26 px on console (Xbox Accessibility Guideline
   101). Text sized in fixed pixels shrinks on a 4K screen, so 16 px at 4K
   is unreadable from a couch. The UI is designed at one reference height
   and scaled with the screen. → `reference_height`, `UiScale` from the
   viewport height, the smallest text role (`caption`) is 18. A test fails
   if it goes lower.
2. **Player-adjustable size.** Players should be able to scale the
   interface, commonly 100–200 %. → the Settings screen's UI scale, from
   75 % to 200 %.
3. **Contrast that survives the scene.** Text needs at least 4.5:1 contrast
   with what is behind it, and non-text elements such as focus rings and
   borders need 3:1 (XAG 102, following WCAG). Game backgrounds change
   constantly, so text sits on a panel or scrim, never bare on the world.
   → translucent surfaces composited over pure black *and* pure white must
   keep text at 4.5:1. `shipped_theme_loads_and_text_keeps_contrast` checks
   every text/surface pair in `ui.ron`.
4. **Safe area.** Keep important UI inside the inner ~90 % of the screen,
   since TVs overscan and corners are outside the eye's focus. → the HUD
   layer is inset by `safe_area`, and elements are placed by anchor, not by
   pixel offset.
5. **Consistency through a design system.** Professional teams keep one
   set of tokens (colors, type ramp, spacing, radii, motion), and widgets
   draw only from them. God of War Ragnarök's UI team built and maintained
   such a system for its whole UI. → `assets/ui.ron`; widgets take a role
   or token, never a color or size.
6. **Every input, with visible focus.** Menus must work with the mouse,
   the keyboard and (later) a gamepad. The focused element must always be
   visible. → Bevy's headless `ui_widgets::Button` (pointer, Enter and
   Space, AccessKit role), `TabGroup::modal()` per screen, Tab and arrow
   navigation, `AutoFocus` on the first button, and a high-contrast focus
   ring.
7. **Minimal, purposeful HUD.** Show what the player needs when they need
   it, and fade the rest. Notices should inform without stealing input.
   → toasts never take the pointer or focus; they fade in, hold and fade
   out, and only `toast_max` show at once.
8. **Motion that explains, not decorates.** Animate an element's arrival
   and departure, keep it still at rest, and use short durations
   (~100–250 ms). → `motion.fast`, `motion.normal`.
9. **Test on the worst background.** Check over the brightest and busiest
   scenes, not a dark test room. → the contrast test composites over
   white, and the e2e scenario shoots over the bright sky and grass.

Not done yet, and recommended next: remappable controls, colorblind-safe
status colors (pair color with an icon or shape), subtitles/captions for
sound cues, gamepad navigation, and persisting `UiSettings`.

## Architecture

```
assets/ui.ron ──► UiTheme (Resource) ──► widgets: text / panel / button
                     │
viewport height ─────┼──► UiScale = height/reference × UiSettings.scale (clamped)
                     │
layers (GlobalZIndex):  Hud 10  ─ safe area ─ 9 HudAnchors (HudSlot places)
                        Menu 100 ─ UiScreens stack ─ scrim + panel (TabGroup::modal)
                        Toast 200 ─ ToastStack (top center)
```

| Module | What it owns |
|---|---|
| `ui/theme.rs` | `UiTheme` tokens from `ui.ron` (hex colors, type ramp, spacing, radii, focus ring, motion), `UiFonts`, `contrast_ratio`, `over` |
| `ui/scale.rs` | `UiSettings` (player scale), `effective_scale`, `UiScale` each frame from the `GameCamera` viewport |
| `ui/layers.rs` | `UiLayer` roots, the safe area, `HudAnchor` × 9, `HudSlot` placement |
| `ui/widgets.rs` | `text`, `panel`, `button`, `ButtonStyle`, `UiAction`, `button_look`, and the per-frame button styling |
| `ui/screens.rs` | `UiScreens` stack, Escape and cursor integration, drawing the top screen, Up/Down navigation, the `Activate` handler |
| `ui/toast.rs` | `Toast` message, fade curve `toast_alpha`, stacking and cap |
| `ui/mod.rs` | `UiKitPlugin`, `UiSystems` |

- **Cursor:** an open screen counts as "editor open" for `CursorGrab`.
  The cursor is free, clicks don't grab, and closing the last screen grabs
  again. Releasing the cursor during play (Escape, focus loss) opens the
  game menu.
- **Wiring:** `rmc_app` adds `UiKitPlugin` to the windowed game when
  `assets/ui.ron` loads, and warns otherwise. The test kit adds it with
  `TestConfig { ui: true, .. }`. The ecology clock's `P` writes a toast.
- **Fonts:** Atkinson Hyperlegible (Braille Institute, SIL OFL,
  `assets/fonts/OFL.txt`). It was designed for low-vision readers, with
  distinct letterforms (Il1, 0O). Regular is for body and caption; Bold is
  for label, title and display.

## Tokens (`assets/ui.ron`)

| Group | Tokens |
|---|---|
| Scale | `reference_height` 1080, `scale` {default 1, min 0.75, max 2, step 0.1}, `clamp` (0.6, 3) |
| Layout | `safe_area` 0.035 (of the shorter side), `space` xs 4 / sm 8 / md 12 / lg 20 / xl 32, `radius` sm 4 / md 8, `border` 1.5, `panel_width` 420, `button` 320 × 52 |
| Type | `caption` 18, `body` 20, `label` 22, `title` 32, `display` 48 |
| Color | `text`, `text_dim`, `text_on_accent`; `surface`, `surface_raised`, `surface_hover`, `scrim`, `border`; `accent` (+ hover, pressed); `focus`; status `info`, `ok`, `warning`, `danger`; `shadow` |
| Motion | `fast` 0.12 s, `normal` 0.22 s, `toast` 3 s hold; `toast_max` 3 |

The palette is warm ore and lamplight: dark umber surfaces and a brass
accent, with off-white text. Change a token, and the contrast test tells you
whether the result is still legible.

## Extending

**A new button action:** add a variant to `UiAction`, then handle it in
`screens::on_activate`. Use `button(theme, fonts, style, action, label)`;
pick `Primary` for the one main choice on a screen.

**A new screen:** add a `Screen` variant, then draw it in
`screens::sync_screens`. Give the first button `AutoFocus`, and push it
from an action. Escape and Back already pop it.

**A HUD element:** spawn it with `HudSlot(HudAnchor::…)`. It moves into
that anchor inside the safe area and stacks with its neighbours. Size its
text with `text(theme, fonts, TextRole::…, ColorToken::…, …)`, and put it on
a `panel`-like surface if it sits over the world.

**A notice:** `MessageWriter<Toast>` (or `Option<ResMut<Messages<Toast>>>`
where the kit may be absent), `Toast::info("…")`, or set `tone` to `Ok`,
`Warning` or `Danger`.

## Migration plan for the existing HUD

`hud.rs` predates the kit. It uses absolute pixel offsets, Bevy's default
font, colors in code and z 0. Move it piece by piece, keeping `HudState`
and its tests unchanged:

1. Inventory, then lantern/gas, then the spoil indicator, then the season
   readout: give each `HudSlot` (bottom-left, top-right, center, top-left)
   and drop its absolute offsets.
2. Replace `HUD_FONT_SIZE` and the color constants with text roles and
   tokens. Add status tokens where a HUD color has a meaning (lantern
   danger = `danger`).
3. Put readable text over the world on a translucent surface (rule 3).
4. Re-shoot the HUD scenarios (`scripts/screens.sh <scenario>`). The
   playtester judges the look (CLAUDE.md rule 21).

## Sources

- Xbox Accessibility Guideline 101, text display (minimum sizes):
  https://devdocs.xbox.com/build/game-principles/accessibility/xag-deep-dives/xag-101-text-display.md
- Xbox Accessibility Guideline 102, contrast:
  https://devdocs.xbox.com/build/game-principles/accessibility/xag-deep-dives/xag-102-contrast.md
- XAG version history:
  https://learn.microsoft.com/ar-sa/xbox/accessibility/xag-version-history
- Game Accessibility Guidelines, allow interfaces to be resized:
  https://gameaccessibilityguidelines.com/allow-interfaces-to-be-resized/
- How to handle UI scaling in games (uxdesign.cc):
  https://uxdesign.cc/how-to-handle-ui-scaling-in-games-3bdfe70bc7ff
- A deep dive into God of War Ragnarök's user interface (80.lv):
  https://80.lv/articles/a-deep-dive-into-god-of-war-ragnar-k-s-user-interface
- UI and UX in games: menus, HUDs and feedback systems:
  https://respawn.outlookindia.com/gaming/gaming-guides/ui-and-ux-in-games-building-menus-huds-and-feedback-systems
- Game UI/UX design principles, the invisible interface:
  https://www.strayspark.studio/blog/game-ui-ux-design-principles
- Bevy `bevy_input_focus` (focus, tab navigation): https://docs.rs/bevy_input_focus
- Bevy examples: standard widgets with observers
  (https://bevy.org/examples/ui-user-interface/standard-widgets-observers/),
  feathers (https://bevy.org/examples/ui-user-interface/feathers/), and
  headless tabs (https://bevy.org/examples/ui-user-interface/headless-tabs/)
