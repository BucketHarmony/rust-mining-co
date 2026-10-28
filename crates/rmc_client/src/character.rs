//! The character screen: the bag, with drag and drop (owner extension,
//! `docs/ITEMS.md` I3).
//!
//! - [`CHARACTER_KEYS`] (`I`, `Tab`) open the screen ([`Screen::Character`])
//!   while there is a bag, the editor is closed and no other screen is
//!   open; they or Escape close it. Like any screen it frees the cursor
//!   ([`crate::cursor`]).
//! - The panel shows what is worn (the five [`EquipSlot`]s), the pack grid
//!   and the hotbar row: one [`BagSlotUi`] per slot, with the thing's name
//!   and, past one, its count. Under the worn slots, [`GearText`] sums what
//!   the worn things do (`docs/ITEMS.md` I4): their warmth and protection.
//! - Gestures are [`BagGesture`] messages. A drag dropped on a slot moves
//!   the stack there; with the right button, half of it (rounded up). A
//!   shift-click moves a stack between the hotbar and the pack (worn
//!   things go to the pack, else the hotbar): onto the same thing first,
//!   then the first empty slot.
//! - A gesture is predicted on [`ClientBag`] with [`Bag::move_stack`] and,
//!   only when that moves something, sent as `MoveItem`. The host answers
//!   every `MoveItem` with its `Bag`, which replaces the prediction. A move
//!   the bag refuses changes nothing and sends nothing, so the dragged
//!   stack snaps back.
//! - Bevy's pointer events on the slots ([`PointerDragStart`],
//!   [`PointerDragDrop`], [`PointerClick`], ...) are turned into gestures by
//!   the observers here; while a drag is on, a [`DragGhost`] follows the
//!   pointer and the source slot is outlined.

use bevy::picking::events::{
    PointerClick, PointerDrag, PointerDragDrop, PointerDragEnd, PointerDragStart,
};
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use rmc_protocol::ClientMsg;
use rmc_world::items::{Bag, EquipSlot, ItemTable, MAX_HOTBAR, SlotRef};

use crate::editor::EditorState;
use crate::hotbar::{ClientItems, thing_name};
use crate::hud::{ClientBag, TemperatureUnit, shown_unit};
use crate::net::ToHost;
use crate::replica::Replica;
use crate::ui::{
    ColorToken, Screen, ScreenBody, TextRole, UiFonts, UiScreens, UiSystems, UiTheme, text,
};

/// The keys that open and close the character screen.
pub const CHARACTER_KEYS: [KeyCode; 2] = [KeyCode::KeyI, KeyCode::Tab];

/// The keys that turn a click into a quick move.
const QUICK_MOVE_KEYS: [KeyCode; 2] = [KeyCode::ShiftLeft, KeyCode::ShiftRight];

/// A slot's edge, in button heights (the hotbar chip's width).
const SLOT_IN_BUTTONS: f32 = 1.6;

/// The text lines of a slot.
const NAME_LINE: u8 = 0;
const COUNT_LINE: u8 = 1;

/// What the player did to the bag on the character screen.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BagGesture {
    /// A stack dragged from `from` and dropped on `to`; `half` for the
    /// right button.
    Drop {
        from: SlotRef,
        to: SlotRef,
        half: bool,
    },
    /// A shift-click on `from`.
    QuickMove { from: SlotRef },
}

/// One slot on the character screen.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BagSlotUi(pub SlotRef);

/// A slot's text: [`NAME_LINE`] or [`COUNT_LINE`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BagSlotLine {
    pub slot: SlotRef,
    pub line: u8,
}

/// The line under the worn slots: warmth and protection.
#[derive(Component, Debug)]
pub struct GearText;

/// The stack under the pointer while a drag is on.
#[derive(Component, Debug)]
pub struct DragGhost;

/// The ghost's text.
#[derive(Component, Debug)]
struct DragGhostText;

/// The drag in progress, if any.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct Dragging(pub Option<Drag>);

/// A drag: from which slot, with which button, and where the pointer is
/// (logical pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    pub from: SlotRef,
    pub half: bool,
    pub at: Vec2,
}

/// [`CharacterPlugin`]'s systems, in `Update`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CharacterSystems;

/// The character screen (see the module docs). Needs the UI kit and
/// [`ClientItems`].
pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientBag>()
            .init_resource::<Dragging>()
            .add_message::<BagGesture>()
            .add_message::<ToHost>()
            .add_systems(
                Update,
                (
                    toggle_character.before(UiSystems),
                    (fill_character, apply_gestures, show_bag_slots, show_gear)
                        .chain()
                        .after(UiSystems),
                )
                    .in_set(CharacterSystems),
            )
            .add_observer(on_drag_start)
            .add_observer(on_drag)
            .add_observer(on_drag_end)
            .add_observer(on_drop)
            .add_observer(on_click);
    }
}

fn toggle_character(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    editor: Option<Res<EditorState>>,
    bag: Res<ClientBag>,
    screens: Option<ResMut<UiScreens>>,
) {
    let (Some(keys), Some(mut screens)) = (keys, screens) else {
        return;
    };
    if !keys.any_just_pressed(CHARACTER_KEYS) {
        return;
    }
    match screens.top() {
        Some(Screen::Character) => {
            screens.pop();
        }
        None if bag.0.is_some() && !editor.is_some_and(|e| e.open) => {
            screens.push(Screen::Character);
        }
        _ => {}
    }
}

/// The slot a shift-click on `from` moves to, if any.
fn quick_target(bag: &Bag, table: &ItemTable, from: SlotRef) -> Option<SlotRef> {
    let stack = bag.get(from)?;
    let hotbar = (0..bag.hotbar().len()).map(|i| SlotRef::Hotbar(i as u8));
    let pack = (0..bag.pack().len()).map(|i| SlotRef::Pack(i as u16));
    let candidates: Vec<SlotRef> = match from {
        SlotRef::Hotbar(_) => pack.collect(),
        SlotRef::Pack(_) => hotbar.collect(),
        SlotRef::Equip(_) => pack.chain(hotbar).collect(),
    };
    let same = candidates
        .iter()
        .filter(|&&s| bag.get(s).is_some_and(|d| d.thing == stack.thing));
    let empty = candidates.iter().filter(|&&s| bag.get(s).is_none());
    same.chain(empty)
        .copied()
        .find(|&to| bag.clone().move_stack(table, from, to, None).is_ok())
}

/// The `MoveItem` a gesture asks for: from, to and count (`None` for the
/// whole stack). `None` when there is nothing to move or nowhere to go.
pub fn gesture_move(
    bag: &Bag,
    table: &ItemTable,
    gesture: BagGesture,
) -> Option<(SlotRef, SlotRef, Option<u32>)> {
    match gesture {
        BagGesture::Drop { from, to, half } => {
            let stack = bag.get(from)?;
            let count = (half && stack.count > 1).then(|| stack.count.div_ceil(2));
            Some((from, to, count))
        }
        BagGesture::QuickMove { from } => quick_target(bag, table, from).map(|to| (from, to, None)),
    }
}

/// Predicts each gesture on the bag and sends the moves that happened.
fn apply_gestures(
    mut gestures: MessageReader<BagGesture>,
    tables: Option<Res<ClientItems>>,
    mut bag: ResMut<ClientBag>,
    mut out: MessageWriter<ToHost>,
) {
    let (Some(tables), Some(bag)) = (tables, bag.0.as_mut()) else {
        gestures.clear();
        return;
    };
    for &gesture in gestures.read() {
        let Some((from, to, count)) = gesture_move(bag, &tables.items, gesture) else {
            continue;
        };
        match bag.move_stack(&tables.items, from, to, count) {
            Ok(moved) => {
                debug!(?from, ?to, moved, "bag move");
                out.write(ToHost(ClientMsg::MoveItem { from, to, count }));
            }
            Err(why) => debug!(?from, ?to, %why, "bag move refused"),
        }
    }
}

/// A slot: a square with the thing's name and count.
fn spawn_slot(commands: &mut Commands, t: &UiTheme, f: &UiFonts, slot: SlotRef, parent: Entity) {
    let size = t.button.height * SLOT_IN_BUTTONS;
    let line = |line: u8, color: ColorToken| {
        (
            BagSlotLine { slot, line },
            text(t, f, TextRole::Caption, color, ""),
            Pickable::IGNORE,
        )
    };
    commands.spawn((
        BagSlotUi(slot),
        Node {
            width: Val::Px(size),
            height: Val::Px(size),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            padding: UiRect::all(Val::Px(t.space.xs)),
            border: UiRect::all(Val::Px(t.border * 2.0)),
            border_radius: BorderRadius::all(Val::Px(t.radius.md)),
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(t.colors.surface),
        BorderColor::all(t.colors.border),
        ChildOf(parent),
        children![
            line(NAME_LINE, ColorToken::Text),
            line(COUNT_LINE, ColorToken::TextDim),
        ],
    ));
}

/// Fills a freshly drawn character panel: what is worn, the pack, the
/// hotbar, a hint, and the drag ghost over it all.
fn fill_character(
    mut commands: Commands,
    mut bodies: Query<(Entity, &ScreenBody, &ChildOf, &mut Node), Added<ScreenBody>>,
    bag: Res<ClientBag>,
    theme: Option<Res<UiTheme>>,
    fonts: Option<Res<UiFonts>>,
) {
    let (Some(theme), Some(fonts)) = (theme, fonts) else {
        return;
    };
    let (t, f) = (&*theme, &*fonts);
    let s = &t.space;
    for (body, screen, root, mut node) in &mut bodies {
        if screen.0 != Screen::Character {
            continue;
        }
        node.width = Val::Auto;
        let (hotbar, pack) = bag
            .0
            .as_ref()
            .map_or((0, 0), |b| (b.hotbar().len(), b.pack().len()));
        let caption = |s: &'static str| text(t, f, TextRole::Caption, ColorToken::TextDim, s);
        commands.spawn((
            text(t, f, TextRole::Title, ColorToken::Text, "Character"),
            ChildOf(body),
        ));
        let columns = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(s.xl),
                    ..default()
                },
                ChildOf(body),
            ))
            .id();
        let column = |commands: &mut Commands| {
            commands
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(s.sm),
                        ..default()
                    },
                    ChildOf(columns),
                ))
                .id()
        };

        let worn = column(&mut commands);
        commands.spawn((caption("Worn"), ChildOf(worn)));
        for e in EquipSlot::ALL {
            let row = commands
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(s.sm),
                        ..default()
                    },
                    ChildOf(worn),
                ))
                .id();
            spawn_slot(&mut commands, t, f, SlotRef::Equip(e), row);
            commands.spawn((
                text(t, f, TextRole::Label, ColorToken::Text, format!("{e:?}")),
                ChildOf(row),
            ));
        }
        commands.spawn((
            text(t, f, TextRole::Caption, ColorToken::Text, ""),
            GearText,
            ChildOf(worn),
        ));

        let bag_column = column(&mut commands);
        let grid = |commands: &mut Commands| {
            commands
                .spawn((
                    Node {
                        display: Display::Grid,
                        grid_template_columns: vec![RepeatedGridTrack::px(
                            u16::from(MAX_HOTBAR),
                            t.button.height * SLOT_IN_BUTTONS,
                        )],
                        column_gap: Val::Px(s.xs),
                        row_gap: Val::Px(s.xs),
                        ..default()
                    },
                    ChildOf(bag_column),
                ))
                .id()
        };
        commands.spawn((caption("Pack"), ChildOf(bag_column)));
        let pack_grid = grid(&mut commands);
        for i in 0..pack {
            spawn_slot(&mut commands, t, f, SlotRef::Pack(i as u16), pack_grid);
        }
        commands.spawn((caption("Hotbar"), ChildOf(bag_column)));
        let hotbar_row = grid(&mut commands);
        for i in 0..hotbar {
            spawn_slot(&mut commands, t, f, SlotRef::Hotbar(i as u8), hotbar_row);
        }
        commands.spawn((
            caption("Drag to move; right-drag moves half; shift-click: hotbar / pack"),
            ChildOf(body),
        ));

        commands.spawn((
            DragGhost,
            Node {
                position_type: PositionType::Absolute,
                display: Display::None,
                padding: UiRect::axes(Val::Px(s.sm), Val::Px(s.xs)),
                border: UiRect::all(Val::Px(t.border * 2.0)),
                border_radius: BorderRadius::all(Val::Px(t.radius.md)),
                ..default()
            },
            BackgroundColor(t.colors.surface_raised),
            BorderColor::all(t.colors.accent),
            GlobalZIndex(i32::MAX),
            Pickable::IGNORE,
            ChildOf(root.parent()),
            children![(
                DragGhostText,
                text(t, f, TextRole::Label, ColorToken::Text, ""),
                Pickable::IGNORE,
            )],
        ));
    }
}

/// Shows the bag in the slots, outlines the dragged slot and moves the
/// ghost.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn show_bag_slots(
    bag: Res<ClientBag>,
    dragging: Res<Dragging>,
    tables: Option<Res<ClientItems>>,
    replica: Option<Res<Replica>>,
    theme: Option<Res<UiTheme>>,
    ui_scale: Option<Res<UiScale>>,
    mut slots: Query<(&BagSlotUi, &mut BorderColor, &mut BackgroundColor), Without<DragGhost>>,
    mut lines: Query<(&BagSlotLine, &mut Text), Without<DragGhostText>>,
    mut ghosts: Query<&mut Node, With<DragGhost>>,
    mut ghost_text: Query<&mut Text, With<DragGhostText>>,
) {
    let (Some(theme), Some(tables)) = (theme, tables) else {
        return;
    };
    let materials = replica.as_deref().map(|r| &**r.world().materials());
    let stack_at = |slot: SlotRef| bag.0.as_ref().and_then(|b| b.get(slot));
    let dragged = dragging.0.map(|d| d.from);
    for (slot, mut border, mut fill) in &mut slots {
        let edge = if dragged == Some(slot.0) {
            theme.colors.accent
        } else {
            theme.colors.border
        };
        let surface = if stack_at(slot.0).is_some() {
            theme.colors.surface_raised
        } else {
            theme.colors.surface
        };
        border.set_if_neq(BorderColor::all(edge));
        fill.set_if_neq(BackgroundColor(surface));
    }
    for (line, mut t) in &mut lines {
        let value = stack_at(line.slot).map_or_else(String::new, |s| match line.line {
            NAME_LINE => thing_name(s.thing, &tables.items, materials),
            _ if s.count > 1 => s.count.to_string(),
            _ => String::new(),
        });
        if t.0 != value {
            t.0 = value;
        }
    }

    // The ghost: the dragged stack's name and the count it would move.
    let ghost = dragging.0.and_then(|d| Some((d, stack_at(d.from)?)));
    let scale = ui_scale.map_or(1.0, |s| s.0);
    for mut node in &mut ghosts {
        let Some((drag, _)) = ghost else {
            if node.display != Display::None {
                node.display = Display::None;
            }
            continue;
        };
        node.display = Display::Flex;
        node.left = Val::Px(drag.at.x / scale + theme.space.sm);
        node.top = Val::Px(drag.at.y / scale + theme.space.sm);
    }
    if let Some((drag, stack)) = ghost {
        let count = if drag.half {
            stack.count.div_ceil(2)
        } else {
            stack.count
        };
        let name = thing_name(stack.thing, &tables.items, materials);
        let value = if count > 1 {
            format!("{name} x{count}")
        } else {
            name
        };
        for mut t in &mut ghost_text {
            if t.0 != value {
                t.0.clone_from(&value);
            }
        }
    }
}

/// What the worn things do, in Celsius (see [`gear_line_in`]).
pub fn gear_line(bag: &Bag, items: &ItemTable) -> String {
    gear_line_in(bag, items, TemperatureUnit::Celsius)
}

/// What the worn things do, as [`GearText`] shows it, warmth in `unit`.
pub fn gear_line_in(bag: &Bag, items: &ItemTable, unit: TemperatureUnit) -> String {
    let warmth = bag
        .warmth(items)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    format!(
        "Warmth {}
Protection {}",
        unit.format_delta(warmth),
        bag.protection(items)
    )
}

fn show_gear(
    bag: Res<ClientBag>,
    tables: Option<Res<ClientItems>>,
    mut lines: Query<&mut Text, With<GearText>>,
    unit: Option<Res<TemperatureUnit>>,
) {
    let unit = shown_unit(unit.as_deref());
    let value = match (&bag.0, tables) {
        (Some(bag), Some(t)) => gear_line_in(bag, &t.items, unit),
        _ => String::new(),
    };
    for mut t in &mut lines {
        if t.0 != value {
            t.0.clone_from(&value);
        }
    }
}

fn on_drag_start(
    mut ev: On<PointerDragStart>,
    slots: Query<&BagSlotUi>,
    bag: Res<ClientBag>,
    mut dragging: ResMut<Dragging>,
) {
    let Ok(slot) = slots.get(ev.entity) else {
        return;
    };
    ev.propagate(false);
    let full = bag.0.as_ref().and_then(|b| b.get(slot.0)).is_some();
    dragging.0 = full.then_some(Drag {
        from: slot.0,
        half: ev.button == PointerButton::Secondary,
        at: ev.pointer.position,
    });
}

fn on_drag(
    mut ev: On<PointerDrag>,
    slots: Query<(), With<BagSlotUi>>,
    mut dragging: ResMut<Dragging>,
) {
    if !slots.contains(ev.entity) {
        return;
    }
    ev.propagate(false);
    if let Some(drag) = &mut dragging.0 {
        drag.at = ev.pointer.position;
    }
}

fn on_drag_end(
    mut ev: On<PointerDragEnd>,
    slots: Query<(), With<BagSlotUi>>,
    mut dragging: ResMut<Dragging>,
) {
    if !slots.contains(ev.entity) {
        return;
    }
    ev.propagate(false);
    dragging.0 = None;
}

fn on_drop(
    mut ev: On<PointerDragDrop>,
    slots: Query<&BagSlotUi>,
    mut gestures: MessageWriter<BagGesture>,
) {
    let (Ok(to), Ok(from)) = (slots.get(ev.entity), slots.get(ev.dropped)) else {
        return;
    };
    ev.propagate(false);
    gestures.write(BagGesture::Drop {
        from: from.0,
        to: to.0,
        half: ev.button == PointerButton::Secondary,
    });
}

fn on_click(
    mut ev: On<PointerClick>,
    slots: Query<&BagSlotUi>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut gestures: MessageWriter<BagGesture>,
) {
    let Ok(slot) = slots.get(ev.entity) else {
        return;
    };
    ev.propagate(false);
    let shift = keys.is_some_and(|k| k.any_pressed(QUICK_MOVE_KEYS));
    if ev.button == PointerButton::Primary && shift {
        gestures.write(BagGesture::QuickMove { from: slot.0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::{CursorGrab, CursorPlugin};
    use crate::hud::format_temperature;
    use crate::ui::UiKitPlugin;
    use crate::ui::theme::tests::THEME_PATH;
    use bevy::camera::NormalizedRenderTarget;
    use bevy::picking::backend::HitData;
    use bevy::picking::pointer::{Location, PointerId};
    use rmc_world::ToolTable;
    use rmc_world::items::{Stack, Thing};
    use rmc_world::materials::MaterialId;

    const ITEMS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/items.ron");
    const TOOLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tools.ron");
    const DIRT: MaterialId = MaterialId(2);

    fn tables() -> ClientItems {
        ClientItems {
            items: ItemTable::load(ITEMS_PATH).unwrap(),
            tools: ToolTable::load(TOOLS_PATH).unwrap(),
        }
    }

    /// The starting kit plus 40 dirt.
    fn bag(t: &ClientItems) -> Bag {
        let mut bag = Bag::starter(&t.items);
        bag.insert(&t.items, Thing::Material(DIRT), 40);
        bag
    }

    fn slot_of(bag: &Bag, thing: Thing) -> SlotRef {
        let i = bag
            .hotbar()
            .iter()
            .position(|s| s.is_some_and(|s| s.thing == thing))
            .unwrap();
        SlotRef::Hotbar(i as u8)
    }

    /// What was sent to the host.
    #[derive(Resource, Default)]
    struct Sent(Vec<ClientMsg>);

    fn record(mut out: MessageReader<ToHost>, mut sent: ResMut<Sent>) {
        sent.0.extend(out.read().map(|m| m.0.clone()));
    }

    fn app(with_bag: bool) -> App {
        let t = tables();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<Sent>()
            .add_plugins(UiKitPlugin {
                theme: UiTheme::load(THEME_PATH).unwrap(),
                assets_dir: None,
            })
            .add_plugins(CharacterPlugin)
            .add_systems(PostUpdate, record);
        // The gear test reads Celsius; the kit's default is Fahrenheit.
        app.insert_resource(TemperatureUnit::Celsius);
        if with_bag {
            app.insert_resource(ClientBag(Some(bag(&t))));
        }
        app.insert_resource(t);
        app.update();
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
        app.update();
    }

    fn top(app: &App) -> Option<Screen> {
        app.world().resource::<UiScreens>().top()
    }

    fn the_bag(app: &App) -> Bag {
        app.world().resource::<ClientBag>().0.clone().unwrap()
    }

    fn sent(app: &mut App) -> Vec<ClientMsg> {
        std::mem::take(&mut app.world_mut().resource_mut::<Sent>().0)
    }

    fn gesture(app: &mut App, g: BagGesture) {
        app.world_mut().write_message(g);
        app.update();
    }

    /// The slot widget for `slot`.
    fn slot_entity(app: &mut App, slot: SlotRef) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &BagSlotUi)>();
        q.iter(world).find(|(_, s)| s.0 == slot).unwrap().0
    }

    #[test]
    fn i_and_tab_open_and_close_the_character_screen_and_free_the_cursor() {
        // Without a bag the keys do nothing.
        let mut bare = app(false);
        press(&mut bare, KeyCode::KeyI);
        assert_eq!(top(&bare), None);

        let mut app = app(true);
        app.add_plugins(CursorPlugin);
        app.update();
        let grabbed = |app: &App| app.world().resource::<CursorGrab>().grabbed();
        assert!(grabbed(&app));

        press(&mut app, KeyCode::KeyI);
        assert_eq!(top(&app), Some(Screen::Character));
        assert!(!grabbed(&app), "the screen frees the cursor");
        let world = app.world_mut();
        let slots = world.query::<&BagSlotUi>().iter(world).count();
        let b = the_bag(&app);
        assert_eq!(
            slots,
            b.hotbar().len() + b.pack().len() + EquipSlot::ALL.len()
        );

        press(&mut app, KeyCode::KeyI);
        assert_eq!(top(&app), None);
        assert!(grabbed(&app), "closing grabs it again");

        press(&mut app, KeyCode::Tab);
        assert_eq!(top(&app), Some(Screen::Character));
        press(&mut app, KeyCode::Escape);
        assert_eq!(top(&app), None, "Escape closes it");
        let world = app.world_mut();
        assert_eq!(world.query::<&BagSlotUi>().iter(world).count(), 0);

        // Not over another screen, nor with the editor open.
        app.world_mut()
            .resource_mut::<UiScreens>()
            .push(Screen::Menu);
        press(&mut app, KeyCode::KeyI);
        assert_eq!(top(&app), Some(Screen::Menu));
        app.world_mut().resource_mut::<UiScreens>().clear();
        app.insert_resource(EditorState {
            open: true,
            ..default()
        });
        press(&mut app, KeyCode::KeyI);
        assert_eq!(top(&app), None);
    }

    #[test]
    fn drops_predict_the_move_and_send_it() {
        let t = tables();
        let mut app = app(true);
        let start = the_bag(&app);
        let pick = Thing::Item(t.items.id("pickaxe").unwrap());
        let dirt = slot_of(&start, Thing::Material(DIRT));

        // The whole stack, to an empty pack slot.
        let from = slot_of(&start, pick);
        let to = SlotRef::Pack(3);
        gesture(
            &mut app,
            BagGesture::Drop {
                from,
                to,
                half: false,
            },
        );
        let mut want = start.clone();
        want.move_stack(&t.items, from, to, None).unwrap();
        assert_eq!(the_bag(&app), want, "predicted");
        assert_eq!(
            sent(&mut app),
            [ClientMsg::MoveItem {
                from,
                to,
                count: None
            }]
        );

        // Right-drag: half, rounded up.
        gesture(
            &mut app,
            BagGesture::Drop {
                from: dirt,
                to: SlotRef::Pack(0),
                half: true,
            },
        );
        assert_eq!(
            sent(&mut app),
            [ClientMsg::MoveItem {
                from: dirt,
                to: SlotRef::Pack(0),
                count: Some(20)
            }]
        );
        let b = the_bag(&app);
        assert_eq!(b.get(dirt).unwrap().count, 20);
        assert_eq!(b.get(SlotRef::Pack(0)).unwrap().count, 20);

        // Shift-click on the hotbar's dirt: onto the pack's dirt first.
        gesture(&mut app, BagGesture::QuickMove { from: dirt });
        assert_eq!(
            sent(&mut app),
            [ClientMsg::MoveItem {
                from: dirt,
                to: SlotRef::Pack(0),
                count: None
            }]
        );
        assert_eq!(the_bag(&app).get(dirt), None);
        assert_eq!(
            the_bag(&app).get(SlotRef::Pack(0)),
            Some(Stack {
                thing: Thing::Material(DIRT),
                count: 40
            })
        );
        // And back from the pack to the first empty hotbar slot (the
        // pickaxe's, now).
        let first_empty = the_bag(&app)
            .hotbar()
            .iter()
            .position(Option::is_none)
            .unwrap();
        assert_eq!(SlotRef::Hotbar(first_empty as u8), from);
        gesture(
            &mut app,
            BagGesture::QuickMove {
                from: SlotRef::Pack(0),
            },
        );
        let [ClientMsg::MoveItem { to, .. }] = sent(&mut app)[..] else {
            panic!("one move");
        };
        assert_eq!(to, SlotRef::Hotbar(first_empty as u8));
    }

    #[test]
    fn refused_drops_leave_the_bag_and_send_nothing() {
        let t = tables();
        let mut app = app(true);
        let start = the_bag(&app);
        let pick = slot_of(&start, Thing::Item(t.items.id("pickaxe").unwrap()));
        let refused = [
            // A pickaxe is not worn on the head.
            BagGesture::Drop {
                from: pick,
                to: SlotRef::Equip(EquipSlot::Head),
                half: false,
            },
            // Nothing to drag.
            BagGesture::Drop {
                from: SlotRef::Pack(5),
                to: SlotRef::Pack(6),
                half: false,
            },
            // Onto itself.
            BagGesture::Drop {
                from: pick,
                to: pick,
                half: false,
            },
            // An empty slot.
            BagGesture::QuickMove {
                from: SlotRef::Pack(9),
            },
        ];
        for g in refused {
            gesture(&mut app, g);
            assert_eq!(the_bag(&app), start, "{g:?}");
            assert_eq!(sent(&mut app), [], "{g:?}");
        }
    }

    #[test]
    fn equipment_slots_show_what_is_worn() {
        let t = tables();
        let mut app = app(true);
        press(&mut app, KeyCode::KeyI);
        let b = the_bag(&app);
        let world = app.world_mut();
        let mut q = world.query::<(&BagSlotLine, &Text)>();
        let mut line = |slot: SlotRef, n: u8| {
            q.iter(world)
                .find(|(l, _)| l.slot == slot && l.line == n)
                .map(|(_, t)| t.0.clone())
                .unwrap()
        };
        let mut worn = 0;
        for e in EquipSlot::ALL {
            let want = b
                .worn(e)
                .map_or_else(String::new, |s| thing_name(s.thing, &t.items, None));
            worn += usize::from(!want.is_empty());
            assert_eq!(line(SlotRef::Equip(e), NAME_LINE), want, "{e:?}");
        }
        assert!(worn > 0, "the starting kit wears something");
        let dirt = slot_of(&b, Thing::Material(DIRT));
        assert_eq!(line(dirt, NAME_LINE), "#2");
        assert_eq!(line(dirt, COUNT_LINE), "40");
        assert_eq!(line(SlotRef::Pack(0), NAME_LINE), "");
    }

    #[test]
    fn gear_line_shows_worn_warmth_and_protection() {
        let t = tables();
        let mut app = app(true);
        press(&mut app, KeyCode::KeyI);
        let gear = |app: &mut App| {
            let world = app.world_mut();
            world
                .query_filtered::<&Text, With<GearText>>()
                .single(world)
                .unwrap()
                .0
                .clone()
        };
        let b = the_bag(&app);
        let want = format!(
            "Warmth +{}
Protection {}",
            format_temperature(b.warmth(&t.items) as i16),
            b.protection(&t.items)
        );
        assert_eq!(gear(&mut app), want);
        assert_eq!(gear_line(&b, &t.items), want);

        // The hard hat on instead of the cap: warmer or not, it protects more.
        let hat = slot_of(&b, Thing::Item(t.items.id("hard_hat").unwrap()));
        gesture(
            &mut app,
            BagGesture::Drop {
                from: hat,
                to: SlotRef::Equip(EquipSlot::Head),
                half: false,
            },
        );
        let b2 = the_bag(&app);
        assert!(b2.protection(&t.items) > b.protection(&t.items));
        assert_eq!(gear(&mut app), gear_line(&b2, &t.items));
        assert_ne!(gear(&mut app), want);
    }

    fn pointer() -> bevy::picking::events::Pointer {
        bevy::picking::events::Pointer::new(
            PointerId::Mouse,
            Location {
                target: NormalizedRenderTarget::None {
                    width: 1,
                    height: 1,
                },
                position: Vec2::new(40.0, 30.0),
            },
        )
    }

    fn hit() -> HitData {
        HitData::new(Entity::PLACEHOLDER, 0.0, None, None)
    }

    #[test]
    fn pointer_drags_and_shift_clicks_become_moves() {
        let t = tables();
        let mut app = app(true);
        press(&mut app, KeyCode::KeyI);
        let start = the_bag(&app);
        let pick = slot_of(&start, Thing::Item(t.items.id("pickaxe").unwrap()));
        let (src, dst) = (
            slot_entity(&mut app, pick),
            slot_entity(&mut app, SlotRef::Pack(2)),
        );

        app.world_mut().trigger(PointerDragStart {
            entity: src,
            pointer: pointer(),
            button: PointerButton::Primary,
            hit: hit(),
        });
        app.update();
        let drag = app.world().resource::<Dragging>().0.unwrap();
        assert_eq!((drag.from, drag.half), (pick, false));
        let world = app.world_mut();
        let ghost = world
            .query_filtered::<&Node, With<DragGhost>>()
            .single(world)
            .unwrap()
            .display;
        assert_eq!(ghost, Display::Flex, "the ghost follows the pointer");

        app.world_mut().trigger(PointerDragDrop {
            entity: dst,
            pointer: pointer(),
            button: PointerButton::Primary,
            dropped: src,
            hit: hit(),
        });
        app.world_mut().trigger(PointerDragEnd {
            entity: src,
            pointer: pointer(),
            button: PointerButton::Primary,
            distance: Vec2::ONE,
        });
        app.update();
        assert_eq!(app.world().resource::<Dragging>().0, None);
        assert_eq!(
            sent(&mut app),
            [ClientMsg::MoveItem {
                from: pick,
                to: SlotRef::Pack(2),
                count: None
            }]
        );
        assert_eq!(the_bag(&app).get(pick), None);

        // A plain click does nothing; a shift-click moves it back.
        let click = |app: &mut App| {
            app.world_mut().trigger(PointerClick {
                entity: dst,
                pointer: pointer(),
                button: PointerButton::Primary,
                hit: hit(),
                duration: Default::default(),
                count: 1,
            });
            app.update();
        };
        click(&mut app);
        assert_eq!(sent(&mut app), []);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ShiftLeft);
        click(&mut app);
        let [ClientMsg::MoveItem { from, .. }] = sent(&mut app)[..] else {
            panic!("one move");
        };
        assert_eq!(from, SlotRef::Pack(2));
    }
}
