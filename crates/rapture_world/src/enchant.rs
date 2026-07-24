//! Enchant/lapisia (0x0805 EnchantAdd): burns one lapisia + one enchant
//! material to raise an eligible weapon/armor's enchant level by 1.
//!
//! Wire format and mechanics per Aries's 0800.h (EP5 variant, our exact
//! client) and a real capture, cross-checked against the user's own
//! knowledge of the retail server:
//! - Eligibility comes from the target's `Items.ReqLuc`: the exact item id
//!   (`Type*1000+TypeID`) of the required enchant material, or 0 = not
//!   enchantable (only weapons/armor ever have this set -- jewelry/capes
//!   don't).
//! - The lapisia must match the target's category: `Items.Type == 95` with
//!   `TypeID` 1-5 for a weapon, 6-10 for armor.
//! - Both the lapisia and the material are consumed by exactly 1 (confirmed
//!   via capture: the response's own remaining-count fields for both items
//!   each dropped by 1, regardless of their starting stack).
//! - Enchant level lives in the same craftname string recreation uses (see
//!   `db::DbCraftStats`), last pair, capped at `ENCHANT_MAX_LEVEL` (20). An
//!   item already at 20 refuses the operation outright -- NOTHING is
//!   consumed (no lapisia, no material, no gold): this is a "the operation
//!   is simply not performed" rejection, same bucket as an invalid lapisia/
//!   material/insufficient funds, all handled by the `fail` closure below
//!   before anything is touched.
//! - Cost is a flat gold amount that depends on the target's category (NOT
//!   a DB column, per the user's explicit "valor simbolico" framing):
//!   `WEAPON_ENCHANT_COST_MONEY` (2,500,000) for weapons,
//!   `ARMOR_ENCHANT_COST_MONEY` (1,040,000) for armor.
//! - Success chance per current level (`ENCHANT_SUCCESS_CHANCE_PERCENT`,
//!   indexed by the level being enchanted FROM, i.e. 0 = the 0->1 attempt):
//!   ALL 20 entries are 100 for now, trivially tunable later. Unlike the
//!   "operation not performed" rejections above, a chance-roll failure DOES
//!   go through: gold and both items are already spent by the time the roll
//!   happens (the standard MMO enchant-risk convention), only the craftname
//!   stays unchanged. At 100% this branch is unreachable today, but the
//!   machinery is in place for whenever the table changes.
//! - Real combat effect: an enchanted weapon's flat attack bonus (both
//!   physical AND magic, per the user) and an enchanted armor's flat
//!   "absorption" (a universal damage reduction, applied directly in the
//!   final damage formula -- NOT a separate stat/packet like Imgeneus) are
//!   computed in `db::get_equipment_stats` from `WEAPON_ENCHANT_ATTACK_
//!   BONUS`/`ARMOR_ENCHANT_ABSORPTION` and consumed in
//!   `combat::apply_incoming_mob_damage`. The client owns 100% of the
//!   visual side (glow, tooltip) -- nothing here sends anything beyond the
//!   existing 0x0507 equipment refresh.

use anyhow::Result;
use rapture_common::db::{self, DbCharacter, DbCharacterItem, is_armor_item_type, is_weapon_item_type};
use rapture_common::packet::{Opcode, PacketWriter};

use crate::drops::chance_succeeds;
use crate::inventory::{find_item_index, is_valid_item_position};
use crate::session::Logger;

const ENCHANT_RESULT_FAILURE: u8 = 0;
const ENCHANT_RESULT_SUCCESS: u8 = 1;

/// `Items.Type` for a lapisia stone. `TypeID` 1-5 = weapon lapisia, 6-10 =
/// armor lapisia (confirmed by the user against the retail server).
const LAPISIA_ITEM_TYPE: u8 = 95;
const WEAPON_LAPISIA_TYPE_IDS: std::ops::RangeInclusive<u8> = 1..=5;
const ARMOR_LAPISIA_TYPE_IDS: std::ops::RangeInclusive<u8> = 6..=10;

/// Max enchant level (the craftname's last pair caps at this).
pub(crate) const ENCHANT_MAX_LEVEL: u8 = 20;

/// Flat gold cost for one weapon enchant operation.
const WEAPON_ENCHANT_COST_MONEY: u32 = 2_500_000;
/// Flat gold cost for one armor enchant operation.
const ARMOR_ENCHANT_COST_MONEY: u32 = 1_040_000;

/// Success chance (percent) for the enchant attempt FROM level N (index N)
/// TO level N+1, for N in 0..ENCHANT_MAX_LEVEL. All 100 for now -- trivial
/// to tune per-level later (see module doc).
const ENCHANT_SUCCESS_CHANCE_PERCENT: [u8; ENCHANT_MAX_LEVEL as usize] = [100; ENCHANT_MAX_LEVEL as usize];

#[allow(clippy::too_many_arguments)]
fn build_enchant_result(
    result: u8,
    lapisia_bag: u8,
    lapisia_slot: u8,
    lapisia_count: u8,
    material_bag: u8,
    material_slot: u8,
    material_count: u8,
    item_bag: u8,
    item_slot: u8,
    remaining_money: u32,
    craft_name: &str,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::EnchantAdd);
    packet.write_u8(result);
    packet.write_u8(lapisia_bag);
    packet.write_u8(lapisia_slot);
    packet.write_u8(lapisia_count);
    packet.write_u8(material_bag);
    packet.write_u8(material_slot);
    packet.write_u8(material_count);
    packet.write_u8(item_bag);
    packet.write_u8(item_slot);
    packet.write_u32(remaining_money);
    packet.write_fixed_string(craft_name, 21)?;
    packet.into_bytes()
}

/// Bumps a craftname's enchant pair (index 18-19) by 1, leaving every other
/// pair untouched. `previous` must already be a valid 20-digit craftname
/// (callers check `enchant_level < ENCHANT_MAX_LEVEL` before calling this).
fn bump_enchant_level(previous: &db::DbCraftStats) -> String {
    let pairs = [
        previous.strength,
        previous.dexterity,
        previous.reaction,
        previous.intelligence,
        previous.wisdom,
        previous.luck,
        previous.max_hp / 100,
        previous.max_mp / 100,
        previous.max_sp / 100,
        u32::from(previous.enchant_level) + 1,
    ];
    pairs.iter().map(|value| format!("{value:02}")).collect()
}

pub(crate) struct EnchantOutcome {
    pub(crate) response: Vec<u8>,
    /// True on success when the enchanted item is currently equipped -- the
    /// caller should refresh its 0x0507 equipment broadcast (new enchant
    /// step) to self + nearby viewers.
    pub(crate) item_equipped: bool,
    /// The character's gold balance after this call: unchanged from
    /// `character.money` when the operation wasn't performed at all (max
    /// level, invalid lapisia/material, insufficient funds), or the
    /// post-deduction balance whenever gold was actually spent (both a
    /// chance-roll failure and a real success spend it). The caller must
    /// sync this onto its own working `DbCharacter` (this function only
    /// takes `&DbCharacter`, so it can't update the caller's copy
    /// directly).
    pub(crate) new_money: u32,
}

/// Handles 0x0805 TPC_LAPISIAN_ADD. Returns None only for malformed
/// requests. Two distinct kinds of "didn't work" both answer with result 0
/// (Failure), but differ in whether anything was spent:
/// - Not performed at all (invalid lapisia/material, not enchantable, max
///   level already reached, insufficient gold): nothing consumed, echoes
///   back the unchanged positions/counts/money/craftname.
/// - Performed but the chance roll failed: gold + both items ARE consumed
///   (the standard MMO enchant-risk convention), craftname unchanged.
pub(crate) fn handle_enchant_add(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    active_items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<EnchantOutcome>> {
    if payload.len() < 9 {
        logger.line(format!(
            "Rapture World enchant add packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }
    let lapisia_bag = payload[2];
    let lapisia_slot = payload[3];
    let material_bag = payload[4];
    let material_slot = payload[5];
    // payload[6] is the client's requested materialCount -- ignored, always
    // consumes exactly 1 (see module doc).
    let item_bag = payload[7];
    let item_slot = payload[8];

    if !is_valid_item_position(lapisia_bag, lapisia_slot)
        || !is_valid_item_position(material_bag, material_slot)
        || !is_valid_item_position(item_bag, item_slot)
    {
        logger.line("Rapture World rejected enchant add with invalid positions");
        return Ok(None);
    }

    let Some(lapisia_index) = find_item_index(active_items, lapisia_bag, lapisia_slot) else {
        logger.line("Rapture World rejected enchant add: lapisia slot empty");
        return Ok(None);
    };
    let Some(material_index) = find_item_index(active_items, material_bag, material_slot) else {
        logger.line("Rapture World rejected enchant add: material slot empty");
        return Ok(None);
    };
    let Some(item_index) = find_item_index(active_items, item_bag, item_slot) else {
        logger.line("Rapture World rejected enchant add: item slot empty");
        return Ok(None);
    };
    let lapisia = active_items[lapisia_index].clone();
    let material = active_items[material_index].clone();
    let item = active_items[item_index].clone();

    let fail = |craft_name: &str| -> Result<Option<EnchantOutcome>> {
        Ok(Some(EnchantOutcome {
            response: build_enchant_result(
                ENCHANT_RESULT_FAILURE,
                lapisia_bag,
                lapisia_slot,
                lapisia.count,
                material_bag,
                material_slot,
                material.count,
                item_bag,
                item_slot,
                character.money,
                craft_name,
            )?,
            item_equipped: false,
            new_money: character.money,
        }))
    };

    if lapisia.count == 0 || material.count == 0 {
        logger.line("Rapture World rejected enchant add: lapisia/material count zero");
        return fail(&item.craft_name);
    }

    // Target category (weapon or armor) -- also implicitly excludes
    // jewelry/capes, which are never enchantable.
    let is_weapon = is_weapon_item_type(item.item_type);
    let is_armor = is_armor_item_type(item.item_type);
    if !is_weapon && !is_armor {
        logger.line(format!(
            "Rapture World rejected enchant add: item type={} type_id={} is neither weapon nor armor",
            item.item_type, item.type_id
        ));
        return fail(&item.craft_name);
    }

    // Lapisia must be Type 95, TypeID matching the target's category.
    let lapisia_type_id_valid = if is_weapon {
        WEAPON_LAPISIA_TYPE_IDS.contains(&lapisia.type_id)
    } else {
        ARMOR_LAPISIA_TYPE_IDS.contains(&lapisia.type_id)
    };
    if lapisia.item_type != LAPISIA_ITEM_TYPE || !lapisia_type_id_valid {
        logger.line(format!(
            "Rapture World rejected enchant add: lapisia type={} type_id={} invalid for {} target",
            lapisia.item_type,
            lapisia.type_id,
            if is_weapon { "weapon" } else { "armor" }
        ));
        return fail(&item.craft_name);
    }

    // Eligibility + required material, from the target's ReqLuc.
    let Some(req_luc) = db::get_item_req_luc(db_conn, item.item_type, item.type_id)? else {
        logger.line("Rapture World rejected enchant add: unknown item definition");
        return fail(&item.craft_name);
    };
    if req_luc == 0 {
        logger.line(format!(
            "Rapture World rejected enchant add: item type={} type_id={} is not enchantable (ReqLuc=0)",
            item.item_type, item.type_id
        ));
        return fail(&item.craft_name);
    }
    let material_id = u32::from(material.item_type) * 1000 + u32::from(material.type_id);
    if material_id != req_luc {
        logger.line(format!(
            "Rapture World rejected enchant add: material id={material_id} does not match required={req_luc}"
        ));
        return fail(&item.craft_name);
    }

    // Current enchant level, from the item's own craftname (0 if it has
    // none yet -- an un-recreated, never-enchanted item).
    let current_craft = db::parse_craft_name(&item.craft_name).unwrap_or_default();
    if current_craft.enchant_level >= ENCHANT_MAX_LEVEL {
        logger.line(format!(
            "Rapture World rejected enchant add: already at max enchant level {}",
            current_craft.enchant_level
        ));
        return fail(&item.craft_name);
    }

    let cost = if is_weapon {
        WEAPON_ENCHANT_COST_MONEY
    } else {
        ARMOR_ENCHANT_COST_MONEY
    };
    let Some(new_money) = db::deduct_character_money(db_conn, character.id, cost)? else {
        logger.line(format!(
            "Rapture World rejected enchant add: not enough gold (need {cost}, have {})",
            character.money
        ));
        return fail(&item.craft_name);
    };

    // Gold is spent from here on regardless of the roll below -- consume
    // both the lapisia and the material by 1 each now (confirmed via
    // capture -- see module doc), shared by both the success and
    // chance-roll-failure outcomes.
    let lapisia_remaining = lapisia.count - 1;
    db::consume_character_item(db_conn, character.id, lapisia.row_id, lapisia_remaining)?;
    let material_remaining = material.count - 1;
    db::consume_character_item(db_conn, character.id, material.row_id, material_remaining)?;
    if let Some(index) = find_item_index(active_items, lapisia_bag, lapisia_slot) {
        if lapisia_remaining == 0 {
            active_items.remove(index);
        } else if let Some(active) = active_items.get_mut(index) {
            active.count = lapisia_remaining;
        }
    }
    if let Some(index) = find_item_index(active_items, material_bag, material_slot) {
        if material_remaining == 0 {
            active_items.remove(index);
        } else if let Some(active) = active_items.get_mut(index) {
            active.count = material_remaining;
        }
    }

    let chance_percent = ENCHANT_SUCCESS_CHANCE_PERCENT[usize::from(current_craft.enchant_level)];
    if !chance_succeeds(u32::from(chance_percent)) {
        logger.line(format!(
            "Rapture World enchant add FAILED (chance roll) character_id={} item type={} type_id={} level={} chance={chance_percent}% lapisia_remaining={lapisia_remaining} material_remaining={material_remaining} money_remaining={new_money}",
            character.id, item.item_type, item.type_id, current_craft.enchant_level
        ));
        return Ok(Some(EnchantOutcome {
            response: build_enchant_result(
                ENCHANT_RESULT_FAILURE,
                lapisia_bag,
                lapisia_slot,
                lapisia_remaining,
                material_bag,
                material_slot,
                material_remaining,
                item_bag,
                item_slot,
                new_money,
                &item.craft_name,
            )?,
            item_equipped: false,
            new_money,
        }));
    }

    // Success: bump the enchant pair and persist.
    let new_craft_name = bump_enchant_level(&current_craft);
    db::set_character_item_craftname(db_conn, character.id, item.row_id, &new_craft_name)?;
    if let Some(active_item) = active_items.get_mut(item_index) {
        active_item.craft_name = new_craft_name.clone();
    }

    logger.line(format!(
        "Rapture World enchant add success character_id={} item type={} type_id={} bag={item_bag} slot={item_slot} new_level={} craftname={new_craft_name} lapisia_remaining={lapisia_remaining} material_remaining={material_remaining} money_remaining={new_money}",
        character.id, item.item_type, item.type_id, current_craft.enchant_level + 1
    ));

    Ok(Some(EnchantOutcome {
        response: build_enchant_result(
            ENCHANT_RESULT_SUCCESS,
            lapisia_bag,
            lapisia_slot,
            lapisia_remaining,
            material_bag,
            material_slot,
            material_remaining,
            item_bag,
            item_slot,
            new_money,
            &new_craft_name,
        )?,
        item_equipped: item_bag == 0,
        new_money,
    }))
}
