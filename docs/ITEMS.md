# Items, tools and the character screen: plan

Owner request, 2026-10-27: "We probably need a tool menu and different tools. An inventory where we can drag and drop items between the character and the inventory like tools and equipment and armor/clothes." The owner approved the four steps below ("make it so") and asked for them as numbered features, tests first.

## What exists

- `rmc_world::inventory::Inventory` counts materials (SAD §6: "there is no separate item type in Phase 1"). The host keeps one per joined player. Yields go in, a `Place` takes one out, and `ServerMsg::Inventory` sends the totals.
- `tools.ron` has a single profile, `pickaxe`. The client swings it with left mouse.
- The HUD's Inventory chip lists the held materials. The mouse wheel picks the material that right mouse places (`SelectedMaterial`).
- Inventories are not saved (SAD §10 saves chunks only).

## Steps

Feature ids are given when the owner merges a backlog file: `features-p*.json` and `features.json` are protected. Until then the steps are I1–I4. The test names below are the ones each step adds.

### I1. Items and the bag (model, host, protocol)

Status: done 2026-10-27.

- `assets/items.ron` holds the item rows: `key`, `name`, `tags` (`tool`, `clothing`, `armor`, ...), `stack` (largest stack), `tool` (a `tools.ron` profile key, for tools) and `slot` (the equipment slot it is worn in, for wearables). It also holds the bag's sizes, the largest stack of a material, and the starting kit.
- A **thing** is a material or an item. A **stack** is a thing and a count.
- A **bag** holds stacks in `hotbar` slots (the tool menu), `pack` slots and one stack per **equipment slot**: head, body, legs, feet, hands. An equipment slot takes only an item whose row names that slot.
- Bag operations:
  - **Insert** merges into existing stacks of the same thing, then fills empty hotbar slots, then empty pack slots. It returns what did not fit.
  - **Remove** takes from the hotbar first, then the pack.
  - **Move** from one slot to another: into an empty slot; onto the same thing (merging up to the stack limit, the rest stays); or swapping whole stacks when each slot accepts the other's thing. Anything else is refused and changes nothing.
- The host keeps a bag per player when it is given the item table (`Host::set_items`):
  - a joining player gets the starting kit, with wearables put on;
  - yields go into the bag, and a `Place` spends from it;
  - after each change the player gets `Inventory` (the material totals, as before) and the new `ServerMsg::Bag`;
  - `ClientMsg::MoveItem { from, to, count }` moves a stack, and the player then gets its bag.
- Hosts without the item table behave exactly as before, so the existing host tests and scenarios are unchanged. The messages are additive, so `PROTOCOL_VERSION` stays 2.
- The client mirrors the bag (`ClientBag`). The game binary loads `items.ron` and gives it to the host.
- Tests: `items::tests::{shipped_items_load, bad_items_are_refused, insert_merges_then_fills_hotbar_then_pack, remove_takes_hotbar_first, moves_merge_swap_and_refuse, equipment_slots_take_only_their_items, starter_kit_wears_its_clothes}`, `host::tests::{bags_hold_yields_spend_places_and_move_items, hosts_without_items_send_no_bags}`, codec samples for `Bag` and `MoveItem`, and `hud::tests::client_bag_follows_the_host`.

### I2. Tool menu (hotbar) and several tools

Status: done 2026-10-27.

- `tools.ron` gains a map of further profiles (`shovel`, `axe`) and the bare `hand`. An item row gets `fast` (tags it works well on), `slow_div` (a damage divisor everywhere else) and `model` (its shape in hand). The pickaxe is fast on everything, the shovel on soil, the axe on plants (wood when it exists), and the hand places.
- Number keys 1–9 and the mouse wheel select a hotbar slot, which replaces `SelectedMaterial`:
  - a tool in the slot swings with that profile;
  - a material in the slot is placed by right mouse.
- The hotbar is drawn as a row of chips on the HUD (UI kit), with the selected slot outlined. The view model shows the selected tool: each tool gets its own box model, and a material in hand is shown as a small block.
- The host checks that a `Damage` comes from a tool the player holds in its bag (anti-cheat, owner extension). Not built: see Decisions.
- Tests: `tools::tests::more_tools_and_the_hand_load_by_key`, `items::tests::tools_are_fast_on_their_tags`, `hotbar::tests::{number_keys_and_wheel_select_slots_and_what_is_held, hotbar_row_shows_the_slots_with_the_selection_outlined}`, `pickaxe::tests::held_tools_hit_hard_only_on_their_tags`, `viewmodel::tests::{every_held_model_stays_within_reach, view_model_follows_what_is_held}`, and `e2e:hotbar_renders` with a semantic assert and screenshots (rule 21).

### I3. Character screen with drag and drop

- `Tab` (or `I`) opens the character screen on the UI kit's menu layer. It shows the pack grid, the hotbar row and a figure with the five equipment slots. The cursor is freed while it is open.
- Drag a stack with the left mouse; drop it on a slot to send `MoveItem`. Right-drag moves half a stack. Shift-click moves a stack between the pack and the hotbar. A drop on a slot that refuses it snaps back.
- Moves are predicted on the client and corrected by the host's `Bag`, so a refused move snaps back without a flicker.
- Tests: the screen opens and closes and frees the cursor; a drag sends the right `MoveItem`; a refused drop leaves the bag unchanged; equipment slots show what is worn; `e2e:character_screen_renders` with a semantic assert and screenshot.

### I4. What gear does

- Tools: the I2 tag speeds.
- Clothing and armor get `warmth` and `protection` in `items.ron`. The player's felt temperature is the air temperature (from the calendar) plus the worn warmth, shown in the HUD readout. With nothing to harm the player yet, protection is shown, not used.
- Harvests from the vegetation plan (wood, fruit, seeds; `docs/VEGETATION.md` §7) arrive as items in the bag.
- Tests: worn warmth adds to the felt temperature; the readout shows it.

## Decisions

- Materials stay `MaterialId`s and are not copied into `items.ron`. A stack holds either kind, so mining, placing and the material table are unchanged.
- Equipment slots are a fixed set in code (head, body, legs, feet, hands), named by items in data. Code branches on tags and slots, never on item keys (rules 5 and 18).
- Bags are not saved yet, matching inventories (SAD §10). Saving them is a save-format change for the owner.
- `items.ron` is not in the material hash, the same as `tools.ron`.
- I2: what a tool is fast on lives in its `items.ron` row, not in its `tools.ron` profile. The profile stays a small `Copy` value that the swing, the HUD and the editor already pass around, and the item row is where tags are already read. The pickaxe keeps `pickaxe:` as its own field, so older tool files still load.
- I2: the pickaxe is fast on everything (`fast: []`), so mining stone, ore and soil feels as before. The shovel (damage 20) is quicker than the pickaxe on dirt and clay, and the axe on shrubs; each does a quarter of its damage elsewhere.
- I2: the swing's damage comes from the hit cell's tags, and a sphere-shaped tool applies that one value to its whole shape (`EditOp::Damage` carries one amount).
- I2: no host anti-cheat for `Damage` yet. The editor's brush sends `Damage` with amounts no tool has, from the same player, so the host cannot tell them apart without a protocol change; and the game is single-player. Recorded for the owner.
- I2: an item without a `model` (a hard hat in the hotbar) leaves the hand empty; a material in hand is drawn as a small block of its color.
