//! Item recreation (0x0806 ItemCompose): a simple recreation rune
//! (`Items.Special == 62`) rerolls an eligible item's orange (craftname)
//! bonus stats.
//!
//! Wire format per Aries's 0800.h (EP5 variant, our exact client); the
//! mechanics cross-checked against Imgeneus's `LinkingManager.Compose` with
//! two corrections confirmed by the user against the retail server:
//! - The number of bonuses rolled comes from the item's `Items.Server`
//!   column (Imgeneus hardcodes 3).
//! - Accessories (RealType 22/23/25) are excluded from HP/MP/SP rolls just
//!   like weapons (RealType 1-15); only armor rolls from the full 9-stat
//!   pool.
//!
//! Each rolled bonus is `random(1..=Items.ReqWis)` (`ReqWis` also gates
//! eligibility: 0 = not recreatable). The result is serialized into
//! `CharItems.Craftname` (see `db::DbCraftStats` for the 10-pair digit
//! encoding); the enchant pair (index 18) is preserved untouched for the
//! future enchant feature. The recreated stats count toward the character's
//! effective stats via `db::get_equipment_stats`, which parses every
//! equipped item's craftname.

use anyhow::Result;
use rapture_common::db::{self, DbCharacterItem, is_weapon_item_type};
use rapture_common::packet::{Opcode, PacketWriter};

use crate::character::SPECIAL_RECREATION_RUNE;
use crate::drops::next_random_u64;
use crate::inventory::{build_use_item_result, find_item_index, is_valid_item_position};
use crate::session::Logger;

const COMPOSE_RESULT_SUCCESS: u8 = 0;
const COMPOSE_RESULT_FAILURE: u8 = 1;

/// `Items.Type` values for jewelry (Ring/Necklace/Bracelet), the other
/// category (besides weapons) excluded from HP/MP/SP recreation rolls.
/// Confirmed by the user against the live retail server: Type 22, 23, 40 --
/// see `character::is_weapon_item_type`'s doc for how this whole
/// classification is sourced (RealType is not a DB column).
const JEWELRY_ITEM_TYPES: [u8; 3] = [22, 23, 40];

/// Whether `item_type` only ever rolls the six stats (no HP/MP/SP) on
/// recreation: weapons and jewelry. Everything else eligible for recreation
/// (armor, capes) rolls the full 9-stat pool including HP/MP/SP.
fn rolls_stats_only(item_type: u8) -> bool {
    is_weapon_item_type(item_type) || JEWELRY_ITEM_TYPES.contains(&item_type)
}

fn build_item_compose_result(
    result: u8,
    bag: u8,
    slot: u8,
    craft_name: &str,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ItemCompose);
    packet.write_u8(result);
    packet.write_u8(bag);
    packet.write_u8(slot);
    packet.write_fixed_string(craft_name, 21)?;
    packet.into_bytes()
}

/// Generates a fresh 20-digit craftname: `count` distinct bonuses from the
/// item's pool, each rolled 1..=`max_value`, preserving the enchant pair
/// from `previous` (a recreation must never wipe a future enchant level).
fn roll_craft_name(count: u8, max_value: u8, stats_only: bool, previous: &str) -> String {
    // Pair indices: 0=str 1=dex 2=rec 3=int 4=wis 5=luc 6=hp 7=mp 8=sp.
    let pool_size: u64 = if stats_only { 6 } else { 9 };
    let mut chosen: Vec<usize> = Vec::new();
    let count = usize::from(count).min(pool_size as usize);
    while chosen.len() < count {
        let index = (next_random_u64() % pool_size) as usize;
        if !chosen.contains(&index) {
            chosen.push(index);
        }
    }

    let mut pairs = [0u32; 10];
    for index in chosen {
        pairs[index] = (next_random_u64() % u64::from(max_value.max(1))) as u32 + 1;
    }
    // Preserve the enchant pair (index 18..20 of the old string) verbatim.
    pairs[9] = db::parse_craft_name(previous)
        .map(|craft| u32::from(craft.enchant_level))
        .unwrap_or(0);

    pairs.iter().map(|value| format!("{value:02}")).collect()
}

pub(crate) struct ComposeOutcome {
    /// The 0x0806 result packet.
    pub(crate) response: Vec<u8>,
    /// The 0x050A rune-consume packet, sent AFTER the result (confirmed via
    /// a real capture: 0x050B max-energy first, then 0x0806, then 0x050A --
    /// omitting this crashed the client). None on failure responses.
    pub(crate) rune_consume: Option<Vec<u8>>,
    /// True when the recreated item is currently equipped -- the caller must
    /// send a 0x050B max-energy update for each pool whose max changed
    /// (matching the real server; NOT the full stat bundle).
    pub(crate) item_equipped: bool,
}

/// Handles 0x0806 TPC_ITEM_COMPOSE. Returns None only for malformed
/// requests; identifiable-but-invalid ones (wrong rune, non-recreatable
/// item) answer with result 1 so the client's UI unlocks.
pub(crate) fn handle_item_compose(
    db_conn: &mut db::Db,
    character_id: u32,
    active_items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<ComposeOutcome>> {
    if payload.len() < 6 {
        logger.line(format!(
            "Rapture World item compose packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }
    let (rune_bag, rune_slot, item_bag, item_slot) =
        (payload[2], payload[3], payload[4], payload[5]);

    // Both an equipped target (bag 0) and a dragged inventory target
    // (bags 1-5) are valid -- is_valid_item_position accepts the whole
    // 0..=5 range. The rune itself always sits in a regular inventory bag,
    // but nothing requires rejecting other positions server-side.
    if !is_valid_item_position(rune_bag, rune_slot)
        || !is_valid_item_position(item_bag, item_slot)
    {
        logger.line("Rapture World rejected item compose with invalid positions");
        return Ok(None);
    }

    let fail = |craft_name: &str| -> Result<Option<ComposeOutcome>> {
        Ok(Some(ComposeOutcome {
            response: build_item_compose_result(COMPOSE_RESULT_FAILURE, item_bag, item_slot, craft_name)?,
            rune_consume: None,
            item_equipped: false,
        }))
    };

    let Some(rune_index) = find_item_index(active_items, rune_bag, rune_slot) else {
        logger.line("Rapture World rejected item compose: rune slot empty");
        return fail("");
    };
    let Some(item_index) = find_item_index(active_items, item_bag, item_slot) else {
        logger.line("Rapture World rejected item compose: item slot empty");
        return fail("");
    };
    let rune = active_items[rune_index].clone();
    let item = active_items[item_index].clone();
    if rune.count == 0 {
        logger.line("Rapture World rejected item compose: rune count zero");
        return fail(&item.craft_name);
    }

    // The rune must actually be a (simple) recreation rune.
    let Some(rune_effect) = db::get_consumable_effect(db_conn, rune.item_type, rune.type_id)?
    else {
        logger.line("Rapture World rejected item compose: unknown rune definition");
        return fail(&item.craft_name);
    };
    if rune_effect.special != SPECIAL_RECREATION_RUNE {
        logger.line(format!(
            "Rapture World rejected item compose: special={} is not a recreation rune",
            rune_effect.special
        ));
        return fail(&item.craft_name);
    }

    // The target must be recreatable: ReqWis > 0 (roll ceiling) and
    // Server > 0 (number of bonuses).
    let Some(info) = db::get_item_compose_info(db_conn, item.item_type, item.type_id)? else {
        logger.line("Rapture World rejected item compose: unknown item definition");
        return fail(&item.craft_name);
    };
    if info.req_wis == 0 || info.recreation_count == 0 {
        logger.line(format!(
            "Rapture World rejected item compose: item type={} type_id={} not recreatable (req_wis={} count={})",
            item.item_type, item.type_id, info.req_wis, info.recreation_count
        ));
        return fail(&item.craft_name);
    }

    let craft_name = roll_craft_name(
        info.recreation_count,
        info.req_wis,
        rolls_stats_only(item.item_type),
        &item.craft_name,
    );
    db::set_character_item_craftname(db_conn, character_id, item.row_id, &craft_name)?;
    if let Some(active_item) = active_items.get_mut(item_index) {
        active_item.craft_name = craft_name.clone();
    }

    // Consume one rune.
    let remaining_count = rune.count.saturating_sub(1);
    db::consume_character_item(db_conn, character_id, rune.row_id, remaining_count)?;
    // Re-located by position (not the cloned index) for clarity; the
    // craftname edit above doesn't reorder the vec.
    if let Some(index) = find_item_index(active_items, rune_bag, rune_slot) {
        if remaining_count == 0 {
            active_items.remove(index);
        } else if let Some(active_rune) = active_items.get_mut(index) {
            active_rune.count = remaining_count;
        }
    }

    logger.line(format!(
        "Rapture World item compose success item type={} type_id={} bag={item_bag} slot={item_slot} stats_only={} rolls={} max={} craftname={craft_name} rune_remaining={remaining_count}",
        item.item_type, item.type_id, rolls_stats_only(item.item_type), info.recreation_count, info.req_wis
    ));

    // The 0x050A consume mirrors a normal item use: the rune at its own
    // position with the decremented count.
    let mut consumed_rune = rune;
    consumed_rune.count = remaining_count;

    Ok(Some(ComposeOutcome {
        response: build_item_compose_result(COMPOSE_RESULT_SUCCESS, item_bag, item_slot, &craft_name)?,
        rune_consume: Some(build_use_item_result(character_id, Some(&consumed_rune))?),
        item_equipped: item_bag == 0,
    }))
}
