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

pub(crate) struct PointBuyOutcome {
    pub(crate) response: Vec<u8>,
    pub(crate) add_item_packets: Vec<Vec<u8>>,
    pub(crate) new_points: u32,
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
