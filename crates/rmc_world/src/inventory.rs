//! Per-player material counts (SAD §6: "`Inventory` counts materials").
//!
//! Yields are [`MaterialId`]s and there is no separate item type in Phase 1,
//! so an [`Inventory`] is a count per material. Counts saturate at
//! `u32::MAX`, and materials with a count of zero are not stored, so two
//! inventories holding the same counts compare equal.

use std::collections::BTreeMap;

use crate::materials::MaterialId;

/// Material counts, ordered by material id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    counts: BTreeMap<MaterialId, u32>,
}

impl Inventory {
    /// An empty inventory.
    pub fn new() -> Self {
        Self::default()
    }

    /// An inventory holding `items`. Repeated materials are summed.
    pub fn from_items(items: &[(MaterialId, u32)]) -> Self {
        let mut inv = Self::new();
        inv.add_all(items);
        inv
    }

    /// Adds `count` of `id`, saturating at `u32::MAX`.
    pub fn add(&mut self, id: MaterialId, count: u32) {
        if count == 0 {
            return;
        }
        let c = self.counts.entry(id).or_insert(0);
        *c = c.saturating_add(count);
    }

    /// Adds every `(material, count)` pair, in order.
    pub fn add_all(&mut self, items: &[(MaterialId, u32)]) {
        for &(id, count) in items {
            self.add(id, count);
        }
    }

    /// How many of `id` the inventory holds.
    pub fn count(&self, id: MaterialId) -> u32 {
        self.counts.get(&id).copied().unwrap_or(0)
    }

    /// Every held material with a nonzero count, ascending by id. This is the
    /// list `ServerMsg::Inventory` carries.
    pub fn items(&self) -> Vec<(MaterialId, u32)> {
        self.counts.iter().map(|(&id, &c)| (id, c)).collect()
    }

    /// Whether the inventory holds nothing.
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_sum_saturate_and_sort() {
        let a = MaterialId(4);
        let b = MaterialId(2);
        let mut inv = Inventory::new();
        assert!(inv.is_empty());
        assert_eq!(inv.count(a), 0);
        inv.add(a, 3);
        inv.add(b, 0);
        inv.add_all(&[(b, 1), (a, 2)]);
        assert_eq!(inv.count(a), 5);
        assert_eq!(inv.count(b), 1);
        assert_eq!(inv.items(), vec![(b, 1), (a, 5)]);
        inv.add(a, u32::MAX);
        assert_eq!(inv.count(a), u32::MAX);
        assert_eq!(
            Inventory::from_items(&[(a, 1), (b, 0), (a, 1)]).items(),
            vec![(a, 2)]
        );
        assert_eq!(Inventory::from_items(&[(b, 0)]), Inventory::new());
    }
}
