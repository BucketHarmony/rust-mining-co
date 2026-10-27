//! Items and the bag (owner extension, `docs/ITEMS.md` I1).
//!
//! `assets/items.ron` lists the items that are not materials (tools,
//! clothing, armor, ...) as [`ItemDef`] rows, the size of a [`Bag`], the
//! largest stack of a material and the starting kit. An item's id is its row
//! index, so host and client must load the same file; like `tools.ron`, it
//! is not in the material hash.
//!
//! A [`Thing`] is a material or an item, and a [`Stack`] is a thing and a
//! count. A bag holds stacks in hotbar slots (the tool menu), pack slots and
//! one per [`EquipSlot`]. An equipment slot holds a single item whose row
//! names that slot; hotbar and pack slots hold anything, up to the thing's
//! stack limit.
//!
//! - [`Bag::insert`] tops up stacks of the same thing (hotbar, then pack),
//!   then fills empty hotbar slots, then empty pack slots, and returns how
//!   many did not fit.
//! - [`Bag::remove`] takes from the hotbar, then the pack, then what is worn.
//! - [`Bag::move_stack`] moves into an empty slot, onto the same thing (as
//!   many as fit; the rest stays), or swaps two whole stacks when each slot
//!   takes the other's thing. Anything else is refused and changes nothing.
//!
//! Every scan is in slot order, so a bag's contents depend only on the
//! operations applied to it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::materials::MaterialId;

/// An item: its row in [`ItemTable`].
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ItemId(pub u16);

/// Where on the body an item is worn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EquipSlot {
    Head,
    Body,
    Legs,
    Feet,
    Hands,
}

impl EquipSlot {
    /// Every equipment slot, in display order.
    pub const ALL: [EquipSlot; 5] = [
        EquipSlot::Head,
        EquipSlot::Body,
        EquipSlot::Legs,
        EquipSlot::Feet,
        EquipSlot::Hands,
    ];
}

/// One row of `items.ron`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemDef {
    /// Stable name, unique in the table.
    pub key: String,
    /// What the HUD shows.
    pub name: String,
    /// `tool`, `clothing`, `armor`, ...: code filters on these.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The largest stack. At least 1.
    pub stack: u32,
    /// The `tools.ron` profile a tool swings with.
    #[serde(default)]
    pub tool: Option<String>,
    /// The equipment slot a wearable goes in.
    #[serde(default)]
    pub slot: Option<EquipSlot>,
}

/// How many hotbar and pack slots a bag has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BagSize {
    /// 1 to [`MAX_HOTBAR`]: the number keys select them.
    pub hotbar: u8,
    pub pack: u16,
}

/// The most hotbar slots: one per number key 1–9.
pub const MAX_HOTBAR: u8 = 9;

/// The contents of `items.ron`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemFile {
    bag: BagSize,
    material_stack: u32,
    items: Vec<ItemDef>,
    #[serde(default)]
    start: Vec<(String, u32)>,
}

/// Every item, the bag's size, the largest material stack and the starting
/// kit: the contents of `items.ron`, validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTable {
    rows: Vec<ItemDef>,
    by_key: BTreeMap<String, ItemId>,
    bag: BagSize,
    material_stack: u32,
    start: Vec<(ItemId, u32)>,
}

#[derive(Debug, Error)]
pub enum ItemError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing items RON: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("item {0:?}: an empty key or name")]
    Empty(String),
    #[error("item {0:?} is listed twice")]
    Duplicate(String),
    #[error("item {0:?}: stack must be at least 1")]
    NoStack(String),
    #[error("item {0:?}: an empty tool profile")]
    EmptyTool(String),
    #[error("bag: {0}")]
    BadBag(String),
    #[error("material_stack must be at least 1")]
    NoMaterialStack,
    #[error("the starting kit names {0:?}, which is not an item")]
    UnknownStart(String),
    #[error("the starting kit gives no {0:?}")]
    EmptyStart(String),
    #[error("too many items: {0}")]
    TooMany(usize),
}

impl ItemTable {
    /// Loads and validates the table from a RON file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ItemError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ItemError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let table = Self::from_ron_str(&text)?;
        tracing::info!(path = %path.display(), items = table.rows.len(), "item table loaded");
        Ok(table)
    }

    /// Parses and validates the table from RON text.
    pub fn from_ron_str(text: &str) -> Result<Self, ItemError> {
        let file: ItemFile = ron::from_str(text)?;
        if file.bag.hotbar == 0 || file.bag.hotbar > MAX_HOTBAR {
            return Err(ItemError::BadBag(format!(
                "hotbar is {}; it must be 1 to {MAX_HOTBAR}",
                file.bag.hotbar
            )));
        }
        if file.material_stack == 0 {
            return Err(ItemError::NoMaterialStack);
        }
        if file.items.len() > usize::from(u16::MAX) {
            return Err(ItemError::TooMany(file.items.len()));
        }
        let mut by_key = BTreeMap::new();
        for (i, row) in file.items.iter().enumerate() {
            if row.key.is_empty() || row.name.is_empty() {
                return Err(ItemError::Empty(row.key.clone()));
            }
            if row.stack == 0 {
                return Err(ItemError::NoStack(row.key.clone()));
            }
            if row.tool.as_deref() == Some("") {
                return Err(ItemError::EmptyTool(row.key.clone()));
            }
            if by_key.insert(row.key.clone(), ItemId(i as u16)).is_some() {
                return Err(ItemError::Duplicate(row.key.clone()));
            }
        }
        let mut start = Vec::with_capacity(file.start.len());
        for (key, count) in file.start {
            let id = *by_key
                .get(&key)
                .ok_or(ItemError::UnknownStart(key.clone()))?;
            if count == 0 {
                return Err(ItemError::EmptyStart(key));
            }
            start.push((id, count));
        }
        Ok(Self {
            rows: file.items,
            by_key,
            bag: file.bag,
            material_stack: file.material_stack,
            start,
        })
    }

    /// The item named `key`.
    pub fn id(&self, key: &str) -> Option<ItemId> {
        self.by_key.get(key).copied()
    }

    /// Item `id`'s row.
    pub fn get(&self, id: ItemId) -> Option<&ItemDef> {
        self.rows.get(usize::from(id.0))
    }

    /// Every row with its id, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (ItemId, &ItemDef)> {
        self.rows
            .iter()
            .enumerate()
            .map(|(i, row)| (ItemId(i as u16), row))
    }

    /// The bag's size.
    pub fn bag(&self) -> BagSize {
        self.bag
    }

    /// The largest stack of `thing`. An unknown item stacks to 1.
    pub fn stack_limit(&self, thing: Thing) -> u32 {
        match thing {
            Thing::Material(_) => self.material_stack,
            Thing::Item(id) => self.get(id).map_or(1, |row| row.stack),
        }
    }

    /// The equipment slot `thing` is worn in, if any.
    pub fn slot_of(&self, thing: Thing) -> Option<EquipSlot> {
        match thing {
            Thing::Material(_) => None,
            Thing::Item(id) => self.get(id).and_then(|row| row.slot),
        }
    }

    /// The starting kit, in file order.
    pub fn start(&self) -> &[(ItemId, u32)] {
        &self.start
    }
}

/// A material or an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Thing {
    Material(MaterialId),
    Item(ItemId),
}

/// Some of one thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stack {
    pub thing: Thing,
    /// At least 1.
    pub count: u32,
}

/// A slot in a bag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SlotRef {
    Hotbar(u8),
    Pack(u16),
    Equip(EquipSlot),
}

/// Why [`Bag::move_stack`] moved nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum MoveError {
    #[error("no such slot")]
    NoSuchSlot,
    #[error("from and to are the same slot")]
    SameSlot,
    #[error("nothing to move")]
    Empty,
    #[error("the count is 0 or more than the stack holds")]
    BadCount,
    #[error("the slot does not take that")]
    WrongSlot,
    #[error("the stack there is full")]
    Full,
    #[error("the slot holds something else")]
    Occupied,
}

/// One player's things: hotbar, pack and what is worn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bag {
    hotbar: Vec<Option<Stack>>,
    pack: Vec<Option<Stack>>,
    equip: BTreeMap<EquipSlot, Stack>,
}

impl Bag {
    /// An empty bag of `size`.
    pub fn new(size: BagSize) -> Self {
        Self {
            hotbar: vec![None; usize::from(size.hotbar)],
            pack: vec![None; usize::from(size.pack)],
            equip: BTreeMap::new(),
        }
    }

    /// A new player's bag: the starting kit, each wearable put on if its
    /// slot is free and everything else inserted, in kit order. What does
    /// not fit is left out.
    pub fn starter(table: &ItemTable) -> Self {
        let mut bag = Self::new(table.bag());
        for &(id, count) in table.start() {
            let thing = Thing::Item(id);
            let mut count = count;
            if let Some(slot) = table.slot_of(thing)
                && !bag.equip.contains_key(&slot)
            {
                bag.equip.insert(slot, Stack { thing, count: 1 });
                count -= 1;
            }
            bag.insert(table, thing, count);
        }
        bag
    }

    /// The hotbar slots, in order.
    pub fn hotbar(&self) -> &[Option<Stack>] {
        &self.hotbar
    }

    /// The pack slots, in order.
    pub fn pack(&self) -> &[Option<Stack>] {
        &self.pack
    }

    /// What is worn in `slot`.
    pub fn worn(&self, slot: EquipSlot) -> Option<Stack> {
        self.equip.get(&slot).copied()
    }

    /// The stack in `slot`, if the slot exists and holds one.
    pub fn get(&self, slot: SlotRef) -> Option<Stack> {
        match slot {
            SlotRef::Hotbar(i) => self.hotbar.get(usize::from(i)).copied().flatten(),
            SlotRef::Pack(i) => self.pack.get(usize::from(i)).copied().flatten(),
            SlotRef::Equip(e) => self.worn(e),
        }
    }

    /// Whether the bag has `slot`.
    pub fn has_slot(&self, slot: SlotRef) -> bool {
        match slot {
            SlotRef::Hotbar(i) => usize::from(i) < self.hotbar.len(),
            SlotRef::Pack(i) => usize::from(i) < self.pack.len(),
            SlotRef::Equip(_) => true,
        }
    }

    /// How many of `thing` the bag holds, worn included.
    pub fn count(&self, thing: Thing) -> u32 {
        self.stacks()
            .filter(|s| s.thing == thing)
            .fold(0u32, |sum, s| sum.saturating_add(s.count))
    }

    /// Every material held and its total, ascending by id: what
    /// `ServerMsg::Inventory` carries.
    pub fn materials(&self) -> Vec<(MaterialId, u32)> {
        let mut totals: BTreeMap<MaterialId, u32> = BTreeMap::new();
        for s in self.stacks() {
            if let Thing::Material(id) = s.thing {
                let t = totals.entry(id).or_insert(0);
                *t = t.saturating_add(s.count);
            }
        }
        totals.into_iter().collect()
    }

    /// Puts up to `count` of `thing` in the bag: first onto stacks of it
    /// that have room (hotbar, then pack), then into empty hotbar slots,
    /// then empty pack slots. Returns how many did not fit.
    pub fn insert(&mut self, table: &ItemTable, thing: Thing, count: u32) -> u32 {
        let limit = table.stack_limit(thing);
        let mut rest = count;
        for slot in self.hotbar.iter_mut().chain(self.pack.iter_mut()) {
            if rest == 0 {
                break;
            }
            if let Some(s) = slot
                && s.thing == thing
                && s.count < limit
            {
                let n = rest.min(limit - s.count);
                s.count += n;
                rest -= n;
            }
        }
        for slot in self.hotbar.iter_mut().chain(self.pack.iter_mut()) {
            if rest == 0 {
                break;
            }
            if slot.is_none() {
                let n = rest.min(limit);
                *slot = Some(Stack { thing, count: n });
                rest -= n;
            }
        }
        rest
    }

    /// Takes up to `count` of `thing`: from the hotbar, then the pack, then
    /// what is worn, each in slot order. Returns how many it took.
    pub fn remove(&mut self, thing: Thing, count: u32) -> u32 {
        let mut taken = 0;
        for slot in self.hotbar.iter_mut().chain(self.pack.iter_mut()) {
            if taken == count {
                return taken;
            }
            if let Some(s) = slot
                && s.thing == thing
            {
                let n = (count - taken).min(s.count);
                s.count -= n;
                taken += n;
                if s.count == 0 {
                    *slot = None;
                }
            }
        }
        for e in EquipSlot::ALL {
            if taken == count {
                break;
            }
            if self.equip.get(&e).is_some_and(|s| s.thing == thing) {
                self.equip.remove(&e);
                taken += 1;
            }
        }
        taken
    }

    /// Moves `count` (`None`: all) of the stack in `from` to `to` and
    /// returns how many moved:
    /// - into an empty `to`: as many as `to` holds of it;
    /// - onto the same thing: as many as fit; the rest stays in `from`;
    /// - onto something else: the two whole stacks swap, if `count` is the
    ///   whole stack and each slot takes the other's stack.
    ///
    /// An equipment slot takes one item whose row names that slot. A
    /// refused move changes nothing.
    pub fn move_stack(
        &mut self,
        table: &ItemTable,
        from: SlotRef,
        to: SlotRef,
        count: Option<u32>,
    ) -> Result<u32, MoveError> {
        if !self.has_slot(from) || !self.has_slot(to) {
            return Err(MoveError::NoSuchSlot);
        }
        if from == to {
            return Err(MoveError::SameSlot);
        }
        let src = self.get(from).ok_or(MoveError::Empty)?;
        let n = count.unwrap_or(src.count);
        if n == 0 || n > src.count {
            return Err(MoveError::BadCount);
        }
        if !Self::takes(table, to, src.thing) {
            return Err(MoveError::WrongSlot);
        }
        let room_for = |slot: SlotRef, thing: Thing| match slot {
            SlotRef::Equip(_) => 1,
            _ => table.stack_limit(thing),
        };
        match self.get(to) {
            None => {
                let moved = n.min(room_for(to, src.thing));
                self.put(
                    to,
                    Some(Stack {
                        thing: src.thing,
                        count: moved,
                    }),
                );
                self.put(from, left(src, moved));
                Ok(moved)
            }
            Some(dst) if dst.thing == src.thing => {
                let room = room_for(to, src.thing).saturating_sub(dst.count);
                if room == 0 {
                    return Err(MoveError::Full);
                }
                let moved = n.min(room);
                self.put(
                    to,
                    Some(Stack {
                        thing: dst.thing,
                        count: dst.count + moved,
                    }),
                );
                self.put(from, left(src, moved));
                Ok(moved)
            }
            Some(dst) => {
                if n != src.count {
                    return Err(MoveError::Occupied);
                }
                if !Self::takes(table, from, dst.thing)
                    || src.count > room_for(to, src.thing)
                    || dst.count > room_for(from, dst.thing)
                {
                    return Err(MoveError::WrongSlot);
                }
                self.put(to, Some(src));
                self.put(from, Some(dst));
                Ok(n)
            }
        }
    }

    /// Whether `slot` takes `thing`: an equipment slot only the items worn
    /// there, other slots anything.
    pub fn takes(table: &ItemTable, slot: SlotRef, thing: Thing) -> bool {
        match slot {
            SlotRef::Equip(e) => table.slot_of(thing) == Some(e),
            SlotRef::Hotbar(_) | SlotRef::Pack(_) => true,
        }
    }

    /// Every stack: hotbar, pack, then worn, in slot order.
    fn stacks(&self) -> impl Iterator<Item = &Stack> {
        self.hotbar
            .iter()
            .chain(self.pack.iter())
            .flatten()
            .chain(self.equip.values())
    }

    /// Sets an existing slot.
    fn put(&mut self, slot: SlotRef, stack: Option<Stack>) {
        match slot {
            SlotRef::Hotbar(i) => self.hotbar[usize::from(i)] = stack,
            SlotRef::Pack(i) => self.pack[usize::from(i)] = stack,
            SlotRef::Equip(e) => match stack {
                Some(s) => {
                    self.equip.insert(e, s);
                }
                None => {
                    self.equip.remove(&e);
                }
            },
        }
    }
}

/// What is left of `stack` after `moved` leave it.
fn left(stack: Stack, moved: u32) -> Option<Stack> {
    (stack.count > moved).then_some(Stack {
        thing: stack.thing,
        count: stack.count - moved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEMS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/items.ron");

    /// A small table: 3 hotbar and 2 pack slots, materials stack to 10.
    fn table() -> ItemTable {
        ItemTable::from_ron_str(
            r#"(
                bag: (hotbar: 3, pack: 2),
                material_stack: 10,
                items: [
                    (key: "pick", name: "Pick", tags: ["tool"], stack: 1, tool: Some("pickaxe")),
                    (key: "cap", name: "Cap", tags: ["clothing"], stack: 4, slot: Some(Head)),
                    (key: "helm", name: "Helm", tags: ["armor"], stack: 1, slot: Some(Head)),
                    (key: "boots", name: "Boots", tags: ["clothing"], stack: 1, slot: Some(Feet)),
                ],
                start: [("pick", 1), ("cap", 2), ("boots", 1)],
            )"#,
        )
        .unwrap()
    }

    fn item(t: &ItemTable, key: &str) -> Thing {
        Thing::Item(t.id(key).unwrap())
    }

    const STONE: Thing = Thing::Material(MaterialId(4));
    const DIRT: Thing = Thing::Material(MaterialId(2));

    fn stack(thing: Thing, count: u32) -> Option<Stack> {
        Some(Stack { thing, count })
    }

    #[test]
    fn shipped_items_load() {
        let t = ItemTable::load(ITEMS_PATH).expect("assets/items.ron loads");
        assert_eq!(t.bag().hotbar, MAX_HOTBAR);
        assert!(t.bag().pack > 0);
        assert!(t.stack_limit(STONE) > 1);
        let tools: Vec<_> = t
            .iter()
            .filter(|(_, row)| row.tags.iter().any(|tag| tag == "tool"))
            .collect();
        assert!(!tools.is_empty(), "at least one tool");
        assert!(tools.iter().all(|(_, row)| row.tool.is_some()));
        for slot in EquipSlot::ALL {
            assert!(
                t.iter().any(|(_, row)| row.slot == Some(slot)),
                "something is worn on {slot:?}"
            );
        }
        // The kit starts with a tool in hand.
        let bag = Bag::starter(&t);
        let first = bag.hotbar()[0].expect("a tool in the first slot");
        let Thing::Item(id) = first.thing else {
            panic!("a material in the first slot")
        };
        assert!(t.get(id).unwrap().tool.is_some());
    }

    #[test]
    fn bad_items_are_refused() {
        let file = |bag: &str, mstack: u32, items: &str, start: &str| {
            format!("(bag: {bag}, material_stack: {mstack}, items: [{items}], start: [{start}])")
        };
        let ok_bag = "(hotbar: 3, pack: 2)";
        let row = |key: &str, stack: u32| format!("(key: \"{key}\", name: \"N\", stack: {stack})");
        let a = row("a", 1);
        let cases = [
            (file("(hotbar: 0, pack: 2)", 10, &a, ""), "hotbar 0"),
            (file("(hotbar: 10, pack: 2)", 10, &a, ""), "hotbar 10"),
            (file(ok_bag, 0, &a, ""), "material_stack 0"),
            (file(ok_bag, 10, &row("", 1), ""), "empty key"),
            (file(ok_bag, 10, &row("a", 0), ""), "stack 0"),
            (file(ok_bag, 10, &format!("{a}, {a}"), ""), "duplicate"),
            (file(ok_bag, 10, &a, "(\"b\", 1)"), "unknown start"),
            (file(ok_bag, 10, &a, "(\"a\", 0)"), "empty start"),
            (
                file(
                    ok_bag,
                    10,
                    "(key: \"a\", name: \"N\", stack: 1, tool: Some(\"\"))",
                    "",
                ),
                "empty tool",
            ),
            (
                file(ok_bag, 10, "(key: \"a\", name: \"N\", stack: 1, x: 1)", ""),
                "unknown field",
            ),
        ];
        for (text, why) in cases {
            assert!(ItemTable::from_ron_str(&text).is_err(), "{why} accepted");
        }
        assert!(ItemTable::from_ron_str(&file(ok_bag, 10, &a, "(\"a\", 2)")).is_ok());
    }

    #[test]
    fn insert_merges_then_fills_hotbar_then_pack() {
        let t = table();
        let mut bag = Bag::new(t.bag());
        assert_eq!(bag.insert(&t, STONE, 4), 0);
        assert_eq!(bag.insert(&t, DIRT, 3), 0);
        assert_eq!(bag.hotbar(), &[stack(STONE, 4), stack(DIRT, 3), None]);
        // Tops up the stone stack to 10, then takes the next empty slots.
        assert_eq!(bag.insert(&t, STONE, 17), 0);
        assert_eq!(
            bag.hotbar(),
            &[stack(STONE, 10), stack(DIRT, 3), stack(STONE, 10)]
        );
        assert_eq!(bag.pack(), &[stack(STONE, 1), None]);
        // A full bag returns what did not fit.
        assert_eq!(bag.insert(&t, DIRT, 30), 30 - 7 - 10);
        assert_eq!(bag.pack(), &[stack(STONE, 1), stack(DIRT, 10)]);
        assert_eq!(bag.count(STONE), 21);
        assert_eq!(bag.count(DIRT), 20);
        assert_eq!(
            bag.materials(),
            vec![(MaterialId(2), 20), (MaterialId(4), 21)]
        );
        // Items stack to their own limit.
        let pick = item(&t, "pick");
        let mut bag = Bag::new(t.bag());
        assert_eq!(bag.insert(&t, pick, 2), 0);
        assert_eq!(bag.hotbar(), &[stack(pick, 1), stack(pick, 1), None]);
        assert_eq!(bag.insert(&t, STONE, 0), 0);
        assert_eq!(bag.hotbar()[2], None, "inserting none takes no slot");
    }

    #[test]
    fn remove_takes_hotbar_first() {
        let t = table();
        let mut bag = Bag::new(t.bag());
        bag.insert(&t, STONE, 25);
        bag.insert(&t, DIRT, 1);
        assert_eq!(bag.remove(STONE, 12), 12);
        assert_eq!(bag.hotbar(), &[None, stack(STONE, 8), stack(STONE, 5)]);
        assert_eq!(bag.pack(), &[stack(DIRT, 1), None]);
        assert_eq!(bag.remove(STONE, 100), 13);
        assert_eq!(bag.count(STONE), 0);
        assert_eq!(bag.remove(STONE, 1), 0);
        assert_eq!(bag.remove(DIRT, 0), 0);
        assert_eq!(bag.materials(), vec![(MaterialId(2), 1)]);
        // What is worn goes last.
        let mut bag = Bag::starter(&t);
        let cap = item(&t, "cap");
        assert_eq!(bag.count(cap), 2);
        assert_eq!(bag.remove(cap, 2), 2);
        assert_eq!(bag.worn(EquipSlot::Head), None);
    }

    #[test]
    fn moves_merge_swap_and_refuse() {
        let t = table();
        let (h, p) = (SlotRef::Hotbar, SlotRef::Pack);
        let mut bag = Bag::new(t.bag());
        bag.insert(&t, STONE, 16);
        bag.insert(&t, DIRT, 3);
        assert_eq!(
            bag.hotbar(),
            &[stack(STONE, 10), stack(STONE, 6), stack(DIRT, 3)]
        );

        // Into an empty slot: some, then the rest.
        assert_eq!(bag.move_stack(&t, h(2), p(0), Some(1)), Ok(1));
        assert_eq!(bag.move_stack(&t, h(2), p(0), None), Ok(2));
        assert_eq!(bag.get(h(2)), None);
        assert_eq!(bag.get(p(0)), stack(DIRT, 3));
        // Onto the same thing: as many as fit, the rest stays.
        assert_eq!(bag.move_stack(&t, h(1), h(0), None), Err(MoveError::Full));
        assert_eq!(bag.move_stack(&t, h(0), h(1), None), Ok(4));
        assert_eq!(bag.get(h(0)), stack(STONE, 6));
        assert_eq!(bag.get(h(1)), stack(STONE, 10));
        // Onto something else: whole stacks swap; part of one is refused.
        assert_eq!(
            bag.move_stack(&t, p(0), h(0), Some(1)),
            Err(MoveError::Occupied)
        );
        assert_eq!(bag.move_stack(&t, p(0), h(0), None), Ok(3));
        assert_eq!(bag.get(h(0)), stack(DIRT, 3));
        assert_eq!(bag.get(p(0)), stack(STONE, 6));

        // Refusals change nothing.
        let before = bag.clone();
        for (from, to, count, err) in [
            (h(0), h(3), None, MoveError::NoSuchSlot),
            (p(9), h(0), None, MoveError::NoSuchSlot),
            (h(0), h(0), None, MoveError::SameSlot),
            (h(2), h(0), None, MoveError::Empty),
            (h(0), h(2), Some(0), MoveError::BadCount),
            (h(0), h(2), Some(4), MoveError::BadCount),
        ] {
            assert_eq!(
                bag.move_stack(&t, from, to, count),
                Err(err),
                "{from:?} -> {to:?}"
            );
        }
        assert_eq!(bag, before);
        assert_eq!(bag.count(STONE), 16);
        assert_eq!(bag.count(DIRT), 3);
    }

    #[test]
    fn equipment_slots_take_only_their_items() {
        let t = table();
        let (h, p, e) = (SlotRef::Hotbar, SlotRef::Pack, SlotRef::Equip);
        let (cap, helm, boots, pick) = (
            item(&t, "cap"),
            item(&t, "helm"),
            item(&t, "boots"),
            item(&t, "pick"),
        );
        let mut bag = Bag::new(t.bag());
        bag.insert(&t, STONE, 1);
        bag.insert(&t, cap, 3);
        bag.insert(&t, helm, 1);
        bag.insert(&t, boots, 1);
        assert_eq!(
            bag.hotbar(),
            &[stack(STONE, 1), stack(cap, 3), stack(helm, 1)]
        );
        assert_eq!(bag.pack(), &[stack(boots, 1), None]);

        // Materials, tools and the wrong wearable are refused.
        let before = bag.clone();
        assert_eq!(
            bag.move_stack(&t, h(0), e(EquipSlot::Head), None),
            Err(MoveError::WrongSlot)
        );
        assert_eq!(
            bag.move_stack(&t, p(0), e(EquipSlot::Head), None),
            Err(MoveError::WrongSlot)
        );
        assert_eq!(bag, before);
        let mut with_pick = bag.clone();
        with_pick.remove(STONE, 1);
        with_pick.insert(&t, pick, 1);
        assert_eq!(
            with_pick.move_stack(&t, h(0), e(EquipSlot::Hands), None),
            Err(MoveError::WrongSlot)
        );

        // A slot holds one: one cap of three goes on.
        assert_eq!(bag.move_stack(&t, h(1), e(EquipSlot::Head), None), Ok(1));
        assert_eq!(bag.worn(EquipSlot::Head), stack(cap, 1));
        assert_eq!(bag.get(h(1)), stack(cap, 2));
        assert_eq!(
            bag.move_stack(&t, h(1), e(EquipSlot::Head), None),
            Err(MoveError::Full)
        );
        // The two caps cannot swap with the helm's slot; the helm swaps in.
        assert_eq!(bag.move_stack(&t, h(2), e(EquipSlot::Head), None), Ok(1));
        assert_eq!(bag.worn(EquipSlot::Head), stack(helm, 1));
        assert_eq!(bag.get(h(2)), stack(cap, 1));
        assert_eq!(
            bag.move_stack(&t, h(1), e(EquipSlot::Head), None),
            Err(MoveError::WrongSlot)
        );
        // Taking something off: into an empty slot, or a swap only when the
        // equipment slot takes what comes back.
        assert_eq!(
            bag.move_stack(&t, e(EquipSlot::Head), h(0), None),
            Err(MoveError::WrongSlot)
        );
        assert_eq!(bag.move_stack(&t, e(EquipSlot::Head), p(1), None), Ok(1));
        assert_eq!(bag.worn(EquipSlot::Head), None);
        assert_eq!(bag.move_stack(&t, p(0), e(EquipSlot::Feet), None), Ok(1));
        assert_eq!(bag.worn(EquipSlot::Feet), stack(boots, 1));
        assert_eq!(bag.count(cap), 3);
        assert_eq!(bag.count(helm), 1);
    }

    #[test]
    fn starter_kit_wears_its_clothes() {
        let t = table();
        let (cap, boots, pick) = (item(&t, "cap"), item(&t, "boots"), item(&t, "pick"));
        let bag = Bag::starter(&t);
        assert_eq!(bag.worn(EquipSlot::Head), stack(cap, 1));
        assert_eq!(bag.worn(EquipSlot::Feet), stack(boots, 1));
        assert_eq!(bag.worn(EquipSlot::Body), None);
        // The pick and the spare cap go in the hotbar, in kit order.
        assert_eq!(bag.hotbar(), &[stack(pick, 1), stack(cap, 1), None]);
        assert_eq!(bag.count(cap), 2);
        assert_eq!(bag, Bag::starter(&t), "the same kit every time");
        // A bag round-trips through RON (the wire uses the same serde).
        let text = ron::to_string(&bag).unwrap();
        assert_eq!(ron::from_str::<Bag>(&text).unwrap(), bag);
    }
}
