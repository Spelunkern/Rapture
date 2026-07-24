use anyhow::Result;
use rapture_common::db::{self, DbCharacterItem, DbItemDefinition, DbMobDrop};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::inventory::{MapItem, build_map_add_item};
use crate::map_items;
use crate::session::Logger;
use crate::world_entities::lock_or_recover;

#[derive(Clone, Debug, Default)]
struct DropTables {
    mob_drops: HashMap<u16, Vec<DbMobDrop>>,
    grade_items: HashMap<u16, Vec<DbItemDefinition>>,
}

fn tables() -> &'static Mutex<DropTables> {
    static TABLES: OnceLock<Mutex<DropTables>> = OnceLock::new();
    TABLES.get_or_init(|| Mutex::new(DropTables::default()))
}

pub(crate) fn next_random_u64() -> u64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut state = STATE.load(Ordering::Relaxed);
    if state == 0 {
        state = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
    }
    state ^= state << 7;
    state ^= state >> 9;
    state = state.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    STATE.store(state, Ordering::Relaxed);
    state
}

pub(crate) fn chance_succeeds(percent: u32) -> bool {
    if percent == 0 {
        return false;
    }
    if percent >= 100 {
        return true;
    }
    (next_random_u64() % 100) < u64::from(percent)
}

fn choose_grade_item(items: &[DbItemDefinition]) -> Option<DbItemDefinition> {
    if items.is_empty() {
        return None;
    }
    let index = (next_random_u64() as usize) % items.len();
    Some(items[index])
}

pub(crate) fn load_drop_tables(logger: &Logger) -> Result<()> {
    let mut db_conn = db::connect()?;
    let mob_drops = db::get_mob_drops(&mut db_conn)?;
    let grade_items = db::get_item_definitions_by_grade(&mut db_conn)?;
    let mob_count = mob_drops.len();
    let drop_rows = mob_drops.values().map(Vec::len).sum::<usize>();
    let grade_count = grade_items.len();
    let grade_item_count = grade_items.values().map(Vec::len).sum::<usize>();

    *lock_or_recover(tables()) = DropTables {
        mob_drops,
        grade_items,
    };

    logger.line(format!(
        "Rapture World loaded drop tables mobs={} drop_rows={} grades={} grade_items={}",
        mob_count, drop_rows, grade_count, grade_item_count
    ));
    Ok(())
}

fn map_item_from_definition(
    definition: DbItemDefinition,
    owner_id: u32,
    pos_x: f32,
    pos_y: f32,
    pos_z: f32,
) -> MapItem {
    MapItem {
        id: map_items::next_item_id(),
        item: DbCharacterItem {
            row_id: 0,
            bag: 0,
            slot: 0,
            item_type: definition.item_type,
            type_id: definition.type_id,
            quality: definition.quality,
            gems: [0; 6],
            count: 1,
            craft_name: String::new(),
            dyed: false,
        },
        money: 0,
        owner_id,
        dropped_at: Instant::now(),
        pos_x,
        pos_y,
        pos_z,
    }
}

fn map_money_from_amount(
    amount: u32,
    owner_id: u32,
    pos_x: f32,
    pos_y: f32,
    pos_z: f32,
) -> MapItem {
    MapItem {
        id: map_items::next_item_id(),
        item: DbCharacterItem {
            row_id: 0,
            bag: 0,
            slot: 0,
            item_type: 26,
            type_id: 0,
            quality: 0,
            gems: [0; 6],
            count: 1,
            craft_name: String::new(),
            dyed: false,
        },
        money: amount,
        owner_id,
        dropped_at: Instant::now(),
        pos_x,
        pos_y,
        pos_z,
    }
}

/// money_base = Money1 (guaranteed floor), money_extra = Money2 (random extra
/// added on top, 0..=money_extra). Drop chance is a fixed 25%, independent of
/// mob-specific drop_rate values used for item drops.
fn roll_money_amount(money_base: u32, money_extra: u32) -> Option<u32> {
    if money_base == 0 && money_extra == 0 {
        return None;
    }
    if !chance_succeeds(25) {
        return None;
    }

    let extra = if money_extra == 0 {
        0
    } else {
        (next_random_u64() % u64::from(money_extra + 1)) as u32
    };
    Some(money_base.saturating_add(extra))
}

pub(crate) fn roll_mob_drops(
    map_id: u16,
    mob_id: u16,
    money_min: u32,
    money_max: u32,
    owner_id: u32,
    pos_x: f32,
    pos_y: f32,
    pos_z: f32,
    logger: &Logger,
) -> Result<Vec<Vec<u8>>> {
    let tables = lock_or_recover(tables()).clone();

    let mut packets = Vec::new();
    for drop in tables.mob_drops.get(&mob_id).into_iter().flatten() {
        if !chance_succeeds(drop.drop_rate.min(100)) {
            continue;
        }
        let Some(items) = tables.grade_items.get(&drop.grade) else {
            logger.line(format!(
                "Rapture World mob drop skipped mob_id={} order={} grade={} because grade has no items",
                drop.mob_id, drop.item_order, drop.grade
            ));
            continue;
        };
        let Some(definition) = choose_grade_item(items) else {
            continue;
        };
        let map_item = map_item_from_definition(definition, owner_id, pos_x, pos_y, pos_z);
        let packet = build_map_add_item(&map_item)?;
        logger.line(format!(
            "Rapture World mob drop mob_id={} order={} grade={} rate={} item_type={} type_id={} quality={} map_item_id={}",
            drop.mob_id,
            drop.item_order,
            drop.grade,
            drop.drop_rate,
            definition.item_type,
            definition.type_id,
            definition.quality,
            map_item.id
        ));
        map_items::add_item(map_id, map_item);
        packets.push(packet);
    }

    if let Some(amount) = roll_money_amount(money_min, money_max) {
        let map_item = map_money_from_amount(amount, owner_id, pos_x, pos_y, pos_z);
        let packet = build_map_add_item(&map_item)?;
        logger.line(format!(
            "Rapture World mob money drop mob_id={} amount={} map_item_id={}",
            mob_id, amount, map_item.id
        ));
        map_items::add_item(map_id, map_item);
        packets.push(packet);
    }

    Ok(packets)
}
