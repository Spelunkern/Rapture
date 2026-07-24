//! Item Mall / cash shop purchases (`0x2602 PointBuy`).
//!
//! Wire format confirmed byte-for-byte against Shaiya-Aries's own headers
//! (`network/game/outgoing/2600.h`, `PointLogUnit.h`, `PointItemUnit.h`) and
//! a real capture of both a free (cost 0) and a priced (cost 1000) single-
//! item purchase:
//!
//! Incoming `TPC_POINT_BUY` (0x2602): `productCode: String<21>` (plus one
//! trailing reserved byte in the capture, always 0).
//!
//! Outgoing `TP_POINT_BUY` (0x2602):
//! ```text
//! result: u8            (0=Success, 1=ProductCodeDoesNotExist,
//!                         2=NotEnoughPoints, 3=NotEnoughSlots)
//! remainingPoints: u32
//! log: {
//!     productCode: String<21>
//!     purchaseDate: u32 (time32_t)
//!     purchasePoints: u32   -- the cost, confirmed 0 for a free item and
//!                              1000 for a 1000-point item in captures
//! }
//! itemCount: u8
//! items[itemCount]: { bag: u8, slot: u8, type: u8, typeId: u8, count: u8 }
//! ```
//! (`PointItemUnit_EP5` -- the no-expiry variant; Aries 5.4 doesn't appear
//! to need `PointItemUnit_EP6_4`'s trailing date range).
//!
//! Item placement doesn't attempt to merge into existing stacks (unlike
//! `getitem`'s `handle_getitem_request`) -- each granted item entry always
//! claims a fresh free inventory slot. This is a deliberate simplification,
//! not a protocol requirement.

use anyhow::Result;
use rapture_common::db::{self, DbCharacter, DbCharacterItem};
use rapture_common::packet::{Opcode, PacketWriter, read_fixed_ascii};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::inventory::{effective_stack_limit, find_free_inventory_slots};
use crate::session::Logger;

const RESULT_SUCCESS: u8 = 0;
const RESULT_PRODUCT_NOT_FOUND: u8 = 1;
const RESULT_NOT_ENOUGH_POINTS: u8 = 2;
const RESULT_NOT_ENOUGH_SLOTS: u8 = 3;

// GamePointGiftSendResult (Aries 2600.h) -- gift-send (0x2603) result codes.
// Distinct from the PointBuy codes above (different enum, different values).
pub(crate) const GIFT_RESULT_SUCCESS: u8 = 0;
pub(crate) const GIFT_RESULT_CHAR_NAME_DOES_NOT_EXIST: u8 = 1;
pub(crate) const GIFT_RESULT_SAME_ACCOUNT: u8 = 2;
pub(crate) const GIFT_RESULT_FULL: u8 = 3;
pub(crate) const GIFT_RESULT_NOT_ENOUGH_SLOTS: u8 = 4;
pub(crate) const GIFT_RESULT_PRODUCT_NOT_FOUND: u8 = 6;
pub(crate) const GIFT_RESULT_NOT_ENOUGH_POINTS: u8 = 10;

/// Gift-box capacity (PS_Billing.dbo.UsersProduct3 slots 1..=240, per the
/// Aries list packet's own 240-entry array).
pub(crate) const GIFT_BOX_CAPACITY: u8 = 240;

fn current_time32() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as u32)
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn build_point_buy_result(
    result: u8,
    remaining_points: u32,
    product_code: &str,
    purchase_points: u32,
    items: &[(u8, u8, u8, u8, u8)],
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::PointBuy);
    packet.write_u8(result);
    packet.write_u32(remaining_points);
    packet.write_fixed_string(product_code, 21)?;
    packet.write_u32(current_time32());
    packet.write_u32(purchase_points);
    packet.write_u8(items.len() as u8);
    for &(bag, slot, item_type, type_id, count) in items {
        packet.write_u8(bag);
        packet.write_u8(slot);
        packet.write_u8(item_type);
        packet.write_u8(type_id);
        packet.write_u8(count);
    }
    packet.into_bytes()
}

/// Builds the 0x2603 TP_POINT_GIFT_SEND response (see
/// `Opcode::PointGiftSend` for the field layout and result codes).
pub(crate) fn build_gift_send_result(
    result: u8,
    target_name: &str,
    remaining_points: u32,
    product_code: &str,
    purchase_points: u32,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::PointGiftSend);
    packet.write_u8(result);
    packet.write_fixed_string(target_name, 21)?;
    packet.write_u32(remaining_points);
    packet.write_fixed_string(product_code, 21)?;
    packet.write_u32(current_time32());
    packet.write_u32(purchase_points);
    packet.into_bytes()
}

/// Builds the 0x2606 TP_POINT_STORED_ITEMLIST gift-box push: each entry is
/// `(npcSlot, type, typeId, count)`, `npcSlot` being the 0-based wire slot
/// (DB `Slot` - 1).
pub(crate) fn build_stored_item_list(items: &[(u8, u8, u8, u8)]) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::PointStoredItemList);
    packet.write_u8(items.len().min(usize::from(GIFT_BOX_CAPACITY)) as u8);
    for &(npc_slot, item_type, type_id, count) in items.iter().take(usize::from(GIFT_BOX_CAPACITY)) {
        packet.write_u8(npc_slot);
        packet.write_u8(item_type);
        packet.write_u8(type_id);
        packet.write_u8(count);
    }
    packet.into_bytes()
}

/// Builds the 0x2607 TP_POINT_STORED_ITEMGET claim response: which gift slot
/// was claimed and the inventory (bag, slot) the item landed in.
pub(crate) fn build_stored_item_get(npc_slot: u8, bag: u8, slot: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::PointStoredItemGet);
    packet.write_u8(npc_slot);
    packet.write_u8(bag);
    packet.write_u8(slot);
    packet.into_bytes()
}

pub(crate) struct PointBuyOutcome {
    pub(crate) response: Vec<u8>,
    pub(crate) add_item_packets: Vec<Vec<u8>>,
    pub(crate) new_points: u32,
}

pub(crate) struct GiftSendOutcome {
    pub(crate) response: Vec<u8>,
    pub(crate) new_points: u32,
    /// Set only on success, so the caller can live-push a refreshed 0x2606
    /// list to the recipient if they happen to be online.
    pub(crate) recipient: Option<(String, u32)>,
}

/// Handles 0x2603 `TPC_POINT_GIFT_SEND`: buys `productCode` on behalf of the
/// sender and stores its item bundle in the *target character's account*
/// gift box (PS_Billing.dbo.Users_Product3), one row/slot per bundle item.
/// The recipient need not be online. Free (cost-0) products can NOT be
/// gifted -- by project design the operation fails silently (returns None,
/// no response at all).
pub(crate) fn handle_gift_send(
    db_conn: &mut db::Db,
    sender_account_id: u32,
    current_points: u32,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<GiftSendOutcome>> {
    if payload.len() < 44 {
        logger.line(format!(
            "Rapture World gift send packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }
    let product_code = read_fixed_ascii(&payload[2..23]);
    let target_name = read_fixed_ascii(&payload[23..44]);

    let fail = |result: u8, cost: u32| -> Result<Option<GiftSendOutcome>> {
        Ok(Some(GiftSendOutcome {
            response: build_gift_send_result(result, &target_name, current_points, &product_code, cost)?,
            new_points: current_points,
            recipient: None,
        }))
    };

    let Some(product) = db::get_product_by_code(db_conn, &product_code)? else {
        logger.line(format!(
            "Rapture World rejected gift send: unknown product_code='{product_code}'"
        ));
        return fail(GIFT_RESULT_PRODUCT_NOT_FOUND, 0);
    };
    // Free products can't be gifted -- silent failure by design (no
    // response; gifting something universally free makes no sense).
    if product.buy_cost == 0 {
        logger.line(format!(
            "Rapture World silently rejected gift send of free product_code='{product_code}'"
        ));
        return Ok(None);
    }
    let Some(target) = db::get_character_by_name(db_conn, &target_name)? else {
        logger.line(format!(
            "Rapture World rejected gift send: unknown target character '{target_name}'"
        ));
        return fail(GIFT_RESULT_CHAR_NAME_DOES_NOT_EXIST, product.buy_cost);
    };
    if target.account_id == sender_account_id {
        logger.line(format!(
            "Rapture World rejected gift send to own account character '{target_name}'"
        ));
        return fail(GIFT_RESULT_SAME_ACCOUNT, product.buy_cost);
    }
    if current_points < product.buy_cost {
        logger.line(format!(
            "Rapture World rejected gift send: not enough points cost={} balance={current_points}",
            product.buy_cost
        ));
        return fail(GIFT_RESULT_NOT_ENOUGH_POINTS, product.buy_cost);
    }

    // Free slots in the target's box (DB slots 1..=240, ascending).
    let used: std::collections::HashSet<u8> = db::get_gift_box(db_conn, target.account_id)?
        .iter()
        .map(|entry| entry.slot)
        .collect();
    let free_slots: Vec<u8> = (1..=GIFT_BOX_CAPACITY)
        .filter(|slot| !used.contains(slot))
        .collect();
    if free_slots.is_empty() {
        logger.line(format!(
            "Rapture World rejected gift send: gift box full for '{target_name}'"
        ));
        return fail(GIFT_RESULT_FULL, product.buy_cost);
    }
    if free_slots.len() < product.items.len() {
        logger.line(format!(
            "Rapture World rejected gift send: not enough gift slots for '{target_name}' (need {}, free {})",
            product.items.len(),
            free_slots.len()
        ));
        return fail(GIFT_RESULT_NOT_ENOUGH_SLOTS, product.buy_cost);
    }

    let new_points = current_points - product.buy_cost;
    db::set_account_points(db_conn, sender_account_id, new_points)?;
    for (index, &(item_type, type_id, count)) in product.items.iter().enumerate() {
        db::insert_gift(
            db_conn,
            target.account_id,
            free_slots[index],
            item_type,
            type_id,
            count,
            &product.product_code,
        )?;
    }

    logger.line(format!(
        "Rapture World gift send success product_code='{product_code}' target='{target_name}' (account {}) cost={} items={} remaining_points={new_points}",
        target.account_id,
        product.buy_cost,
        product.items.len()
    ));
    Ok(Some(GiftSendOutcome {
        response: build_gift_send_result(
            GIFT_RESULT_SUCCESS,
            &target_name,
            new_points,
            &product_code,
            product.buy_cost,
        )?,
        new_points,
        recipient: Some((target.name.clone(), target.account_id)),
    }))
}

/// Builds the 0x2606 gift-box list for `account_id` straight from the DB
/// (wire `npcSlot` = DB slot - 1).
pub(crate) fn build_gift_box_list(db_conn: &mut db::Db, account_id: u32) -> Result<Vec<u8>> {
    let entries: Vec<(u8, u8, u8, u8)> = db::get_gift_box(db_conn, account_id)?
        .iter()
        .map(|entry| (entry.slot.saturating_sub(1), entry.item_type, entry.type_id, entry.count))
        .collect();
    build_stored_item_list(&entries)
}

/// Handles 0x2607 `TPC_POINT_STORED_ITEMGET`: claims the gift at `npcSlot`
/// (DB slot = npcSlot + 1) out of the account's box into the character's
/// inventory. Fails silently (no response) if the slot is empty, the
/// inventory has no free slot, or the item definition is unknown -- the gift
/// is only deleted once delivery is guaranteed to proceed, and the delete's
/// rows-affected doubles as the double-claim guard. Pushes the created item
/// into `active_items`.
pub(crate) fn handle_stored_item_get(
    db_conn: &mut db::Db,
    character_id: u32,
    account_id: u32,
    active_items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<Vec<u8>>> {
    if payload.len() < 3 {
        return Ok(None);
    }
    let npc_slot = payload[2];
    if npc_slot >= GIFT_BOX_CAPACITY {
        return Ok(None);
    }
    let db_slot = npc_slot + 1;

    let Some(entry) = db::get_gift_box(db_conn, account_id)?
        .into_iter()
        .find(|entry| entry.slot == db_slot)
    else {
        logger.line(format!(
            "Rapture World rejected gift claim: empty gift slot npc_slot={npc_slot}"
        ));
        return Ok(None);
    };
    let Some(definition) = db::get_item_definition(db_conn, entry.item_type, entry.type_id)? else {
        logger.line(format!(
            "Rapture World rejected gift claim: unknown item type={} type_id={}",
            entry.item_type, entry.type_id
        ));
        return Ok(None);
    };
    let Some((bag, slot)) = find_free_inventory_slots(active_items).into_iter().next() else {
        logger.line("Rapture World rejected gift claim: inventory full");
        return Ok(None);
    };

    // Delete first (the rows-affected check makes a concurrent double-claim
    // impossible), then deliver. A gift taken out can never be put back.
    if !db::delete_gift(db_conn, account_id, db_slot)? {
        logger.line(format!(
            "Rapture World rejected gift claim: slot {db_slot} already taken"
        ));
        return Ok(None);
    }
    let stack_count = entry.count.min(effective_stack_limit(&definition).max(1));
    let created = db::create_character_item(db_conn, character_id, &definition, bag, slot, stack_count)?;
    logger.line(format!(
        "Rapture World gift claim success npc_slot={npc_slot} type={} type_id={} count={stack_count} -> bag={bag} slot={slot}",
        entry.item_type, entry.type_id
    ));
    active_items.push(created);

    Ok(Some(build_stored_item_get(npc_slot, bag, slot)?))
}

/// Handles 0x2602 `TPC_POINT_BUY`: looks up `product_code` in
/// `PS_GameDefs.dbo.ProductList`, checks the account has enough points and
/// the character has enough free inventory slots, then deducts points and
/// grants every item in the product's bundle (up to 24).
pub(crate) fn handle_point_buy(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    account_id: u32,
    current_points: u32,
    active_items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<PointBuyOutcome> {
    let product_code = if payload.len() > 2 {
        read_fixed_ascii(&payload[2..])
    } else {
        String::new()
    };

    let Some(product) = db::get_product_by_code(db_conn, &product_code)? else {
        logger.line(format!(
            "Rapture World rejected point buy: unknown product_code='{product_code}'"
        ));
        let response = build_point_buy_result(
            RESULT_PRODUCT_NOT_FOUND,
            current_points,
            &product_code,
            0,
            &[],
        )?;
        return Ok(PointBuyOutcome {
            response,
            add_item_packets: Vec::new(),
            new_points: current_points,
        });
    };

    if current_points < product.buy_cost {
        logger.line(format!(
            "Rapture World rejected point buy: not enough points product_code='{product_code}' cost={} balance={current_points}",
            product.buy_cost
        ));
        let response = build_point_buy_result(
            RESULT_NOT_ENOUGH_POINTS,
            current_points,
            &product_code,
            product.buy_cost,
            &[],
        )?;
        return Ok(PointBuyOutcome {
            response,
            add_item_packets: Vec::new(),
            new_points: current_points,
        });
    }

    // Pre-check available slots before touching anything -- every granted
    // item claims a fresh slot (see module doc), so this must match the
    // granting loop below exactly.
    let mut required_slots = 0usize;
    for &(item_type, type_id, count) in &product.items {
        let Some(definition) = db::get_item_definition(db_conn, item_type, type_id)? else {
            continue;
        };
        let max_stack = effective_stack_limit(&definition).max(1);
        required_slots += (u32::from(count)).div_ceil(u32::from(max_stack)) as usize;
    }
    if required_slots > find_free_inventory_slots(active_items).len() {
        logger.line(format!(
            "Rapture World rejected point buy: not enough slots product_code='{product_code}' needed={required_slots}"
        ));
        let response = build_point_buy_result(
            RESULT_NOT_ENOUGH_SLOTS,
            current_points,
            &product_code,
            product.buy_cost,
            &[],
        )?;
        return Ok(PointBuyOutcome {
            response,
            add_item_packets: Vec::new(),
            new_points: current_points,
        });
    }

    let new_points = current_points - product.buy_cost;
    db::set_account_points(db_conn, account_id, new_points)?;

    let mut add_item_packets = Vec::new();
    let mut granted = Vec::new();
    for &(item_type, type_id, count) in &product.items {
        let Some(definition) = db::get_item_definition(db_conn, item_type, type_id)? else {
            logger.line(format!(
                "Rapture World point buy skipped unknown item type={item_type} type_id={type_id} in product_code='{product_code}'"
            ));
            continue;
        };
        let max_stack = effective_stack_limit(&definition).max(1);
        let mut remaining = count;
        while remaining > 0 {
            let Some((bag, slot)) = find_free_inventory_slots(active_items).into_iter().next()
            else {
                break;
            };
            let stack_count = remaining.min(max_stack);
            let created = db::create_character_item(
                db_conn,
                character.id,
                &definition,
                bag,
                slot,
                stack_count,
            )?;
            add_item_packets.push(crate::inventory::build_add_item(&created)?);
            granted.push((bag, slot, item_type, type_id, stack_count));
            active_items.push(created);
            remaining -= stack_count;
        }
    }

    logger.line(format!(
        "Rapture World point buy success product_code='{product_code}' cost={} items={} remaining_points={new_points}",
        product.buy_cost,
        granted.len()
    ));

    let response = build_point_buy_result(
        RESULT_SUCCESS,
        new_points,
        &product_code,
        product.buy_cost,
        &granted,
    )?;
    Ok(PointBuyOutcome {
        response,
        add_item_packets,
        new_points,
    })
}
