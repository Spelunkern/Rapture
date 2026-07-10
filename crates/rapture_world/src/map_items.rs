use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::inventory::MapItem;
use crate::world_entities::lock_or_recover;

/// Total lifetime of a dropped item, from drop to permanent despawn. Per
/// the desired behavior: 0-30s pickable only by the dropper
/// (inventory::MAP_ITEM_OWNER_WINDOW), 30-60s pickable by anyone, at 60s
/// it despawns and is gone for good.
pub(crate) const MAP_ITEM_DESPAWN_AGE: Duration = Duration::from_secs(60);

/// Dropped items are shared process-wide (keyed by map id) instead of
/// living in a single connection's local state, so every player on a map
/// can see and pick up items dropped by anyone else -- previously each
/// connection had its own private Vec<MapItem>, so only the dropper's own
/// client ever knew an item existed.
fn registry() -> &'static Mutex<HashMap<u16, Vec<MapItem>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u16, Vec<MapItem>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_ITEM_ID: AtomicU32 = AtomicU32::new(1);

pub(crate) fn next_item_id() -> u32 {
    NEXT_ITEM_ID.fetch_add(1, Ordering::SeqCst).max(1)
}

pub(crate) fn add_item(map_id: u16, item: MapItem) {
    lock_or_recover(registry())
        .entry(map_id)
        .or_default()
        .push(item);
}

pub(crate) fn find_item(map_id: u16, item_id: u32) -> Option<MapItem> {
    lock_or_recover(registry())
        .get(&map_id)?
        .iter()
        .find(|item| item.id == item_id)
        .cloned()
}

pub(crate) fn remove_item(map_id: u16, item_id: u32) -> Option<MapItem> {
    let mut registry = lock_or_recover(registry());
    let items = registry.get_mut(&map_id)?;
    let index = items.iter().position(|item| item.id == item_id)?;
    Some(items.remove(index))
}

pub(crate) fn set_item_count(map_id: u16, item_id: u32, count: u8) -> Option<MapItem> {
    let mut registry = lock_or_recover(registry());
    let item = registry
        .get_mut(&map_id)?
        .iter_mut()
        .find(|item| item.id == item_id)?;
    item.item.count = count;
    Some(item.clone())
}

pub(crate) fn items_on_map(map_id: u16) -> Vec<MapItem> {
    lock_or_recover(registry())
        .get(&map_id)
        .cloned()
        .unwrap_or_default()
}

/// Removes and returns every item that has reached MAP_ITEM_DESPAWN_AGE,
/// across all maps, for the background despawn sweep (see main.rs) to
/// broadcast a removal to whoever is still looking at that map.
pub(crate) fn sweep_expired() -> Vec<(u16, MapItem)> {
    let mut registry = lock_or_recover(registry());
    let mut expired = Vec::new();
    for (&map_id, items) in registry.iter_mut() {
        let mut index = 0;
        while index < items.len() {
            if items[index].dropped_at.elapsed() >= MAP_ITEM_DESPAWN_AGE {
                expired.push((map_id, items.remove(index)));
            } else {
                index += 1;
            }
        }
    }
    expired
}
