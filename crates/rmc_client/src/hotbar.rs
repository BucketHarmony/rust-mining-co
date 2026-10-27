//! The hotbar: the tool menu (owner extension, `docs/ITEMS.md` I2).
//!
//! With a bag from the host ([`ClientBag`]) and the item and tool tables
//! ([`ClientItems`]), the hotbar slots take over from the material
//! selection:
//!
//! - [`HOTBAR_KEYS`] (1–9) select a slot ([`HotbarSlot`]). The mouse wheel
//!   (down: next) and [`NEXT_SLOT_KEY`] / [`PREV_SLOT_KEY`] step through the
//!   slots, wrapping. Not while the editor is open.
//! - [`Held`] is what the selected slot holds. A tool swings with its
//!   `tools.ron` profile and is fast on its row's tags. An empty slot, a
//!   material or an item that is not a tool swings with the hand profile
//!   (the pickaxe when `tools.ron` has none) at full speed. The profile in
//!   use is the [`Pickaxe`] resource, so reach and interval follow the tool.
//! - A material in the selected slot is what right mouse places
//!   ([`SelectedMaterial`]); anything else places nothing.
//! - The HUD shows the slots as a row of chips at the bottom of the screen
//!   ([`HotbarHud`]): the slot's key, the thing's name and, past one, its
//!   count. The selected chip is outlined in the accent color. The row is
//!   hidden without a bag.
//!
//! Without a bag (a host without items, most tests) none of this runs: the
//! pickaxe swings as set up and the wheel steps the material selection.

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use rmc_world::ToolTable;
use rmc_world::items::{Bag, HeldModel, ItemTable, Thing, ToolSpeed};
use rmc_world::materials::MaterialTable;

use crate::editor::EditorState;
use crate::hud::{ClientBag, NEXT_SLOT_KEY, PREV_SLOT_KEY, SelectedMaterial};
use crate::pickaxe::Pickaxe;
use crate::replica::Replica;
use crate::ui::{ColorToken, HudAnchor, HudSlot, TextRole, UiFonts, UiTheme, text};

/// The keys that select hotbar slots 1 to 9.
pub const HOTBAR_KEYS: [KeyCode; 9] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
];

/// The item and tool tables (`items.ron`, `tools.ron`). Without them the
/// hotbar does nothing.
#[derive(Resource, Clone, Debug)]
pub struct ClientItems {
    pub items: ItemTable,
    pub tools: ToolTable,
}

/// The selected hotbar slot.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HotbarSlot(pub u8);

/// What the selected slot holds.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub thing: Option<Thing>,
    /// The tool's shape in hand; `None` for anything else.
    pub model: Option<HeldModel>,
    /// What a swing is fast on: the tool's, or everything.
    pub speed: ToolSpeed,
}

/// The HUD row of hotbar chips.
#[derive(Component, Debug)]
pub struct HotbarHud;

/// One slot's chip.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotbarChip(pub u8);

/// A chip's text: line 0 the key, 1 the name, 2 the count.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotbarLine {
    pub slot: u8,
    pub line: u8,
}

/// What one chip shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotView {
    pub key: String,
    pub name: String,
    /// Empty for a stack of one.
    pub count: String,
    pub selected: bool,
}

/// [`HotbarPlugin`]'s systems, in `Update`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HotbarSystems;

/// The hotbar (see the module docs).
pub struct HotbarPlugin;

impl Plugin for HotbarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientBag>()
            .init_resource::<HotbarSlot>()
            .init_resource::<Held>()
            .init_resource::<SelectedMaterial>()
            .add_systems(
                Update,
                (select_slot, hold, spawn_hotbar_hud, show_hotbar)
                    .chain()
                    .in_set(HotbarSystems),
            );
    }
}

/// The slot after this frame's number keys, wheel turn (one step per
/// frame) and step keys, in a hotbar of `n` slots.
fn next_slot(current: u8, n: u8, keys: Option<&ButtonInput<KeyCode>>, wheel: f32) -> u8 {
    let mut s = current.min(n.saturating_sub(1));
    let step = |s: u8, forward: bool| {
        if forward {
            (s + 1) % n
        } else {
            (s + n - 1) % n
        }
    };
    if let Some(keys) = keys {
        for (i, key) in HOTBAR_KEYS.iter().enumerate().take(usize::from(n)) {
            if keys.just_pressed(*key) {
                s = i as u8;
            }
        }
    }
    if wheel != 0.0 {
        s = step(s, wheel < 0.0);
    }
    if let Some(keys) = keys {
        if keys.just_pressed(NEXT_SLOT_KEY) {
            s = step(s, true);
        }
        if keys.just_pressed(PREV_SLOT_KEY) {
            s = step(s, false);
        }
    }
    s
}

fn select_slot(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    editor: Option<Res<EditorState>>,
    bag: Res<ClientBag>,
    mut slot: ResMut<HotbarSlot>,
) {
    let Some(bag) = &bag.0 else {
        return;
    };
    let n = bag.hotbar().len() as u8;
    if n == 0 || editor.is_some_and(|e| e.open) {
        return;
    }
    let wheel = scroll.map_or(0.0, |s| s.delta.y);
    slot.set_if_neq(HotbarSlot(next_slot(slot.0, n, keys.as_deref(), wheel)));
}

/// What `slot` of `bag` puts in hand, and the profile it swings with.
pub fn held(bag: &Bag, slot: u8, tables: &ClientItems) -> (Held, rmc_world::ToolProfile) {
    let thing = bag
        .hotbar()
        .get(usize::from(slot))
        .copied()
        .flatten()
        .map(|s| s.thing);
    let row = match thing {
        Some(Thing::Item(id)) => tables.items.get(id),
        _ => None,
    };
    let tool = row
        .and_then(|r| r.tool.as_deref())
        .and_then(|key| tables.tools.profile(key));
    let profile = tool
        .or(tables.tools.hand.as_ref())
        .unwrap_or(&tables.tools.pickaxe);
    let held = Held {
        thing,
        model: tool.and(row).and_then(|r| r.model),
        speed: match (tool, row) {
            (Some(_), Some(r)) => r.speed(),
            _ => ToolSpeed::default(),
        },
    };
    (held, *profile)
}

/// Keeps [`Held`], [`SelectedMaterial`] and [`Pickaxe`] on the selected
/// slot.
fn hold(
    bag: Res<ClientBag>,
    slot: Res<HotbarSlot>,
    tables: Option<Res<ClientItems>>,
    pickaxe: Option<ResMut<Pickaxe>>,
    mut held_res: ResMut<Held>,
    mut selected: ResMut<SelectedMaterial>,
    mut commands: Commands,
) {
    let (Some(bag), Some(tables)) = (&bag.0, tables) else {
        return;
    };
    let (now, profile) = held(bag, slot.0, &tables);
    let material = match now.thing {
        Some(Thing::Material(id)) => Some(id),
        _ => None,
    };
    selected.set_if_neq(SelectedMaterial(material));
    held_res.set_if_neq(now);
    match pickaxe {
        Some(mut p) => {
            p.set_if_neq(Pickaxe(profile));
        }
        None => commands.insert_resource(Pickaxe(profile)),
    }
}

/// A thing's name: the item row's, the material's key, or its id when
/// neither table knows it.
pub fn thing_name(thing: Thing, items: &ItemTable, materials: Option<&MaterialTable>) -> String {
    match thing {
        Thing::Item(id) => items
            .get(id)
            .map_or_else(|| format!("item {}", id.0), |r| r.name.clone()),
        Thing::Material(id) => materials
            .and_then(|m| m.get(id))
            .map_or_else(|| format!("#{}", id.0), |d| d.key.clone()),
    }
}

/// What each chip shows: one per hotbar slot of `bag`.
pub fn slot_views(
    bag: &Bag,
    selected: u8,
    items: &ItemTable,
    materials: Option<&MaterialTable>,
) -> Vec<SlotView> {
    bag.hotbar()
        .iter()
        .enumerate()
        .map(|(i, stack)| {
            let name = stack.map_or_else(String::new, |s| thing_name(s.thing, items, materials));
            SlotView {
                key: (i + 1).to_string(),
                name,
                count: stack
                    .filter(|s| s.count > 1)
                    .map_or_else(String::new, |s| s.count.to_string()),
                selected: i == usize::from(selected),
            }
        })
        .collect()
}

/// Spawns the chip row, once, where the UI kit is.
fn spawn_hotbar_hud(
    mut commands: Commands,
    theme: Option<Res<UiTheme>>,
    fonts: Option<Res<UiFonts>>,
    existing: Query<(), With<HotbarHud>>,
) {
    let (Some(theme), Some(fonts)) = (theme, fonts) else {
        return;
    };
    if !existing.is_empty() {
        return;
    }
    let s = &theme.space;
    let row = commands
        .spawn((
            HotbarHud,
            HudSlot(HudAnchor::Bottom),
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(s.xs),
                display: Display::None,
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    for slot in 0..rmc_world::items::MAX_HOTBAR {
        let line = |line: u8, role: TextRole, color: ColorToken| {
            (
                HotbarLine { slot, line },
                text(&theme, &fonts, role, color, ""),
            )
        };
        commands.spawn((
            HotbarChip(slot),
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                min_width: Val::Px(theme.button.height * 1.6),
                padding: UiRect::axes(Val::Px(s.sm), Val::Px(s.xs)),
                border: UiRect::all(Val::Px(theme.border * 2.0)),
                border_radius: BorderRadius::all(Val::Px(theme.radius.md)),
                ..default()
            },
            BackgroundColor(theme.colors.surface),
            BorderColor::all(theme.colors.border),
            Pickable::IGNORE,
            ChildOf(row),
            children![
                line(0, TextRole::Caption, ColorToken::TextDim),
                line(1, TextRole::Body, ColorToken::Text),
                line(2, TextRole::Caption, ColorToken::Text),
            ],
        ));
    }
}

/// Fills the chips from the bag and the selection.
#[allow(clippy::too_many_arguments)]
fn show_hotbar(
    bag: Res<ClientBag>,
    slot: Res<HotbarSlot>,
    tables: Option<Res<ClientItems>>,
    replica: Option<Res<Replica>>,
    theme: Option<Res<UiTheme>>,
    mut rows: Query<&mut Node, (With<HotbarHud>, Without<HotbarChip>)>,
    mut chips: Query<
        (
            &HotbarChip,
            &mut Node,
            &mut BorderColor,
            &mut BackgroundColor,
        ),
        Without<HotbarHud>,
    >,
    mut lines: Query<(&HotbarLine, &mut Text)>,
) {
    let Some(theme) = theme else {
        return;
    };
    let views = match (&bag.0, &tables) {
        (Some(bag), Some(t)) => slot_views(
            bag,
            slot.0,
            &t.items,
            replica.as_deref().map(|r| &**r.world().materials()),
        ),
        _ => Vec::new(),
    };
    let display = |shown: bool| if shown { Display::Flex } else { Display::None };
    for mut node in &mut rows {
        let want = display(!views.is_empty());
        if node.display != want {
            node.display = want;
        }
    }
    for (chip, mut node, mut border, mut fill) in &mut chips {
        let view = views.get(usize::from(chip.0));
        let want = display(view.is_some());
        if node.display != want {
            node.display = want;
        }
        let selected = view.is_some_and(|v| v.selected);
        let (edge, surface) = if selected {
            (theme.colors.accent, theme.colors.surface_raised)
        } else {
            (theme.colors.border, theme.colors.surface)
        };
        border.set_if_neq(BorderColor::all(edge));
        fill.set_if_neq(BackgroundColor(surface));
    }
    for (line, mut t) in &mut lines {
        let value = views
            .get(usize::from(line.slot))
            .map_or("", |v| match line.line {
                0 => v.key.as_str(),
                1 => v.name.as_str(),
                _ => v.count.as_str(),
            });
        if t.0 != value {
            t.0 = value.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::UiKitPlugin;
    use crate::ui::theme::tests::THEME_PATH;
    use rmc_world::items::{EquipSlot, SlotRef};
    use rmc_world::materials::MaterialId;

    const ITEMS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/items.ron");
    const TOOLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tools.ron");

    fn tables() -> ClientItems {
        ClientItems {
            items: ItemTable::load(ITEMS_PATH).unwrap(),
            tools: ToolTable::load(TOOLS_PATH).unwrap(),
        }
    }

    const DIRT: MaterialId = MaterialId(2);

    /// The starting kit plus 40 dirt.
    fn bag(t: &ClientItems) -> Bag {
        let mut bag = Bag::starter(&t.items);
        bag.insert(&t.items, Thing::Material(DIRT), 40);
        bag
    }

    fn slot_of(bag: &Bag, thing: Thing) -> u8 {
        bag.hotbar()
            .iter()
            .position(|s| s.is_some_and(|s| s.thing == thing))
            .unwrap() as u8
    }

    fn app(with_kit: bool) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(AccumulatedMouseScroll::default())
            .insert_resource(tables());
        if with_kit {
            app.add_plugins(UiKitPlugin {
                theme: UiTheme::load(THEME_PATH).unwrap(),
                assets_dir: None,
            });
        }
        app.add_plugins(HotbarPlugin);
        app
    }

    fn press(app: &mut App, key: KeyCode) {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release_all();
        keys.clear();
        keys.press(key);
        app.update();
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release_all();
        keys.clear();
    }

    fn wheel(app: &mut App, y: f32) {
        app.world_mut()
            .resource_mut::<AccumulatedMouseScroll>()
            .delta
            .y = y;
        app.update();
        app.world_mut()
            .resource_mut::<AccumulatedMouseScroll>()
            .delta
            .y = 0.0;
    }

    #[test]
    fn number_keys_and_wheel_select_slots_and_what_is_held() {
        let t = tables();
        let bag = bag(&t);
        let pick = Thing::Item(t.items.id("pickaxe").unwrap());
        let shovel = Thing::Item(t.items.id("shovel").unwrap());
        let n = bag.hotbar().len() as u8;
        let dirt_slot = slot_of(&bag, Thing::Material(DIRT));

        // Without a bag nothing changes.
        let mut app = app(false);
        app.insert_resource(Pickaxe(t.tools.pickaxe));
        press(&mut app, KeyCode::Digit3);
        assert_eq!(*app.world().resource::<HotbarSlot>(), HotbarSlot(0));
        assert_eq!(*app.world().resource::<Held>(), Held::default());

        app.insert_resource(ClientBag(Some(bag.clone())));
        app.update();
        let held = app.world().resource::<Held>().clone();
        assert_eq!(held.thing, Some(pick), "slot 1 holds the pickaxe");
        assert_eq!(held.model, Some(HeldModel::Pickaxe));
        assert_eq!(app.world().resource::<Pickaxe>().0, t.tools.pickaxe);
        assert_eq!(app.world().resource::<SelectedMaterial>().0, None);

        // A number key picks its slot: the shovel swings with its profile
        // and is fast on soil only.
        let shovel_slot = slot_of(&bag, shovel);
        press(&mut app, HOTBAR_KEYS[usize::from(shovel_slot)]);
        assert_eq!(app.world().resource::<HotbarSlot>().0, shovel_slot);
        let held = app.world().resource::<Held>().clone();
        assert_eq!(held.thing, Some(shovel));
        assert_eq!(held.model, Some(HeldModel::Shovel));
        assert_eq!(held.speed.fast, ["soil"]);
        assert_eq!(
            app.world().resource::<Pickaxe>().0,
            *t.tools.profile("shovel").unwrap()
        );

        // Dirt in hand: right mouse places it, and the hand swings.
        press(&mut app, HOTBAR_KEYS[usize::from(dirt_slot)]);
        let held = app.world().resource::<Held>().clone();
        assert_eq!(held.thing, Some(Thing::Material(DIRT)));
        assert_eq!((held.model, held.speed), (None, ToolSpeed::default()));
        assert_eq!(app.world().resource::<SelectedMaterial>().0, Some(DIRT));
        assert_eq!(app.world().resource::<Pickaxe>().0, t.tools.hand.unwrap());

        // The wheel steps (down: next) and wraps; a key past the hotbar
        // does nothing.
        press(&mut app, KeyCode::Digit1);
        wheel(&mut app, 1.0);
        assert_eq!(app.world().resource::<HotbarSlot>().0, n - 1, "up wraps");
        wheel(&mut app, -1.0);
        assert_eq!(app.world().resource::<HotbarSlot>().0, 0, "down wraps");
        press(&mut app, NEXT_SLOT_KEY);
        assert_eq!(app.world().resource::<HotbarSlot>().0, 1);
        press(&mut app, PREV_SLOT_KEY);
        assert_eq!(app.world().resource::<HotbarSlot>().0, 0);

        // An empty slot is an empty hand.
        let mut empty = bag.clone();
        empty
            .move_stack(&t.items, SlotRef::Hotbar(0), SlotRef::Pack(0), None)
            .unwrap();
        app.insert_resource(ClientBag(Some(empty)));
        app.update();
        assert_eq!(*app.world().resource::<Held>(), Held::default());
        assert_eq!(app.world().resource::<SelectedMaterial>().0, None);

        // Not while the editor is open.
        app.insert_resource(EditorState {
            open: true,
            ..default()
        });
        press(&mut app, KeyCode::Digit2);
        assert_eq!(app.world().resource::<HotbarSlot>().0, 0);
    }

    #[test]
    fn hotbar_row_shows_the_slots_with_the_selection_outlined() {
        let t = tables();
        let bag = bag(&t);
        let mut app = app(true);
        app.update();
        app.update();
        let row = |app: &mut App| {
            let world = app.world_mut();
            let mut q = world.query_filtered::<(&HudSlot, &Node), With<HotbarHud>>();
            let v: Vec<(HudSlot, Display)> = q.iter(world).map(|(s, n)| (*s, n.display)).collect();
            v
        };
        assert_eq!(row(&mut app), [(HudSlot(HudAnchor::Bottom), Display::None)]);

        app.insert_resource(ClientBag(Some(bag.clone())));
        press(&mut app, KeyCode::Digit2);
        app.update();
        assert_eq!(row(&mut app), [(HudSlot(HudAnchor::Bottom), Display::Flex)]);
        let accent = app.world().resource::<UiTheme>().colors.accent;
        let world = app.world_mut();
        let mut q = world.query::<(&HotbarChip, &Node, &BorderColor)>();
        let mut chips: Vec<(u8, bool, bool)> = q
            .iter(world)
            .map(|(c, n, b)| {
                (
                    c.0,
                    n.display != Display::None,
                    *b == BorderColor::all(accent),
                )
            })
            .collect();
        chips.sort();
        assert_eq!(chips.len(), usize::from(rmc_world::items::MAX_HOTBAR));
        let shown = chips.iter().filter(|c| c.1).count();
        assert_eq!(shown, bag.hotbar().len());
        let outlined: Vec<u8> = chips.iter().filter(|c| c.2).map(|c| c.0).collect();
        assert_eq!(outlined, [1], "the selected slot");

        let mut q = world.query::<(&HotbarLine, &Text)>();
        let mut lines: Vec<(u8, u8, String)> = q
            .iter(world)
            .map(|(l, t)| (l.slot, l.line, t.0.clone()))
            .collect();
        lines.sort();
        let line = |slot: u8, n: u8| {
            lines
                .iter()
                .find(|l| l.0 == slot && l.1 == n)
                .unwrap()
                .2
                .clone()
        };
        assert_eq!(
            (line(0, 0), line(0, 1), line(0, 2)),
            ("1".into(), "Pickaxe".into(), String::new())
        );
        let dirt = slot_of(&bag, Thing::Material(DIRT));
        assert_eq!(line(dirt, 0), (dirt + 1).to_string());
        assert_eq!(line(dirt, 1), "#2", "a material without a replica");
        assert_eq!(line(dirt, 2), "40");
        assert!(
            bag.worn(EquipSlot::Head).is_some(),
            "worn things are not on the bar"
        );
    }
}
